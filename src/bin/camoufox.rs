//! Camoufox CLI matching `python/src/camoufox/__main__.py` and `typescript/src/cli.ts`.

use std::fs;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::{Map, Value};

use camoufox_rs::browser_pin::{effective_pin, load_pin};
use camoufox_rs::fpgen_model::ensure_fpgen_model;
use camoufox_rs::geolocation::{
    download_mmdb, geoip_dir, get_mmdb_path, load_geoip_config, load_geoip_repos,
    save_geoip_config, warn_if_deprecated,
};
use camoufox_rs::gpudisplay::{select_gpu, setup_script, xorg_config};
use camoufox_rs::multiversion::{
    browsers_dir, config_file, find_install, get_default_channel, latest_per_build,
    list_installed, load_config, load_repo_cache, print_tree, remove_incompatible_data,
    remove_version, repo_cache_file, save_config, save_repo_cache, InstalledVersion,
};
use camoufox_rs::paths::install_dir;
use camoufox_rs::pkgman::{
    incompatible_message, installed_verstr, interface_supported, list_available_versions,
    tagged_versions, upgrade_message, AvailableVersion, CamoufoxFetcher, RepoConfig, Version,
};
use camoufox_rs::server::launch_server;
use camoufox_rs::utils::{CamoufoxOptions, HeadlessMode};
use camoufox_rs::version::VERSION;

#[derive(Parser)]
#[command(name = "camoufox", version = VERSION, about = "CLI package manager for Camoufox")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Sync available versions from remote repositories
    Sync {
        #[arg(long)]
        spoof_os: Option<String>,
        #[arg(long)]
        spoof_arch: Option<String>,
    },
    /// Install the active version, or a specific version
    Fetch {
        version: Option<String>,
    },
    /// Set the active Camoufox version or GeoIP source
    Set {
        specifier: Option<String>,
        #[arg(long)]
        geoip: bool,
        #[arg(long)]
        release: bool,
    },
    /// List installed or all available versions
    List {
        #[arg(default_value = "installed")]
        mode: String,
        #[arg(long)]
        path: bool,
    },
    /// Remove downloaded Camoufox data or a specific version
    Remove {
        version_path: Option<String>,
        #[arg(short, long)]
        yes: bool,
    },
    /// Open Playwright Inspector against Camoufox
    Test {
        url: Option<String>,
        #[arg(long)]
        executable_path: Option<PathBuf>,
    },
    /// Launch a Playwright WebSocket server
    Server {
        #[arg(long)]
        port: Option<u16>,
    },
    /// Print the Xorg config headless="virtual-gpu" needs for GPU
    GpuXorgConfig {
        gpu: Option<String>,
    },
    /// Print a root script that sets this machine up for headless="virtual-gpu" on GPU
    GpuSetup {
        gpu: Option<String>,
    },
    /// Display version, package, browser, and storage info
    Version,
    /// Print the current active version
    Active,
    /// Print the install directory path
    Path,
}

