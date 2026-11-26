//! Browser package management: version resolution, GitHub release discovery,
//! download/extract, and path lookup, matching `pkgman.ts` / `pkgman.py`.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::browser_pin::{effective_pin, pin_matches, pin_spec, warn_if_unpaired};
use crate::exceptions::{CamoufoxError, Result};
use crate::multiversion::{
    compat_flag, find_installed_version, get_active_path, get_default_channel, load_config,
    load_repo_cache,
};
use crate::paths::{
    install_dir, launch_file_for, load_data_value, map_arch_name, map_os_name,
    supported_arches_for, OS_NAME,
};
use crate::pycompat::compare_py_str;
use crate::version::Constraints;
use crate::warnings::warn;

fn expand_user(p: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    if p == "~" {
        return home;
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(p)
}

/// Ensures `~/.camoufox` exists on Linux before launching Firefox.
pub fn ensure_browser_profile_dir(env: Option<&BTreeMap<String, String>>) -> Result<Option<PathBuf>> {
    if OS_NAME != "lin" {
        return Ok(None);
    }
    let home = env
        .and_then(|e| e.get("HOME"))
        .map(|s| expand_user(s))
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let profile_dir = home.join(".camoufox");
    if profile_dir.is_dir() {
        return Ok(Some(profile_dir));
    }
    fs::create_dir_all(&profile_dir).map_err(|err| {
        CamoufoxError::ProfileDirectoryError(format!(
            "Camoufox requires '{}' to exist before launch, but it could not be created ({err}). \
             For a read-only runtime, create this directory before making HOME read-only.",
            profile_dir.display()
        ))
    })?;
    if !profile_dir.is_dir() {
        return Err(CamoufoxError::ProfileDirectoryError(format!(
            "Camoufox requires '{}' to be a directory before launch.",
            profile_dir.display()
        )));
    }
    Ok(Some(profile_dir))
}

/// Parses a semver string into a `[major, minor, patch, ...]` numeric tuple.
pub fn parse_semver(version: &str) -> Vec<i64> {
    let trimmed = version.trim_start_matches(['^', '~']);
    let base = split_prerelease(trimmed);
    let mut out: Vec<i64> = base
        .split('.')
        .map(|part| part.trim().parse::<i64>().unwrap_or(0))
        .collect();
    while out.len() < 3 {
        out.push(0);
    }
    out
}

fn split_prerelease(s: &str) -> &str {
    for (idx, ch) in s.char_indices() {
        if idx > 0 && s.as_bytes()[idx - 1].is_ascii_digit() && matches!(ch, '-' | '+') {
            return &s[..idx];
        }
    }
    s
}

fn compare_tuples(a: &[i64], b: &[i64]) -> Ordering {
    let len = a.len().max(b.len());
    for i in 0..len {
        let x = *a.get(i).unwrap_or(&0);
        let y = *b.get(i).unwrap_or(&0);
        match x.cmp(&y) {
            Ordering::Equal => {}
            ord => return ord,
        }
    }
    Ordering::Equal
}

/// Comparable browser build version (up to 5 numeric parts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub build: String,
    pub version: Option<String>,
    pub sorted_rel: Vec<i64>,
}

