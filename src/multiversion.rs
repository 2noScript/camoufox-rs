//! Multi-version browser management under `INSTALL_DIR`, matching `multiversion.ts` / `multiversion.py`.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::browser_pin::{effective_pin, pin_matches};
use crate::exceptions::Result;
use crate::paths::{install_dir, load_data_value, OS_NAME};
use crate::pkgman::{
    format_asset_date, make_executable, unzip, verify_sha256_hex, webdl_to_file,
    AvailableVersion, CamoufoxFetcher, Version,
};
use crate::pycompat::compare_py_str;

pub fn browsers_dir() -> PathBuf {
    install_dir().join("browsers")
}

pub fn config_file() -> PathBuf {
    install_dir().join("config.json")
}

pub fn repo_cache_file() -> PathBuf {
    install_dir().join("repo_cache.json")
}

pub fn compat_flag() -> PathBuf {
    install_dir().join(".compat_v2")
}

pub fn get_repo_name(github_repo: &str) -> String {
    load_data_value("repos.yml")
        .ok()
        .and_then(|v| v.get("browsers").and_then(Value::as_array).cloned())
        .and_then(|arr| {
            arr.into_iter().find_map(|b| {
                let repo_str = b.get("repo").and_then(Value::as_str)?;
                let name = b.get("name").and_then(Value::as_str)?;
                let matches = repo_str
                    .split(',')
                    .any(|part| part.trim() == github_repo.trim());
                if matches {
                    Some(name.to_ascii_lowercase())
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| {
            github_repo
                .split('/')
                .next()
                .unwrap_or(github_repo)
                .to_ascii_lowercase()
        })
}

pub fn get_default_channel() -> String {
    let default_repo = load_data_value("repos.yml")
        .ok()
        .and_then(|v| v.get("default")?.get("browser")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| "official".to_owned())
        .to_ascii_lowercase();
    format!("{default_repo}/stable")
}

pub fn version_folder_name(version: &str, build: &str, sha8: &str) -> String {
    if sha8.is_empty() {
        format!("{version}-{build}")
    } else {
        format!("{version}-{build}-{sha8}")
    }
}

/// Keeps the first (newest) release per distinct `build` string.
pub fn latest_per_build(versions: &[Value]) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for entry in versions {
        if let Some(build) = entry.get("build").and_then(Value::as_str) {
            if seen.insert(build.to_owned()) {
                out.push(entry.clone());
            }
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledVersion {
    pub repo_name: String,
    pub version: Version,
    pub path: PathBuf,
    pub is_active: bool,
    pub is_prerelease: bool,
    pub asset_id: Option<u64>,
    pub asset_size: Option<u64>,
    pub asset_updated_at: Option<String>,
    pub sha256: Option<String>,
    pub created_at: Option<String>,
}

impl InstalledVersion {
    pub fn channel_type(&self) -> &'static str {
        if self.is_prerelease {
            "prerelease"
        } else {
            "stable"
        }
    }

    pub fn channel_path(&self) -> String {
        format!(
            "{}/{}/{}",
            self.repo_name,
            self.channel_type(),
            self.version.full_string()
        )
    }

    pub fn relative_path(&self) -> String {
        let folder = self
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        format!("browsers/{}/{}", self.repo_name, folder)
    }

    pub fn get_changes(&self, available: &AvailableVersion) -> Vec<String> {
        let mut changes = Vec::new();
        if let (Some(a), Some(b)) = (&self.sha256, &available.sha256) {
            if a != b {
                changes.push("new build".to_owned());
            }
        }
        if let Some(a) = &available.asset_created_at {
            let new_date = format_asset_date(Some(a));
            let old_date = format_asset_date(self.created_at.as_deref());
            if !new_date.is_empty() && new_date != old_date {
                changes.push(format!("released {new_date}"));
            }
        }
        changes
    }
}

pub fn load_config() -> Map<String, Value> {
    let path = config_file();
    if let Ok(raw) = fs::read_to_string(path) {
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&raw) {
            return map;
        }
    }
    Map::new()
}

pub fn save_config(config: &Map<String, Value>) {
    let dir = install_dir();
    let _ = fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_string_pretty(config) {
        let _ = fs::write(config_file(), json);
    }
}

pub fn load_repo_cache() -> Map<String, Value> {
    let path = repo_cache_file();
    if let Ok(raw) = fs::read_to_string(path) {
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&raw) {
            return map;
        }
    }
    Map::new()
}

pub fn save_repo_cache(cache: &Map<String, Value>) {
    let dir = install_dir();
    let _ = fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_string_pretty(cache) {
        let _ = fs::write(repo_cache_file(), json);
    }
}

pub fn find_install<'a>(
    version_build: &str,
    sha256: Option<&str>,
    installed: &'a [InstalledVersion],
    count: usize,
) -> Option<&'a InstalledVersion> {
    let mut by_folder: BTreeMap<String, &InstalledVersion> = BTreeMap::new();
    for item in installed {
        if let Some(name) = item.path.file_name().and_then(|s| s.to_str()) {
            by_folder.insert(name.to_owned(), item);
        }
    }
    if let Some(sha8) = sha256.filter(|s| s.len() >= 8).map(|s| &s[..8]) {
        if let Some(found) = by_folder.get(&format!("{version_build}-{sha8}")) {
            return Some(found);
        }
    }
    let legacy = by_folder.get(version_build)?;
    if let Some(expected) = sha256 {
        return if legacy.sha256.as_deref() == Some(expected) {
            Some(legacy)
        } else {
            None
        };
    }
    if count <= 1 {
        Some(legacy)
    } else {
        None
    }
}

pub fn list_installed() -> Vec<InstalledVersion> {
    let mut installed = Vec::new();
    let active = load_config()
        .get("active_version")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let bdir = browsers_dir();
    let Ok(repo_entries) = fs::read_dir(&bdir) else {
        return installed;
    };

    for repo_entry in repo_entries.flatten() {
        let repo_path = repo_entry.path();
        if !repo_path.is_dir() {
            continue;
        }
        let repo_name = repo_entry.file_name().to_string_lossy().into_owned();
        if repo_name.starts_with('.') {
            continue;
        }
        let Ok(ver_entries) = fs::read_dir(&repo_path) else {
            continue;
        };
        for ver_entry in ver_entries.flatten() {
            let ver_path = ver_entry.path();
            let ver_json_path = ver_path.join("version.json");
            if !ver_path.is_dir() || !ver_json_path.exists() {
                continue;
            }
            let Ok(ver) = Version::from_path(&ver_path) else {
                continue;
            };
            let Ok(raw) = fs::read_to_string(&ver_json_path) else {
                continue;
            };
            let Ok(data) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            let folder_name = ver_entry.file_name().to_string_lossy().into_owned();
            let rel_path = format!("browsers/{repo_name}/{folder_name}");
            installed.push(InstalledVersion {
                repo_name: repo_name.clone(),
                version: ver,
                path: ver_path,
                is_active: active.as_deref() == Some(rel_path.as_str()),
                is_prerelease: data
                    .get("prerelease")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                asset_id: data.get("asset_id").and_then(Value::as_u64),
                asset_size: data.get("asset_size").and_then(Value::as_u64),
                asset_updated_at: data
                    .get("asset_updated_at")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                sha256: data.get("sha256").and_then(Value::as_str).map(str::to_owned),
                created_at: data
                    .get("created_at")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            });
        }
    }

    installed.sort_by(|a, b| {
        compare_py_str(&b.repo_name, &a.repo_name).then_with(|| b.version.cmp(&a.version))
    });
    installed
}

pub fn get_active_path() -> Option<PathBuf> {
    let mut config = load_config();
    let active = config
        .get("active_version")
        .and_then(Value::as_str)
        .map(str::to_owned);

    if let Some(pin) = effective_pin(&config) {
        for inst in list_installed() {
            if pin_matches(
                &pin,
                &inst.repo_name,
                inst.version.version.as_deref().unwrap_or(""),
                &inst.version.build,
            ) {
                let rel = inst.relative_path();
                if active.as_deref() != Some(rel.as_str()) {
                    config.insert("active_version".into(), Value::String(rel));
                    save_config(&config);
                }
                return Some(inst.path);
            }
        }
        return None;
    }

    if let Some(ref rel) = active {
        let active_path = install_dir().join(rel);
        if active_path.exists() && active_path.join("version.json").exists() {
            return Some(active_path);
        }
    }

    let has_channel = config.get("channel").is_some_and(|v| !v.is_null() && v.as_str() != Some(""));
    let has_pinned = config.get("pinned").is_some_and(|v| !v.is_null() && v.as_str() != Some(""));
    if !has_channel && !has_pinned {
        let installed = list_installed();
        if let Some(first) = installed.first() {
            config.insert("active_version".into(), Value::String(first.relative_path()));
            save_config(&config);
            return Some(first.path.clone());
        }
    }
    None
}

pub fn set_active(relative_path: &str) {
    let mut config = load_config();
    config.insert("active_version".into(), Value::String(relative_path.to_owned()));
    save_config(&config);
}

pub fn find_installed_version(specifier: &str) -> Option<PathBuf> {
    let installed = list_installed();
    if installed.is_empty() {
        return None;
    }
    let lower = specifier.to_ascii_lowercase();
    for v in installed {
        let rel = v.relative_path();
        if rel == specifier || rel == format!("browsers/{specifier}") {
            return Some(v.path);
        }
        let suffix = format!("browsers/{}/{}", v.repo_name, v.version.full_string());
        let repo_build = format!("{}/{}", v.repo_name, v.version.build).to_ascii_lowercase();
        if suffix.ends_with(specifier)
            || repo_build == lower
            || v.version.build.to_ascii_lowercase() == lower
            || v.version.full_string().to_ascii_lowercase() == lower
            || v.version
                .version
                .as_deref()
                .is_some_and(|ver| ver.to_ascii_lowercase() == lower)
        {
            return Some(v.path);
        }
    }
    None
}

/// Stage emitted during [`install_versioned_with_progress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStage {
    Downloading,
    Verifying,
    Extracting,
    Completed,
}