async fn do_sync(spoof_os: Option<&str>, spoof_arch: Option<&str>) {
    println!("Syncing repositories...");
    let mut cache = Map::new();
    let mut repos_out = Vec::new();
    let mut incompatible_out = Vec::new();

    for rc in RepoConfig::load_repos() {
        print!("  {}...", rc.name);
        match list_available_versions(Some(rc.clone()), true, spoof_os, spoof_arch, true).await {
            Ok(listed) => {
                let mut valid_versions = Vec::new();
                for v in listed {
                    if interface_supported(v.interface) {
                        valid_versions.push(serde_json::json!({
                            "version": v.version.version,
                            "build": v.version.build,
                            "url": v.url,
                            "is_prerelease": v.is_prerelease,
                            "asset_id": v.asset_id,
                            "asset_size": v.asset_size,
                            "asset_updated_at": v.asset_updated_at,
                            "sha256": v.sha256,
                            "created_at": v.asset_created_at,
                            "interface": v.interface,
                        }));
                    } else {
                        incompatible_out.push(serde_json::json!({
                            "repo": rc.name,
                            "version": v.version.version,
                            "build": v.version.build,
                            "interface": v.interface,
                        }));
                    }
                }
                println!(" {} versions", valid_versions.len());
                repos_out.push(serde_json::json!({
                    "name": rc.name,
                    "repo": rc.primary_repo(),
                    "versions": valid_versions,
                }));
            }
            Err(err) => {
                println!(" Error: {err}");
            }
        }
    }

    cache.insert("repos".into(), Value::Array(repos_out.clone()));
    cache.insert("incompatible".into(), Value::Array(incompatible_out.clone()));
    save_repo_cache(&cache);

    if let Some(newest) = incompatible_out.iter().max_by(|a, b| {
        let va = Version::new(
            a.get("build").and_then(Value::as_str).unwrap_or(""),
            a.get("version").and_then(Value::as_str).map(str::to_owned),
        );
        let vb = Version::new(
            b.get("build").and_then(Value::as_str).unwrap_or(""),
            b.get("version").and_then(Value::as_str).map(str::to_owned),
        );
        va.cmp(&vb)
    }) {
        let ver = Version::new(
            newest.get("build").and_then(Value::as_str).unwrap_or(""),
            newest.get("version").and_then(Value::as_str).map(str::to_owned),
        );
        let iface = newest
            .get("interface")
            .and_then(Value::as_u64)
            .unwrap_or(1) as u32;
        println!("{}", upgrade_message(&ver, iface));
    }

    let total: usize = repos_out
        .iter()
        .filter_map(|r| r.get("versions").and_then(Value::as_array).map(Vec::len))
        .sum();
    println!("\nSynced {total} versions from {} repos.", repos_out.len());
}

fn find_installed_cli(specifier: &str) -> Option<InstalledVersion> {
    let spec = specifier.to_ascii_lowercase();
    let installed = list_installed();
    let parts: Vec<&str> = spec.split('/').collect();
    for v in &installed {
        if v.channel_path().to_ascii_lowercase() == spec
            || v.relative_path().to_ascii_lowercase() == spec
            || v.version.build.to_ascii_lowercase() == spec
            || v.version.full_string().to_ascii_lowercase() == spec
        {
            return Some(v.clone());
        }
        if parts.len() == 2 && v.repo_name == parts[0] && v.version.full_string().to_ascii_lowercase() == parts[1]
        {
            return Some(v.clone());
        }
    }
    if parts.len() == 2 && (parts[1] == "stable" || parts[1] == "prerelease") {
        let is_pre = parts[1] == "prerelease";
        for v in installed {
            if v.repo_name == parts[0] && v.is_prerelease == is_pre {
                return Some(v);
            }
        }
    }
    None
}