impl Version {
    pub fn new(build: impl Into<String>, version: Option<String>) -> Self {
        let build = build.into();
        let dot_count = build.bytes().filter(|b| *b == b'.').count();
        let mut parts: Vec<i64> = build
            .split('.')
            .map(|x| {
                if !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit()) {
                    x.parse::<i64>().unwrap_or(0)
                } else {
                    x.chars().next().map(|c| c as i64 - 1024).unwrap_or(0)
                }
            })
            .collect();
        let padding = 5usize.saturating_sub(dot_count);
        parts.extend(std::iter::repeat_n(0, padding));
        Self {
            build,
            version,
            sorted_rel: parts,
        }
    }

    pub fn full_string(&self) -> String {
        match &self.version {
            Some(v) if !v.is_empty() => format!("{v}-{}", self.build),
            _ => self.build.clone(),
        }
    }

    pub fn is_alpha(&self) -> bool {
        self.build
            .split('.')
            .next()
            .is_some_and(|head| head.eq_ignore_ascii_case("alpha"))
    }

    pub fn is_supported(&self) -> bool {
        self >= &effective_version_min() && self < &version_max()
    }

    pub fn from_path(dir: &Path) -> Result<Self> {
        let version_path = dir.join("version.json");
        if !version_path.exists() {
            return Err(CamoufoxError::FileNotFound(format!(
                "Version information not found at {}. Please run `camoufox fetch` to install.",
                version_path.display()
            )));
        }
        let raw = fs::read_to_string(&version_path)?;
        let data: Value = serde_json::from_str(&raw)
            .map_err(|e| CamoufoxError::ValueError(e.to_string()))?;
        let build = data
            .get("release")
            .or_else(|| data.get("tag"))
            .or_else(|| data.get("build"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CamoufoxError::ValueError(format!(
                    "KeyError: 'build' (in {})",
                    version_path.display()
                ))
            })?;
        let version = data
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(Self::new(build, version))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_tuples(&self.sorted_rel, &other.sorted_rel)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub fn version_min() -> Version {
    Version::new(Constraints::MIN_VERSION, None)
}

pub fn version_max() -> Version {
    Version::new(Constraints::MAX_VERSION, None)
}

/// Playwright crate version (`playwright-rs` 0.21 -> Playwright 1.61 protocol floor).
pub fn resolved_playwright_version_str() -> String {
    // playwright-rs 0.21 targets Playwright ~1.58/1.61; if overridden via env, honor it.
    std::env::var("CAMOUFOX_PLAYWRIGHT_VERSION").unwrap_or_else(|_| "1.61.0".to_owned())
}

pub fn resolved_playwright_version() -> Vec<u32> {
    parse_semver(&resolved_playwright_version_str())
        .into_iter()
        .map(|n| n.max(0) as u32)
        .collect()
}

pub fn effective_version_min() -> Version {
    let mut floor = version_min();
    let pw: Vec<i64> = resolved_playwright_version()
        .into_iter()
        .map(i64::from)
        .collect();
    for &(pw_floor, min_build) in Constraints::PLAYWRIGHT_BROWSER_FLOORS {
        let floor_i64: Vec<i64> = pw_floor.iter().copied().map(i64::from).collect();
        if compare_tuples(&pw, &floor_i64) != Ordering::Less {
            let candidate = Version::new(min_build, None);
            if candidate > floor {
                floor = candidate;
            }
        }
    }
    floor
}

pub fn interface_supported(iface: u32) -> bool {
    (Constraints::MIN_INTERFACE..=Constraints::INTERFACE).contains(&iface)
}

pub fn installed_interface(dir: &Path) -> u32 {
    let Ok(raw) = fs::read_to_string(dir.join("version.json")) else {
        return 1;
    };
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|v| v.get("interface").and_then(Value::as_u64))
        .unwrap_or(1) as u32
}

pub fn upgrade_message(ver: &Version, iface: u32) -> String {
    format!(
        "Camoufox v{} uses interface v{iface}, which requires a newer `camoufox-rs` package.",
        ver.full_string()
    )
}

pub fn incompatible_message(ver: &Version, iface: u32) -> String {
    if iface > Constraints::INTERFACE {
        upgrade_message(ver, iface)
    } else {
        format!(
            "Camoufox v{} (interface v{iface}) is too old for this `camoufox-rs` package (min interface v{}).",
            ver.full_string(),
            Constraints::MIN_INTERFACE
        )
    }
}

static OUTDATED_WARNED: AtomicBool = AtomicBool::new(false);