impl InstallStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Downloading => "downloading",
            Self::Verifying => "verifying",
            Self::Extracting => "extracting",
            Self::Completed => "completed",
        }
    }
}

/// Progress update emitted during [`install_versioned_with_progress`].
#[derive(Debug, Clone)]
pub struct InstallProgress {
    pub stage: InstallStage,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub percentage: f64,
    pub speed_bytes_per_sec: f64,
    pub eta_seconds: Option<u64>,
}

pub async fn install_versioned(fetcher: &mut CamoufoxFetcher, replace: bool) -> Result<bool> {
    install_versioned_with_progress(fetcher, replace, None, |_| {}).await
}

pub async fn install_versioned_with_progress<F>(
    fetcher: &mut CamoufoxFetcher,
    replace: bool,
    cancel_flag: Option<&AtomicBool>,
    mut on_progress: F,
) -> Result<bool>
where
    F: FnMut(InstallProgress) + Send,
{
    let repo_name = get_repo_name(&fetcher.github_repo);
    let sha8 = fetcher
        .selected_version
        .as_ref()
        .and_then(|s| s.sha256.as_deref())
        .or(fetcher.installed_sha256.as_deref())
        .unwrap_or("")
        .chars()
        .take(8)
        .collect::<String>();

    let ver = fetcher.version()?.to_owned();
    let build = fetcher.build()?.to_owned();
    let verstr = fetcher.verstr()?;
    let version_folder = version_folder_name(&ver, &build, &sha8);
    let install_path = browsers_dir().join(&repo_name).join(&version_folder);

    if install_path.exists() && install_path.join("version.json").exists() {
        if !replace {
            println!("Version v{verstr} already installed.");
            if load_config().get("active_version").is_none() {
                set_active(&format!("browsers/{repo_name}/{version_folder}"));
            }
            return Ok(false);
        }
        println!("Replacing: {}", install_path.display());
        fs::remove_dir_all(&install_path)?;
    }

    let temp_dir = std::env::temp_dir().join(format!(
        "camoufox-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&temp_dir)?;
    let temp_zip = temp_dir.join("camoufox.zip");

    let res: Result<bool> = async {
        fs::create_dir_all(&install_path)?;
        let url = fetcher.url()?.to_owned();
        println!("Downloading Camoufox v{verstr}: {url}");
        let actual_sha = webdl_to_file(&url, &temp_zip, cancel_flag, |p| {
            on_progress(InstallProgress {
                stage: InstallStage::Downloading,
                downloaded_bytes: p.downloaded_bytes,
                total_bytes: p.total_bytes,
                percentage: p.percentage,
                speed_bytes_per_sec: p.speed_bytes_per_sec,
                eta_seconds: p.eta_seconds,
            });
        })
        .await?;

        on_progress(InstallProgress {
            stage: InstallStage::Verifying,
            downloaded_bytes: 0,
            total_bytes: None,
            percentage: 100.0,
            speed_bytes_per_sec: 0.0,
            eta_seconds: None,
        });
        let expected_sha = fetcher
            .selected_version
            .as_ref()
            .and_then(|s| s.sha256.as_deref())
            .or(fetcher.installed_sha256.as_deref());
        verify_sha256_hex(&actual_sha, expected_sha, &format!("Camoufox v{verstr}"))?;

        on_progress(InstallProgress {
            stage: InstallStage::Extracting,
            downloaded_bytes: 0,
            total_bytes: None,
            percentage: 100.0,
            speed_bytes_per_sec: 0.0,
            eta_seconds: None,
        });
        println!("Extracting Camoufox: {}", install_path.display());
        unzip(&temp_zip, &install_path)?;

        let metadata = if let Some(sel) = &fetcher.selected_version {
            sel.to_metadata()
        } else {
            serde_json::json!({
                "version": ver,
                "build": build,
                "prerelease": fetcher.is_prerelease,
                "sha256": fetcher.installed_sha256,
                "created_at": fetcher.installed_created_at,
                "interface": fetcher.installed_interface,
            })
        };
        fs::write(
            install_path.join("version.json"),
            serde_json::to_string(&metadata).unwrap_or_default(),
        )?;

        if OS_NAME != "win" {
            make_executable(&install_path);
        }

        set_active(&format!("browsers/{repo_name}/{version_folder}"));
        fs::write(compat_flag(), "")?;
        println!("\nCamoufox v{verstr} installed.\nPath: {}", install_path.display());
        on_progress(InstallProgress {
            stage: InstallStage::Completed,
            downloaded_bytes: 0,
            total_bytes: None,
            percentage: 100.0,
            speed_bytes_per_sec: 0.0,
            eta_seconds: None,
        });
        Ok(true)
    }
    .await;

    let _ = fs::remove_dir_all(&temp_dir);
    if res.is_err() && install_path.exists() {
        let _ = fs::remove_dir_all(&install_path);
    }
    res
}

pub fn remove_version(version_path: &Path) -> bool {
    if !version_path.exists() {
        return false;
    }
    println!("Removing: {}", version_path.display());
    if fs::remove_dir_all(version_path).is_err() {
        return false;
    }
    if let Some(parent) = version_path.parent() {
        if parent != browsers_dir()
            && fs::read_dir(parent)
                .map(|mut it| it.next().is_none())
                .unwrap_or(false)
        {
            let _ = fs::remove_dir(parent);
        }
    }
    let bdir = browsers_dir();
    if bdir.exists()
        && fs::read_dir(&bdir)
            .map(|mut it| it.next().is_none())
            .unwrap_or(false)
    {
        let _ = fs::remove_dir(&bdir);
    }

    let mut config = load_config();
    if let Ok(rel) = version_path.strip_prefix(install_dir()) {
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if config.get("active_version").and_then(Value::as_str) == Some(rel_str.as_str()) {
            let remaining = list_installed();
            if let Some(first) = remaining.first() {
                config.insert("active_version".into(), Value::String(first.relative_path()));
            } else {
                config.remove("active_version");
            }
            save_config(&config);
        }
    }
    true
}

pub fn remove_incompatible_data() {
    let dir = install_dir();
    if dir.exists()
        && fs::read_dir(&dir)
            .map(|mut it| it.next().is_some())
            .unwrap_or(false)
        && !compat_flag().exists()
    {
        println!("Cleaning old data...");
        let _ = fs::remove_dir_all(&dir);
    }
}

pub fn installed_label(iv: &InstalledVersion) -> String {
    if let Some(created) = &iv.created_at {
        let date = format_asset_date(Some(created));
        if !date.is_empty() {
            return date;
        }
    }
    iv.sha256
        .as_deref()
        .unwrap_or("")
        .chars()
        .take(8)
        .collect()
}

pub fn print_tree(show_header: bool, show_paths: bool) {
    let installed = list_installed();
    if installed.is_empty() {
        println!("No versions installed.");
        println!("Run `camoufox fetch` to install.");
        return;
    }
    if show_header {
        println!("Installed versions:\n");
    }
    let mut current_repo: Option<String> = None;
    for (i, v) in installed.iter().enumerate() {
        let is_last =
            i + 1 == installed.len() || installed[i + 1].repo_name != v.repo_name;
        if current_repo.as_deref() != Some(&v.repo_name) {
            current_repo = Some(v.repo_name.clone());
            if show_paths {
                println!(
                    "{}/ -> {}",
                    v.repo_name,
                    browsers_dir().join(&v.repo_name).display()
                );
            } else {
                println!("{}/", v.repo_name);
            }
        }
        let branch = if is_last { "└── " } else { "├── " };
        let kind = if v.is_prerelease {
            "(prerelease)"
        } else {
            "(stable)"
        };
        let label = installed_label(v);
        let tag_str = if label.is_empty() {
            String::new()
        } else {
            format!(" ({label})")
        };
        let active_str = if v.is_active { " (active)" } else { "" };
        println!("    {branch}v{} {kind}{tag_str}{active_str}", v.version.full_string());
    }
}