async fn cmd_fetch(version: Option<String>) {
    remove_incompatible_data();
    let config = load_config();
    let pin = if version.is_none() {
        effective_pin(&config)
    } else {
        None
    };

    let (repo_name, spec, tag): (Option<String>, Option<String>, Option<String>) =
        if let Some(ref v) = version {
            let parts: Vec<String> = v.to_ascii_lowercase().split('/').map(str::to_owned).collect();
            let (rn, sp) = match parts.len() {
                1 => (RepoConfig::get_default_name().to_ascii_lowercase(), parts[0].clone()),
                2 | 3 => (parts[0].clone(), parts.last().unwrap().clone()),
                _ => {
                    eprintln!("Format: version-build, repo/version-build, or repo/channel/version-build");
                    std::process::exit(1);
                }
            };
            let cleaned = sp.trim_start_matches('v').to_owned();
            let t = format!("v{cleaned}");
            (Some(rn), Some(cleaned), Some(t))
        } else if let Some(ref p) = pin {
            (Some(p.repo_name.clone()), Some(p.spec()), Some(p.tag()))
        } else {
            (None, None, None)
        };

    let mut chosen: Option<(RepoConfig, AvailableVersion)> = None;
    if let (Some(ref rn), Some(ref tg), Some(ref sp)) = (&repo_name, &tag, &spec) {
        if let Some(rc) = RepoConfig::find_by_name(rn) {
            if let Ok(versions) = tagged_versions(&rc, tg).await {
                if let Some(m) = versions.into_iter().find(|v| &v.version.full_string() == sp) {
                    chosen = Some((rc, m));
                }
            }
        }
    }

    if chosen.is_none() {
        do_sync(None, None).await;
        let cache = load_repo_cache();
        let repos = cache
            .get("repos")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let target_repo = if let Some(ref rn) = repo_name {
            repos.into_iter().find(|r| {
                r.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.eq_ignore_ascii_case(rn))
            })
        } else {
            let ch = config
                .get("channel")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(get_default_channel);
            let rn = ch.split('/').next().unwrap_or("official");
            repos.into_iter().find(|r| {
                r.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.eq_ignore_ascii_case(rn))
            })
        };

        if let Some(rd) = target_repo {
            let versions = rd
                .get("versions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let entry = if let Some(ref sp) = spec {
                latest_per_build(&versions).into_iter().find(|v| {
                    let full = format!(
                        "{}-{}",
                        v.get("version").and_then(Value::as_str).unwrap_or(""),
                        v.get("build").and_then(Value::as_str).unwrap_or("")
                    );
                    &full == sp
                })
            } else {
                let ch = config
                    .get("channel")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(get_default_channel);
                let is_pre = ch.split('/').nth(1) == Some("prerelease");
                latest_per_build(&versions).into_iter().find(|v| {
                    v.get("is_prerelease")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                        == is_pre
                })
            };
            if let Some(vd) = entry {
                let rn_str = rd.get("name").and_then(Value::as_str).unwrap_or("Official");
                if let Some(rc) = RepoConfig::find_by_name(rn_str) {
                    let av = AvailableVersion {
                        version: Version::new(
                            vd.get("build").and_then(Value::as_str).unwrap_or(""),
                            vd.get("version").and_then(Value::as_str).map(str::to_owned),
                        ),
                        url: vd.get("url").and_then(Value::as_str).unwrap_or("").to_owned(),
                        is_prerelease: vd
                            .get("is_prerelease")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        asset_id: vd.get("asset_id").and_then(Value::as_u64),
                        asset_size: vd.get("asset_size").and_then(Value::as_u64),
                        asset_updated_at: vd
                            .get("asset_updated_at")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        sha256: vd.get("sha256").and_then(Value::as_str).map(str::to_owned),
                        asset_created_at: vd
                            .get("created_at")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        interface: vd.get("interface").and_then(Value::as_u64).unwrap_or(1) as u32,
                    };
                    chosen = Some((rc, av));
                }
            }
        }
    }

    let Some((rc, av)) = chosen else {
        if let Some(ref sp) = spec {
            let cache = load_repo_cache();
            if let Some(incomp) = cache.get("incompatible").and_then(Value::as_array) {
                for v in incomp {
                    let full = format!(
                        "{}-{}",
                        v.get("version").and_then(Value::as_str).unwrap_or(""),
                        v.get("build").and_then(Value::as_str).unwrap_or("")
                    );
                    if &full == sp {
                        eprintln!(
                            "{}",
                            incompatible_message(
                                &Version::new(
                                    v.get("build").and_then(Value::as_str).unwrap_or(""),
                                    v.get("version").and_then(Value::as_str).map(str::to_owned)
                                ),
                                v.get("interface").and_then(Value::as_u64).unwrap_or(1) as u32
                            )
                        );
                        std::process::exit(1);
                    }
                }
            }
        }
        eprintln!("Version not found. Run 'camoufox sync'.");
        std::process::exit(1);
    };

    let current = installed_verstr(None).ok();
    let target_ver = av.version.full_string();
    if current.as_deref() == Some(target_ver.as_str()) {
        println!("Camoufox binaries up to date!\nCurrent version: v{target_ver}");
    } else {
        match CamoufoxFetcher::new(Some(rc), Some(av)) {
            Ok(mut fetcher) => {
                if let Err(err) = fetcher.install(false).await {
                    eprintln!("Error: {err}");
                    std::process::exit(1);
                }
            }
            Err(err) => {
                eprintln!("Error: {err}");
                std::process::exit(1);
            }
        }
    }
    if let Err(err) = download_mmdb(None, false).await {
        eprintln!("GeoIP download warning: {err}");
    }
    if let Err(err) = ensure_fpgen_model(false).await {
        eprintln!("fpgen model download warning: {err}");
    }
}