pub fn warn_if_package_outdated() {
    if OUTDATED_WARNED.load(AtomicOrdering::Relaxed) {
        return;
    }
    let cache = load_repo_cache();
    let Some(newer) = cache.get("incompatible").and_then(Value::as_array) else {
        return;
    };
    if newer.is_empty() {
        return;
    }
    OUTDATED_WARNED.store(true, AtomicOrdering::Relaxed);
    if let Some(newest) = newer.iter().max_by(|a, b| {
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
        warn(&upgrade_message(&ver, iface), "RuntimeWarning");
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RepoSpecYaml {
    repo: String,
    name: String,
    pattern: String,
}

#[derive(Debug, Clone)]
pub struct RepoConfig {
    pub name: String,
    pub repos: Vec<String>,
    pub pattern: String,
}

impl RepoConfig {
    pub fn load_repos() -> Vec<Self> {
        let Ok(val) = load_data_value("repos.yml") else {
            return Vec::new();
        };
        let Some(arr) = val.get("browsers").and_then(Value::as_array) else {
            return Vec::new();
        };
        arr.iter()
            .filter_map(|item| serde_json::from_value::<RepoSpecYaml>(item.clone()).ok())
            .map(|spec| Self {
                name: spec.name,
                repos: spec
                    .repo
                    .split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect(),
                pattern: spec.pattern,
            })
            .collect()
    }

    pub fn get_default_name() -> String {
        load_data_value("repos.yml")
            .ok()
            .and_then(|v| v.get("default")?.get("browser")?.as_str().map(str::to_owned))
            .unwrap_or_else(|| "Official".to_owned())
    }

    pub fn get_default() -> Self {
        let name = Self::get_default_name();
        Self::find_by_name(&name)
            .or_else(|| Self::load_repos().into_iter().next())
            .unwrap_or_else(|| Self {
                name: "Official".to_owned(),
                repos: vec!["daijro/camoufox".to_owned(), "camoufox/camoufox".to_owned()],
                pattern: "{name}-{version}-{build}-{os}.{arch}.zip".to_owned(),
            })
    }

    pub fn find_by_name(name: &str) -> Option<Self> {
        Self::load_repos()
            .into_iter()
            .find(|r| r.name.eq_ignore_ascii_case(name))
    }

    pub fn primary_repo(&self) -> &str {
        self.repos.first().map(String::as_str).unwrap_or("daijro/camoufox")
    }

    pub fn get_arch(&self, spoof_arch: Option<&str>) -> String {
        let raw = spoof_arch.unwrap_or(std::env::consts::ARCH);
        map_arch_name(raw)
            .unwrap_or_else(|| crate::paths::host_arch())
            .to_owned()
    }

    pub fn build_pattern(
        &self,
        spoof_os: Option<&str>,
        spoof_arch: Option<&str>,
    ) -> Regex {
        let os_key = spoof_os
            .and_then(map_os_name)
            .or(spoof_os)
            .unwrap_or(OS_NAME);
        let arch = self.get_arch(spoof_arch);
        let regex_str = self
            .pattern
            .replace('.', r"\.")
            .replace("{name}", "(?P<name>camoufox)")
            .replace("{version}", r"(?P<version>[^-]+)")
            .replace("{build}", r"(?P<build>[^-]+)")
            .replace("{os}", &regex::escape(os_key))
            .replace("{arch}", &regex::escape(&arch));
        Regex::new(&format!("^{regex_str}$")).expect("valid repo asset regex")
    }

    pub fn is_version_supported(&self, version: &Version, _is_prerelease: bool) -> bool {
        version.is_supported()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubAsset {
    pub id: Option<u64>,
    pub name: String,
    pub size: Option<u64>,
    pub updated_at: Option<String>,
    pub created_at: Option<String>,
    pub digest: Option<String>,
    pub browser_download_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub prerelease: Option<bool>,
    pub assets: Option<Vec<GitHubAsset>>,
}

#[derive(Debug, Clone)]
pub struct AvailableVersion {
    pub version: Version,
    pub url: String,
    pub is_prerelease: bool,
    pub asset_id: Option<u64>,
    pub asset_size: Option<u64>,
    pub asset_updated_at: Option<String>,
    pub sha256: Option<String>,
    pub asset_created_at: Option<String>,
    pub interface: u32,
}

impl AvailableVersion {
    pub fn sha8(&self) -> String {
        self.sha256
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(8)
            .collect()
    }

    pub fn display(&self) -> String {
        let pre = if self.is_prerelease { " (prerelease)" } else { "" };
        format!("v{}{pre}", self.version.full_string())
    }

    pub fn to_metadata(&self) -> Value {
        serde_json::json!({
            "version": self.version.version,
            "build": self.version.build,
            "prerelease": self.is_prerelease,
            "asset_id": self.asset_id,
            "asset_size": self.asset_size,
            "asset_updated_at": self.asset_updated_at,
            "sha256": self.sha256,
            "created_at": self.asset_created_at,
            "interface": self.interface,
        })
    }
}

fn http_client(total_timeout: Option<Duration>) -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60))
        .user_agent(format!("camoufox-rs/{}", crate::version::VERSION));
    if let Some(t) = total_timeout {
        b = b.timeout(t);
    }
    b.build()
        .map_err(|e| CamoufoxError::ValueError(e.to_string()))
}

async fn github_get_with(url: &str, client: &reqwest::Client) -> Result<reqwest::Response> {
    let delays = [Duration::from_secs(1), Duration::from_secs(2)];
    for attempt in 0..=delays.len() {
        let mut req = client.get(url);
        if url.contains("api.github.com") {
            req = req.header("Accept", "application/vnd.github+json");
            if let Ok(token) = std::env::var("GITHUB_TOKEN") {
                if !token.trim().is_empty() {
                    req = req.bearer_auth(token.trim());
                }
            }
        }
        match req.send().await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    return Err(CamoufoxError::ValueError(format!(
                        "{status} HTTP error for url: {url}"
                    )));
                }
                return Ok(resp);
            }
            Err(err) => {
                if attempt < delays.len() {
                    tokio::time::sleep(delays[attempt]).await;
                    continue;
                }
                return Err(CamoufoxError::ValueError(format!(
                    "Could not download {url}: {err}"
                )));
            }
        }
    }
    unreachable!()
}

async fn github_get(url: &str) -> Result<reqwest::Response> {
    let client = http_client(Some(Duration::from_secs(60)))?;
    github_get_with(url, &client).await
}

async fn github_releases(repo: &str) -> Result<Vec<GitHubRelease>> {
    let api_url = format!("https://api.github.com/repos/{repo}/releases?per_page=100");
    let resp = github_get(&api_url).await?;
    resp.json::<Vec<GitHubRelease>>()
        .await
        .map_err(|e| CamoufoxError::ValueError(e.to_string()))
}

async fn release_interface(_release: &GitHubRelease) -> u32 {
    1
}

pub struct CamoufoxFetcher {
    pub github_repos: Vec<String>,
    pub github_repo: String,
    pub is_prerelease: bool,
    pub repo_config: RepoConfig,
    pub arch: String,
    pub pattern: Regex,
    pub installed_sha256: Option<String>,
    pub installed_created_at: Option<String>,
    pub installed_interface: u32,
    pub version_obj: Option<Version>,
    pub selected_version: Option<AvailableVersion>,
    pub url_str: Option<String>,
}

impl CamoufoxFetcher {
    pub fn new(repo_config: Option<RepoConfig>, selected_version: Option<AvailableVersion>) -> Result<Self> {
        let repo_config = repo_config.unwrap_or_else(RepoConfig::get_default);
        let github_repos = repo_config.repos.clone();
        let github_repo = repo_config.primary_repo().to_owned();
        let arch = repo_config.get_arch(None);
        if !supported_arches_for(OS_NAME).contains(&arch.as_str()) {
            return Err(CamoufoxError::UnsupportedArchitecture(format!(
                "Architecture {arch} is not supported for {OS_NAME}"
            )));
        }
        let pattern = repo_config.build_pattern(None, None);
        let mut fetcher = Self {
            github_repos,
            github_repo,
            is_prerelease: false,
            repo_config,
            arch,
            pattern,
            installed_sha256: None,
            installed_created_at: None,
            installed_interface: 1,
            version_obj: None,
            selected_version: None,
            url_str: None,
        };
        if let Some(sel) = selected_version {
            fetcher.version_obj = Some(sel.version.clone());
            fetcher.url_str = Some(sel.url.clone());
            fetcher.is_prerelease = sel.is_prerelease;
            fetcher.installed_sha256 = sel.sha256.clone();
            fetcher.installed_created_at = sel.asset_created_at.clone();
            fetcher.installed_interface = sel.interface;
            fetcher.selected_version = Some(sel);
        }
        Ok(fetcher)
    }

    pub async fn init(&mut self) -> Result<()> {
        if self.version_obj.is_none() {
            self.fetch_latest().await?;
        }
        Ok(())
    }

    pub fn check_asset(
        &mut self,
        asset: &GitHubAsset,
        release: Option<&GitHubRelease>,
    ) -> Option<(Version, String)> {
        let caps = self.pattern.captures(&asset.name)?;
        let ver_str = caps.name("version")?.as_str().to_owned();
        let build_str = caps.name("build")?.as_str().to_owned();
        let version = Version::new(&build_str, Some(ver_str.clone()));

        let pin = effective_pin(&load_config());
        if let Some(ref p) = pin {
            if !pin_matches(p, &self.repo_config.name, &ver_str, &build_str) {
                return None;
            }
        }
        let is_pre = release.and_then(|r| r.prerelease).unwrap_or(false) || version.is_alpha();
        if pin.is_none() && !self.repo_config.is_version_supported(&version, is_pre) {
            return None;
        }
        if let Some(digest) = &asset.digest {
            if let Some(sha) = digest.strip_prefix("sha256:") {
                self.installed_sha256 = Some(sha.to_owned());
            }
        }
        self.installed_created_at = asset.created_at.clone();
        Some((version, asset.browser_download_url.clone()))
    }

    pub async fn fetch_latest(&mut self) -> Result<()> {
        let mut last_error: Option<CamoufoxError> = None;
        let repos = self.github_repos.clone();
        for repo in repos {
            match github_releases(&repo).await {
                Ok(releases) => {
                    for release in releases {
                        for asset in release.assets.as_deref().unwrap_or(&[]) {
                            if let Some((ver, url)) = self.check_asset(asset, Some(&release)) {
                                let iface = release_interface(&release).await;
                                if !interface_supported(iface) {
                                    println!("{}", incompatible_message(&ver, iface));
                                    continue;
                                }
                                self.installed_interface = iface;
                                self.github_repo = repo;
                                self.is_prerelease = release.prerelease.unwrap_or(false);
                                self.version_obj = Some(ver);
                                self.url_str = Some(url);
                                return Ok(());
                            }
                        }
                    }
                }
                Err(err) => last_error = Some(err),
            }
        }
        if let Some(err) = last_error {
            return Err(err);
        }
        Err(CamoufoxError::MissingRelease(format!(
            "No matching release found for {OS_NAME} {} in the supported range. Please update the camoufox-rs package.",
            self.arch
        )))
    }