fn cmd_set(specifier: Option<String>, geoip: bool, release: bool) {
    if geoip {
        if let Ok((repos, _)) = load_geoip_repos() {
            if let Some(spec) = specifier {
                if let Some(repo) = repos
                    .into_iter()
                    .find(|r| r.name.eq_ignore_ascii_case(&spec))
                {
                    let _ = save_geoip_config(&repo, true);
                    println!("GeoIP source: {}", repo.name);
                    warn_if_deprecated(&repo);
                } else {
                    eprintln!("GeoIP source '{spec}' not found.");
                }
            } else {
                for r in &repos {
                    println!("  - {}", r.name);
                }
            }
        }
        return;
    }

    if release {
        let Some(pin) = load_pin() else {
            println!("This camoufox is a development copy; it pairs with no particular browser.");
            return;
        };
        let mut config = load_config();
        for k in ["channel", "pinned", "pinned_sha", "active_version"] {
            config.remove(k);
        }
        save_config(&config);
        println!(
            "Using the paired browser: {} {} ({})",
            pin.repo_name,
            pin.spec(),
            pin.tag()
        );
        return;
    }

    let Some(spec) = specifier else {
        println!("Pass a specifier (e.g. `camoufox set official/stable` or `camoufox set --release`).");
        return;
    };
    let parts: Vec<String> = spec.to_ascii_lowercase().split('/').map(str::to_owned).collect();
    if parts.len() == 2 {
        let repo_name = &parts[0];
        let ctype = &parts[1];
        if ctype != "stable" && ctype != "prerelease" {
            eprintln!("Unknown channel type '{ctype}'. Use 'stable' or 'prerelease'.");
            return;
        }
        let mut config = load_config();
        config.insert("channel".into(), Value::String(format!("{repo_name}/{ctype}")));
        config.remove("pinned");
        config.remove("pinned_sha");
        config.remove("active_version");
        save_config(&config);
        println!("Channel: {repo_name}/{ctype}");
        return;
    }

    let (repo_name, raw_ver) = if parts.len() == 1 {
        (RepoConfig::get_default_name().to_ascii_lowercase(), parts[0].clone())
    } else if parts.len() == 3 {
        (parts[0].clone(), parts[2].clone())
    } else {
        eprintln!("Invalid specifier '{spec}'.");
        return;
    };
    let clean_ver = raw_ver.trim_start_matches('v').to_owned();
    let cache = load_repo_cache();
    let Some(repo_data) = cache
        .get("repos")
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter().find(|r| {
                r.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.eq_ignore_ascii_case(&repo_name))
            })
        })
    else {
        eprintln!("Repo '{repo_name}' not in cache. Run 'camoufox sync'.");
        return;
    };
    let versions = repo_data
        .get("versions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let found = latest_per_build(&versions).into_iter().find(|v| {
        let vb = format!(
            "{}-{}",
            v.get("version").and_then(Value::as_str).unwrap_or(""),
            v.get("build").and_then(Value::as_str).unwrap_or("")
        );
        vb == clean_ver
    });
    let Some(vd) = found else {
        eprintln!("Version '{clean_ver}' not found in {repo_name}.");
        return;
    };
    let ctype = if vd
        .get("is_prerelease")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "prerelease"
    } else {
        "stable"
    };
    let vb = format!(
        "{}-{}",
        vd.get("version").and_then(Value::as_str).unwrap_or(""),
        vd.get("build").and_then(Value::as_str).unwrap_or("")
    );
    let sha = vd.get("sha256").and_then(Value::as_str);
    let installed_list = list_installed();
    let inst = find_install(&vb, sha, &installed_list, 1);

    let mut config = load_config();
    config.insert("channel".into(), Value::String(format!("{repo_name}/{ctype}")));
    config.insert("pinned".into(), Value::String(vb.clone()));
    if let Some(s) = sha {
        config.insert("pinned_sha".into(), Value::String(s.to_owned()));
    } else {
        config.remove("pinned_sha");
    }
    if let Some(i) = inst {
        config.insert("active_version".into(), Value::String(i.relative_path()));
        println!("Pinned: {repo_name}/{ctype}/{vb} (installed)");
    } else {
        config.remove("active_version");
        println!("Pinned: {repo_name}/{ctype}/{vb}\nRun 'camoufox fetch' to install.");
    }
    save_config(&config);
}