    pub async fn install(&mut self, replace: bool) -> Result<bool> {
        self.install_with_progress(replace, None, |_| {}).await
    }

    pub async fn install_with_progress<F>(
        &mut self,
        replace: bool,
        cancel_flag: Option<&AtomicBool>,
        on_progress: F,
    ) -> Result<bool>
    where
        F: FnMut(crate::multiversion::InstallProgress) + Send,
    {
        let installed =
            crate::multiversion::install_versioned_with_progress(self, replace, cancel_flag, on_progress)
                .await?;
        let _ = ensure_browser_profile_dir(None);
        Ok(installed)
    }

    pub fn url(&self) -> Result<&str> {
        self.url_str
            .as_deref()
            .ok_or_else(|| CamoufoxError::ValueError("Url not available; run init() first.".into()))
    }

    pub fn version(&self) -> Result<&str> {
        self.version_obj
            .as_ref()
            .and_then(|v| v.version.as_deref())
            .ok_or_else(|| CamoufoxError::ValueError("Version not available; run init() first.".into()))
    }

    pub fn build(&self) -> Result<&str> {
        self.version_obj
            .as_ref()
            .map(|v| v.build.as_str())
            .ok_or_else(|| CamoufoxError::ValueError("Build not available; run init() first.".into()))
    }

    pub fn verstr(&self) -> Result<String> {
        self.version_obj
            .as_ref()
            .map(Version::full_string)
            .ok_or_else(|| CamoufoxError::ValueError("Version not available; run init() first.".into()))
    }
}

pub async fn list_available_versions(
    repo_config: Option<RepoConfig>,
    include_prerelease: bool,
    spoof_os: Option<&str>,
    spoof_arch: Option<&str>,
    include_incompatible: bool,
) -> Result<Vec<AvailableVersion>> {
    let config = repo_config.unwrap_or_else(RepoConfig::get_default);
    let os_name = spoof_os
        .and_then(map_os_name)
        .or(spoof_os)
        .unwrap_or(OS_NAME);
    let arch = config.get_arch(spoof_arch);
    if !supported_arches_for(os_name).contains(&arch.as_str()) {
        return Err(CamoufoxError::UnsupportedArchitecture(format!(
            "Architecture {arch} is not supported for {os_name}"
        )));
    }
    let pattern = config.build_pattern(spoof_os, spoof_arch);

    let mut releases = Vec::new();
    let mut last_err = None;
    for repo in &config.repos {
        match github_releases(repo).await {
            Ok(r) => {
                releases = r;
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    if releases.is_empty() {
        if let Some(err) = last_err {
            return Err(err);
        }
    }

    let mut versions = Vec::new();
    for release in releases {
        versions.extend(
            release_versions(&release, &config, &pattern, include_prerelease, include_incompatible)
                .await,
        );
    }
    versions.sort_by(|a, b| {
        b.version.cmp(&a.version).then_with(|| {
            compare_py_str(
                b.asset_created_at.as_deref().unwrap_or(""),
                a.asset_created_at.as_deref().unwrap_or(""),
            )
        })
    });
    Ok(versions)
}

pub async fn tagged_versions(repo_config: &RepoConfig, tag: &str) -> Result<Vec<AvailableVersion>> {
    let pattern = repo_config.build_pattern(None, None);
    for repo in &repo_config.repos {
        let api_url = format!("https://api.github.com/repos/{repo}/releases/tags/{tag}");
        match github_get(&api_url).await {
            Ok(resp) => {
                let release: GitHubRelease = resp
                    .json()
                    .await
                    .map_err(|e| CamoufoxError::ValueError(e.to_string()))?;
                return Ok(release_versions(&release, repo_config, &pattern, true, false).await);
            }
            Err(err) if err.to_string().contains("404") => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(Vec::new())
}

async fn release_versions(
    release: &GitHubRelease,
    config: &RepoConfig,
    pattern: &Regex,
    include_prerelease: bool,
    include_incompatible: bool,
) -> Vec<AvailableVersion> {
    let is_prerelease = release.prerelease.unwrap_or(false);
    if is_prerelease && !include_prerelease {
        return Vec::new();
    }
    let mut out = Vec::new();
    for asset in release.assets.as_deref().unwrap_or(&[]) {
        let Some(caps) = pattern.captures(&asset.name) else {
            continue;
        };
        let ver_str = caps.name("version").map(|m| m.as_str()).unwrap_or("");
        let build_str = caps.name("build").map(|m| m.as_str()).unwrap_or("");
        let version = Version::new(build_str, Some(ver_str.to_owned()));
        let asset_pre = is_prerelease || version.is_alpha();
        if asset_pre && !include_prerelease {
            continue;
        }
        if !config.is_version_supported(&version, asset_pre) {
            continue;
        }
        let iface = release_interface(release).await;
        if !interface_supported(iface) && !include_incompatible {
            println!("{}", incompatible_message(&version, iface));
            continue;
        }
        let sha256 = asset
            .digest
            .as_deref()
            .and_then(|d| d.strip_prefix("sha256:"))
            .map(str::to_owned);
        out.push(AvailableVersion {
            version,
            url: asset.browser_download_url.clone(),
            is_prerelease: asset_pre,
            asset_id: asset.id,
            asset_size: asset.size,
            asset_updated_at: asset.updated_at.clone(),
            sha256,
            asset_created_at: asset.created_at.clone(),
            interface: iface,
        });
    }
    out
}

fn not_installed_error() -> CamoufoxError {
    let config = load_config();
    let pin = effective_pin(&config);
    let pinned = config.get("pinned").and_then(Value::as_str);
    let channel = config
        .get("channel")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(get_default_channel);
    let missing = if let Some(p) = pin {
        format!(
            "{} {}, the browser this camoufox release pairs with,",
            p.repo_name,
            pin_spec(&p)
        )
    } else if let Some(pin_ver) = pinned {
        format!("{channel}/{pin_ver}")
    } else {
        channel
    };
    CamoufoxError::CamoufoxNotInstalled(format!(
        "{missing} is not installed. Please run `camoufox fetch` to install."
    ))
}

pub fn installed_verstr(from_dir: Option<&Path>) -> Result<String> {
    if let Some(dir) = from_dir {
        let resolved_dir = if dir.file_name().and_then(|s| s.to_str()) == Some("MacOS")
            && !dir.join("version.json").exists()
        {
            dir.parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .unwrap_or(dir)
        } else {
            dir
        };
        return Ok(Version::from_path(resolved_dir)?.full_string());
    }
    let active = get_active_path().ok_or_else(not_installed_error)?;
    let version = Version::from_path(&active)?;
    if active
        .parent()
        .and_then(Path::parent)
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        == Some("browsers")
    {
        let repo = active
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("");
        warn_if_unpaired(
            repo,
            version.version.as_deref().unwrap_or(""),
            &version.build,
        );
    }
    Ok(version.full_string())
}

pub fn root_install_supported() -> Result<bool> {
    match Version::from_path(&install_dir()) {
        Ok(v) => Ok(v.is_supported()),
        Err(CamoufoxError::FileNotFound(_)) => Ok(false),
        Err(err) => Err(err),
    }
}

fn resolve_installed_path(download_if_missing: bool) -> Result<Option<PathBuf>> {
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

    if let Some(active) = get_active_path() {
        let installed = Version::from_path(&active)?;
        let iface = installed_interface(&active);
        if !interface_supported(iface) {
            return Err(CamoufoxError::UnsupportedVersion(incompatible_message(
                &installed, iface,
            )));
        }
        if installed.is_supported() {
            return Ok(Some(active));
        }
    }

    if !dir.exists()
        || fs::read_dir(&dir)
            .map(|mut it| it.next().is_none())
            .unwrap_or(true)
    {
        if !download_if_missing {
            return Err(not_installed_error());
        }
    } else if root_install_supported()? {
        return Ok(Some(dir));
    } else if !download_if_missing {
        if effective_pin(&load_config()).is_some() {
            return Err(not_installed_error());
        }
        return Err(CamoufoxError::UnsupportedVersion(
            "Camoufox executable is outdated.".to_owned(),
        ));
    }
    Ok(None)
}

pub fn camoufox_path() -> Result<PathBuf> {
    resolve_installed_path(false)?
        .ok_or_else(not_installed_error)
}

pub async fn ensure_camoufox_installed() -> Result<PathBuf> {
    if let Some(found) = resolve_installed_path(true)? {
        return Ok(found);
    }
    let mut fetcher = CamoufoxFetcher::new(None, None)?;
    fetcher.init().await?;
    fetcher.install(false).await?;

    if let Some(active) = get_active_path() {
        if Version::from_path(&active)?.is_supported() {
            return Ok(active);
        }
    }
    let dir = install_dir();
    if dir.exists() && root_install_supported()? {
        return Ok(dir);
    }
    Err(CamoufoxError::UnsupportedVersion(format!(
        "No available Camoufox build satisfies this library's minimum ({}). \
         The matching browser release may not be published yet; wait for it, or install an older release.",
        Constraints::MIN_VERSION
    )))
}

/// Resolves a file shipped alongside the browser binary (`Contents/Resources/` for macOS app bundles).
pub fn build_file(executable_path: &Path, name: &str) -> PathBuf {
    let Some(dir) = executable_path.parent() else {
        return PathBuf::from(name);
    };
    if dir.file_name().and_then(|s| s.to_str()) == Some("MacOS") {
        dir.parent()
            .map(|p| p.join("Resources").join(name))
            .unwrap_or_else(|| dir.join(name))
    } else {
        dir.join(name)
    }
}

pub fn get_path(file: &str) -> Result<PathBuf> {
    let base = camoufox_path()?;
    if OS_NAME == "mac" {
        Ok(base
            .join("Camoufox.app")
            .join("Contents")
            .join("Resources")
            .join(file))
    } else {
        Ok(base.join(file))
    }
}

pub fn launch_path(browser_path: Option<&Path>) -> Result<PathBuf> {
    let exec_path = if let Some(bp) = browser_path {
        if OS_NAME == "mac" {
            bp.join("Camoufox.app")
                .join("Contents")
                .join("MacOS")
                .join("camoufox")
        } else {
            bp.join(launch_file_for(OS_NAME))
        }
    } else if OS_NAME == "mac" {
        camoufox_path()?
            .join("Camoufox.app")
            .join("Contents")
            .join("MacOS")
            .join("camoufox")
    } else {
        get_path(launch_file_for(OS_NAME))?
    };

    if !exec_path.exists() {
        return Err(CamoufoxError::CamoufoxNotInstalled(format!(
            "Camoufox is not installed at {}. Please run `camoufox fetch` to install.",
            browser_path
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| camoufox_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default())
        )));
    }
    Ok(exec_path)
}

pub fn selected_launch_path(specifier: &str) -> Result<PathBuf> {
    let install_path = find_installed_version(specifier).ok_or_else(|| {
        CamoufoxError::ValueError(format!(
            "Browser version '{specifier}' not found. Run `camoufox list` to see installed versions."
        ))
    })?;
    let version = Version::from_path(&install_path)?;
    let iface = installed_interface(&install_path);
    if !interface_supported(iface) {
        return Err(CamoufoxError::UnsupportedVersion(incompatible_message(
            &version, iface,
        )));
    }
    let repo_name = install_path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("");
    warn_if_unpaired(
        repo_name,
        version.version.as_deref().unwrap_or(""),
        &version.build,
    );
    launch_path(Some(&install_path))
}

/// Progress snapshot reported by [`webdl_to_file`].
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub percentage: f64,
    pub speed_bytes_per_sec: f64,
    pub eta_seconds: Option<u64>,
}

/// Streams `url` directly to `dest` with throttling progress callbacks, optional cancellation,
/// and incremental SHA-256 hashing (returns lowercase hex digest).
pub async fn webdl_to_file<F>(
    url: &str,
    dest: &Path,
    cancel_flag: Option<&AtomicBool>,
    mut on_progress: F,
) -> Result<String>
where
    F: FnMut(DownloadProgress) + Send,
{
    use std::io::Write as _;

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let client = http_client(None)?;
    let mut resp = github_get_with(url, &client).await?;
    let total = resp.content_length();
    let file = fs::File::create(dest)?;
    let mut writer = std::io::BufWriter::new(file);
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let start = std::time::Instant::now();
    let mut last_emit = std::time::Instant::now();

    on_progress(DownloadProgress {
        downloaded_bytes: 0,
        total_bytes: total,
        percentage: 0.0,
        speed_bytes_per_sec: 0.0,
        eta_seconds: None,
    });

    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| CamoufoxError::ValueError(e.to_string()))?
    {
        if cancel_flag.is_some_and(|flag| flag.load(AtomicOrdering::Relaxed)) {
            drop(writer);
            let _ = fs::remove_file(dest);
            return Err(CamoufoxError::DownloadCancelled("Download cancelled".into()));
        }
        writer.write_all(&chunk)?;
        hasher.update(&chunk);
        received += chunk.len() as u64;

        if last_emit.elapsed() >= Duration::from_millis(200) {
            let secs = start.elapsed().as_secs_f64().max(0.01);
            let speed = received as f64 / secs;
            let pct = total
                .filter(|t| *t > 0)
                .map(|t| (received as f64 / t as f64) * 100.0)
                .unwrap_or(0.0);
            let eta = total
                .filter(|t| *t > received && speed > 1.0)
                .map(|t| ((t - received) as f64 / speed) as u64);
            on_progress(DownloadProgress {
                downloaded_bytes: received,
                total_bytes: total,
                percentage: pct,
                speed_bytes_per_sec: speed,
                eta_seconds: eta,
            });
            last_emit = std::time::Instant::now();
        }
    }

    writer.flush()?;
    let secs = start.elapsed().as_secs_f64().max(0.01);
    on_progress(DownloadProgress {
        downloaded_bytes: received,
        total_bytes: total.or(Some(received)),
        percentage: 100.0,
        speed_bytes_per_sec: received as f64 / secs,
        eta_seconds: Some(0),
    });
    Ok(hex::encode(hasher.finalize()))
}

pub async fn webdl(url: &str, desc: Option<&str>, bar: bool) -> Result<Vec<u8>> {
    if let Some(d) = desc {
        println!("{d}: {url}");
    }
    let client = http_client(None)?;
    let mut resp = github_get_with(url, &client).await?;
    let total = resp.content_length();
    let mut buf = Vec::with_capacity(total.unwrap_or(0) as usize);
    let mut next_log = 15 * 1024 * 1024usize;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| CamoufoxError::ValueError(e.to_string()))?
    {
        buf.extend_from_slice(&chunk);
        if bar && buf.len() >= next_log {
            if let Some(tot) = total {
                println!(
                    "  {:.1} MB / {:.1} MB",
                    buf.len() as f64 / (1024.0 * 1024.0),
                    tot as f64 / (1024.0 * 1024.0)
                );
            }
            next_log = buf.len() + 15 * 1024 * 1024;
        }
    }
    Ok(buf)
}

pub fn verify_sha256_hex(actual_hex: &str, expected: Option<&str>, desc: &str) -> Result<()> {
    let Some(expected) = expected.filter(|s| !s.is_empty()) else {
        println!("Warning: no sha256 published for {desc}; skipping verification.");
        return Ok(());
    };
    let expected_lower = expected.to_ascii_lowercase();
    let actual_lower = actual_hex.to_ascii_lowercase();
    if actual_lower != expected_lower {
        return Err(CamoufoxError::CorruptedDownload(format!(
            "Checksum mismatch for {desc}.\n  expected sha256: {expected_lower}\n  actual   sha256: {actual_lower}\n\
             The download was corrupted or tampered with. Installation aborted."
        )));
    }
    Ok(())
}

pub fn verify_sha256(bytes: &[u8], expected: Option<&str>, desc: &str) -> Result<()> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let actual = hex::encode(hasher.finalize());
    verify_sha256_hex(&actual, expected, desc)
}