fn cmd_list(mode: &str, show_paths: bool) {
    if mode == "all" {
        let cache = load_repo_cache();
        let Some(repos) = cache.get("repos").and_then(Value::as_array) else {
            println!("No repo cache found. Run 'camoufox sync' first.");
            return;
        };
        let installed = list_installed();
        println!("Available versions:\n");
        for rd in repos {
            let rname = rd.get("name").and_then(Value::as_str).unwrap_or("");
            println!("{rname}/");
            let versions = rd
                .get("versions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let dedup = latest_per_build(&versions);
            for (i, v) in dedup.iter().enumerate() {
                let build = v.get("build").and_then(Value::as_str).unwrap_or("");
                let ver = v.get("version").and_then(Value::as_str).unwrap_or("");
                let is_last = i + 1 == dedup.len();
                let prefix = if is_last { "└── " } else { "├── " };
                let kind = if v
                    .get("is_prerelease")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    "(prerelease)"
                } else {
                    "(stable)"
                };
                let inst = installed.iter().find(|x| x.version.build == build);
                let status = match inst {
                    Some(x) if x.is_active => " (installed, active)",
                    Some(_) => " (installed)",
                    None => "",
                };
                let path_str = if show_paths {
                    inst.map(|x| format!(" -> {}", x.path.display()))
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                println!("    {prefix}v{ver}-{build} {kind}{status}{path_str}");
            }
            println!();
        }
        return;
    }

    print_tree(true, show_paths);
    println!();
    let gdir = geoip_dir();
    if show_paths && gdir.exists() {
        println!("geoip/ -> {}", gdir.display());
    } else {
        println!("geoip/");
    }
    if gdir.exists() {
        if let Ok(cfg) = load_geoip_config() {
            if let Ok(mmdb) = get_mmdb_path("ipv4", Some(&cfg)) {
                if mmdb.exists() {
                    println!(
                        "    └── {} ({})",
                        mmdb.file_name().unwrap_or_default().to_string_lossy(),
                        cfg.name
                    );
                } else {
                    println!("    └── Not downloaded");
                }
            }
        }
    } else {
        println!("    └── Not configured");
    }
}

fn cmd_version() {
    println!("Rust Crate");
    println!("  camoufox-rs      v{VERSION}");
    println!("Browser");
    let config = load_config();
    let pinned = config.get("pinned").and_then(Value::as_str);
    let channel = config
        .get("channel")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(get_default_channel);
    if let Some(p) = pinned {
        println!("  Active           {}/{p}", channel.to_ascii_lowercase());
    } else {
        println!("  Active           {}", channel.to_ascii_lowercase());
    }
    let installed = list_installed();
    if let Some(av) = installed.iter().find(|v| v.is_active) {
        println!("  Current browser  v{}", av.version.full_string());
        if let Some(ref s) = av.sha256 {
            println!("  SHA256           {}", &s[..s.len().min(12)]);
        }
        println!("  Installed        Yes");
    } else {
        println!("  Current browser  Not installed");
        println!("  Installed        No");
    }
    println!("Storage");
    println!("  Install path     {}", install_dir().display());
    println!("  Browsers dir     {}", browsers_dir().display());
    println!("  Config file      {}", config_file().display());
    println!("  Repo cache       {}", repo_cache_file().display());
}

fn cmd_active() {
    let config = load_config();
    let installed = list_installed();
    let label = |v: &InstalledVersion| -> String {
        let sha8: String = v.sha256.as_deref().unwrap_or("").chars().take(8).collect();
        if sha8.is_empty() {
            v.channel_path()
        } else {
            format!("{} ({sha8})", v.channel_path())
        }
    };
    if let Some(pin) = effective_pin(&config) {
        if let Some(t) = find_installed_cli(&format!("{}/{}", pin.repo_name, pin.spec())) {
            println!("{} (paired with this release)", label(&t));
        } else {
            println!(
                "{}/{} (paired with this release) (not fetched)",
                pin.repo_name,
                pin.spec()
            );
        }
        return;
    }
    if let Some(av) = installed.iter().find(|v| v.is_active) {
        println!("{}", label(av));
    } else {
        let channel = config
            .get("channel")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(get_default_channel);
        println!("{} (not fetched)", channel.to_ascii_lowercase());
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Sync {
            spoof_os,
            spoof_arch,
        } => {
            let os_arg = spoof_os.filter(|s| s != "auto");
            let arch_arg = spoof_arch.filter(|s| s != "auto");
            do_sync(os_arg.as_deref(), arch_arg.as_deref()).await;
        }
        Commands::Fetch { version } => {
            cmd_fetch(version).await;
        }
        Commands::Set {
            specifier,
            geoip,
            release,
        } => cmd_set(specifier, geoip, release),
        Commands::List { mode, path } => cmd_list(&mode, path),
        Commands::Remove { version_path, yes } => {
            let _ = yes;
            if let Some(spec) = version_path {
                if let Some(t) = find_installed_cli(&spec) {
                    if remove_version(&t.path) {
                        println!("Removed {}", t.channel_path());
                    }
                } else {
                    eprintln!("Version '{spec}' not found.");
                }
            } else if install_dir().exists() {
                let _ = fs::remove_dir_all(install_dir());
                println!("Removed camoufox data directory.");
            } else {
                println!("Nothing to remove.");
            }
        }
        Commands::Test {
            url,
            executable_path,
        } => {
            let mut opts = CamoufoxOptions {
                headless: HeadlessMode::Bool(false),
                executable_path,
                ..Default::default()
            };
            opts.config.insert("showcursor".into(), Value::Bool(false));
            match camoufox_rs::camoufox(opts).await {
                Ok(mut browser) => {
                    if let Ok(page) = browser.new_page().await {
                        if let Some(u) = url {
                            let _ = page.goto(&u, None).await;
                        }
                        let _ = page.pause().await;
                    }
                    let _ = browser.close().await;
                }
                Err(err) => {
                    eprintln!("Error launching test browser: {err}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Server { port } => {
            let opts = CamoufoxOptions {
                port,
                ..Default::default()
            };
            match launch_server(opts).await {
                Ok(mut srv) => {
                    let _ = tokio::signal::ctrl_c().await;
                    let _ = srv.close().await;
                }
                Err(err) => {
                    eprintln!("Error: {err}");
                    std::process::exit(1);
                }
            }
        }
        Commands::GpuXorgConfig { gpu } => match select_gpu(gpu.as_deref(), None) {
            Ok(g) => print!("{}", xorg_config(&g, None)),
            Err(err) => {
                eprintln!("Error: {err}");
                std::process::exit(1);
            }
        },
        Commands::GpuSetup { gpu } => match select_gpu(gpu.as_deref(), None) {
            Ok(g) => print!("{}", setup_script(&g)),
            Err(err) => {
                eprintln!("Error: {err}");
                std::process::exit(1);
            }
        },
        Commands::Version => cmd_version(),
        Commands::Active => cmd_active(),
        Commands::Path => println!("{}", install_dir().display()),
    }
}