pub fn unzip(zip_path: &Path, extract_path: &Path) -> Result<()> {
    let file = fs::File::open(zip_path)?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| CamoufoxError::ValueError(e.to_string()))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| CamoufoxError::ValueError(e.to_string()))?;
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out_path = extract_path.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out_path)?;
        } else if entry.is_symlink() {
            #[cfg(unix)]
            {
                use std::io::Read as _;
                if let Some(parent) = out_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut target = String::new();
                entry.read_to_string(&mut target)?;
                let _ = fs::remove_file(&out_path);
                std::os::unix::fs::symlink(target, &out_path)?;
            }
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out)?;
        }
    }
    Ok(())
}

pub fn make_executable(dir: &Path) {
    if OS_NAME == "win" {
        return;
    }
    if let Err(err) = Command::new("chmod").arg("-R").arg("755").arg(dir).status() {
        println!("Warning: could not chmod {}: {err}", dir.display());
    }
}

pub fn format_asset_date(iso: Option<&str>) -> String {
    let Some(iso) = iso.filter(|s| !s.is_empty()) else {
        return String::new();
    };
    // ISO8601 prefix `YYYY-MM-DD...`
    if iso.len() < 10 {
        return String::new();
    }
    let year: i32 = iso[0..4].parse().unwrap_or(0);
    let month: usize = iso[5..7].parse().unwrap_or(0);
    let day: u32 = iso[8..10].parse().unwrap_or(0);
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let Some(mon) = month.checked_sub(1).and_then(|i| MONTHS.get(i)) else {
        return String::new();
    };
    if year == 2026 {
        format!("{mon} {day}")
    } else {
        format!("{mon} {day}, {year}")
    }
}
