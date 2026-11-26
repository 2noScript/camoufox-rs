//! Launch-option assembly, fontconfig/quota/pref helpers, and virtual display lifecycle,
//! matching `utils.ts` / `utils.py`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::addons::{add_default_addons, confirm_paths, DefaultAddons};
use crate::coherence::{
    self, cpu_conflict, drop_incoherent_source_values, repair_device_pixel_ratio,
    repair_screen_orientation, screen_is_implausible, Violation,
};
use crate::config::validate_config;
use crate::cpu_affinity;
use crate::display::{has_display, largest_display};
use crate::exceptions::{CamoufoxError, Result};
use crate::fingerprints::{
    drop_audio_seed, draw_voice_config, fix_hardware_concurrency, fix_navigator_arch,
    follow_baseline_protection, from_fpgen, from_preset, generate_fingerprint,
    generate_random_font_subset, get_random_preset, identity_salt, identity_seed, load_os_fonts,
    clamp_screen_to_display, clamp_window_dimensions, clamp_window_position,
    raise_screen_to_modern_floor, set_cpu_default, set_media_devices_defaults,
    target_os_from_platform, windows11_marker_fonts, Screen,
};
use crate::fpgen_model::ensure_fpgen_model;
use crate::geolocation::{geoip_allowed, get_geolocation};
use crate::gpudisplay::{gpu_prefs, select_gpu, GpuDisplay};
use crate::headless_stream::{apply_headless_stream_env, HeadlessStreamOptions};
use crate::host_rendering::{
    add_canvas_placeholder, has_canvas_placeholder, host_gpu, set_webgpu, Gpu,
};
use crate::humanize::{normalize as normalize_humanize, HumanizeSetting};
use crate::ip::{
    proxy_auth_env, public_ip, public_ipv6, socks5_auth, valid_ipv4, valid_ipv6, ProxyConfig,
};
use crate::locales::handle_locales;
use crate::paths::{install_dir, launcher_constants, OS_NAME};
use crate::pkgman::{
    build_file, effective_version_min, ensure_browser_profile_dir, ensure_camoufox_installed,
    get_path, installed_verstr, launch_path, resolved_playwright_version_str,
    selected_launch_path, warn_if_package_outdated, Version,
};
use crate::pycompat::{
    compare_py_str, orjson_dumps, py_json_dumps_ascii, py_str, py_truthy,
};
use crate::virtdisplay::VirtualDisplay;
use crate::warnings::{load_warnings, warn, FallbackWarning, LeakWarning};
use crate::webgl::{firefox_gpus, sample_webgl_for_screen, webgl_for_gpu};

const QUOTA_FIXED_LIMIT_PREF: &str = "dom.quotaManager.temporaryStorage.fixedLimit";
const INT32_MAX: u64 = (1u64 << 31) - 1;
const WINDOW_DIM_KEYS: [&str; 4] = [
    "window.outerWidth",
    "window.outerHeight",
    "window.innerWidth",
    "window.innerHeight",
];

pub fn short_os_to_full(short: &str) -> &'static str {
    match short {
        "win" | "windows" => "windows",
        "mac" | "macos" => "macos",
        _ => "linux",
    }
}

pub fn host_os_key() -> &'static str {
    OS_NAME
}

pub fn cache_prefs() -> &'static Map<String, Value> {
    &launcher_constants().cache_prefs
}

pub fn windows_behaviour_prefs() -> &'static Map<String, Value> {
    &launcher_constants().windows_behaviour_prefs
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HeadlessMode {
    Bool(bool),
    Virtual,
    VirtualGpu,
}

impl Default for HeadlessMode {
    fn default() -> Self {
        Self::Bool(false)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuOption {
    pub arch: Option<String>,
    pub manufacturer: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CamoufoxOptions {
    pub config: Map<String, Value>,
    pub os: Vec<String>,
    pub block_images: bool,
    pub block_webrtc: bool,
    pub block_webgl: bool,
    pub disable_coop: bool,
    pub webgl_config: Option<(String, String)>,
    pub cpu: Option<CpuOption>,
    pub geoip: Option<GeoipOption>,
    pub geoip_db: Option<String>,
    pub humanize: Option<HumanizeSetting>,
    pub locale: Vec<String>,
    pub addons: Vec<PathBuf>,
    pub fonts: Vec<String>,
    pub custom_fonts_only: bool,
    pub exclude_addons: Vec<DefaultAddons>,
    pub screen: Option<Screen>,
    pub window: Option<(i64, i64)>,
    pub fingerprint: Option<Value>,
    pub fingerprint_preset: Option<PresetOption>,
    pub ff_version: Option<u32>,
    pub headless: HeadlessMode,
    pub main_world_eval: bool,
    pub allow_addon_new_tab: bool,
    pub executable_path: Option<PathBuf>,
    pub browser: Option<String>,
    pub firefox_user_prefs: Map<String, Value>,
    pub proxy: Option<ProxyConfig>,
    pub enable_cache: bool,
    pub args: Vec<String>,
    pub env: Option<BTreeMap<String, String>>,
    pub i_know_what_im_doing: bool,
    pub debug: bool,
    pub virtual_display: Option<String>,
    pub pin_cpu_cores: bool,
    pub canvas_noise: Option<bool>,
    pub gpu: Option<String>,
    pub require_gpu: Option<bool>,
    pub persistent_context: bool,
    pub user_data_dir: Option<PathBuf>,
    pub is_mobile: bool,
    pub no_viewport: Option<bool>,
    pub port: Option<u16>,
    pub ws_path: Option<String>,
    pub timeout_ms: Option<f64>,
    pub slow_mo_ms: Option<f64>,
    pub ignore_https_errors: Option<bool>,
    pub downloads_path: Option<PathBuf>,
    pub headless_stream: Option<HeadlessStreamOptions>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeoipOption {
    Auto,
    Ip(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PresetOption {
    Random,
    Custom(Value),
}

/// Output of `launch_options()`: ready to hand to `playwright-rs` or Inspect.
#[derive(Debug, Clone)]
pub struct PreparedLaunchOptions {
    pub executable_path: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub firefox_user_prefs: Map<String, Value>,
    pub headless: bool,
    pub proxy: Option<ProxyConfig>,
    pub target_os: String,
    pub config: Map<String, Value>,
    pub timeout_ms: Option<f64>,
    pub slow_mo_ms: Option<f64>,
    pub downloads_path: Option<PathBuf>,
}

impl PreparedLaunchOptions {
    pub fn to_playwright_launch_options(&self) -> playwright_rs::api::LaunchOptions {
        let env_map: HashMap<String, String> =
            self.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let prefs_map: HashMap<String, Value> = self
            .firefox_user_prefs
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut opts = playwright_rs::api::LaunchOptions::new()
            .executable_path(self.executable_path.display().to_string())
            .headless(self.headless)
            .env(env_map)
            .firefox_user_prefs(prefs_map);
        if !self.args.is_empty() {
            opts = opts.args(self.args.clone());
        }
        if let Some(ref p) = self.proxy {
            opts = opts.proxy(p.to_playwright());
        }
        if let Some(t) = self.timeout_ms {
            opts = opts.timeout(t);
        }
        if let Some(sm) = self.slow_mo_ms {
            opts = opts.slow_mo(sm);
        }
        if let Some(ref dp) = self.downloads_path {
            opts = opts.downloads_path(dp.display().to_string());
        }
        opts
    }

    pub fn env_value(&self, key: &str) -> Option<&str> {
        self.env.get(key).map(String::as_str)
    }

    pub fn to_json_value(&self) -> Value {
        serde_json::json!({
            "executablePath": self.executable_path,
            "args": self.args,
            "env": self.env,
            "firefoxUserPrefs": self.firefox_user_prefs,
            "headless": self.headless,
            "proxy": self.proxy,
        })
    }
}

pub enum ActiveDisplay {
    Xvfb(VirtualDisplay),
    Gpu(GpuDisplay),
}

impl ActiveDisplay {
    pub fn display_str(&self) -> Option<String> {
        match self {
            Self::Xvfb(v) => v.display.map(|d| format!(":{d}")),
            Self::Gpu(g) => g.display.map(|d| format!(":{d}")),
        }
    }

    pub fn kill(&mut self) {
        match self {
            Self::Xvfb(v) => v.kill(),
            Self::Gpu(g) => g.kill(),
        }
    }
}

pub fn saved_pref(profile_dir: Option<&Path>, pref: &str) -> Option<Value> {
    let dir = profile_dir?;
    let text = fs::read_to_string(dir.join("prefs.js")).ok()?;
    let pattern = format!(r#"user_pref\("{}",\s*(.+?)\);"#, regex::escape(pref));
    let re = Regex::new(&pattern).ok()?;
    let caps = re.captures(&text)?;
    let raw = &caps[1];
    serde_json::from_str(raw).ok().or_else(|| Some(Value::String(raw.to_owned())))
}

pub fn set_toggle_pref(
    prefs: &mut Map<String, Value>,
    pref: &str,
    on: bool,
    stock: Value,
    toggled: Value,
    profile_dir: Option<&Path>,
) {
    if on {
        prefs.entry(pref.to_owned()).or_insert(toggled);
        return;
    }
    if let Some(saved) = saved_pref(profile_dir, pref) {
        if saved != stock {
            prefs.entry(pref.to_owned()).or_insert(stock);
        }
    }
}

fn physical_scale(config: &Map<String, Value>, prefs: &Map<String, Value>) -> f64 {
    if host_os_key() != "lin" {
        return 1.0;
    }
    let pref = prefs
        .get("layout.css.devPixelsPerPx")
        .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(-1.0);
    if pref > 0.0 {
        pref
    } else {
        config
            .get("window.devicePixelRatio")
            .and_then(Value::as_f64)
            .filter(|&d| d > 0.0)
            .unwrap_or(1.0)
    }
}

#[cfg(unix)]
fn disk_total_bytes(p: &Path) -> Option<u64> {
    let out = std::process::Command::new("df")
        .arg("-k")
        .arg(p)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().nth(1)?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb.saturating_mul(1024))
}

#[cfg(windows)]
fn disk_total_bytes(_p: &Path) -> Option<u64> {
    None
}

fn stock_profile_disk_capacity_kb() -> Option<u64> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let candidates = match host_os_key() {
        "win" => {
            let appdata = std::env::var("APPDATA").ok().map(PathBuf::from);
            vec![
                appdata.map(|a| a.join("Mozilla")).unwrap_or_else(|| home.clone()),
                home.clone(),
            ]
        }
        "mac" => vec![
            home.join("Library")
                .join("Application Support")
                .join("Firefox"),
            home.clone(),
        ],
        _ => vec![home.join(".mozilla"), home.clone()],
    };
    for cand in candidates {
        let mut probe = cand.as_path();
        while !probe.exists() {
            let Some(parent) = probe.parent() else {
                break;
            };
            probe = parent;
        }
        if let Some(total) = disk_total_bytes(probe) {
            if total > 0 {
                return Some(((total / 2) / 1024).min(INT32_MAX));
            }
        }
    }
    None
}

pub fn chunk_code_points(s: &str, size: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut start_byte = 0usize;
    for (count, (byte_idx, _)) in s.char_indices().enumerate() {
        if count > 0 && count.is_multiple_of(size) {
            out.push(s[start_byte..byte_idx].to_owned());
            start_byte = byte_idx;
        }
    }
    if start_byte < s.len() {
        out.push(s[start_byte..].to_owned());
    }
    out
}

pub fn get_pref_env_vars(prefs: &Map<String, Value>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if prefs.is_empty() {
        return out;
    }
    let data = py_json_dumps_ascii(&Value::Object(prefs.clone()));
    let chunk_size = if host_os_key() == "win" { 2047 } else { 32767 };
    for (i, chunk) in chunk_code_points(&data, chunk_size).into_iter().enumerate() {
        out.insert(format!("CAMOU_PREFS_{}", i + 1), chunk);
    }
    out
}

fn generate_fontconfig(
    fontconfig_path: &Path,
    executable_path: Option<&Path>,
    os_dir: &str,
) -> Result<String> {
    let fonts_dir = match executable_path {
        Some(exe) => exe
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("fonts"),
        None => get_path("fonts")?,
    };
    let os_key = match os_dir {
        "linux" => Some("lin"),
        "macos" => Some("mac"),
        "windows" => Some("win"),
        _ => None,
    };
    let mut scan_dirs = Vec::new();
    let groups_path = fonts_dir.join("groups.json");
    if let (Some(ok), Ok(raw)) = (os_key, fs::read_to_string(&groups_path)) {
        if let Ok(parsed) = serde_json::from_str::<Value>(&raw) {
            if let Some(arr) = parsed
                .get("readBy")
                .and_then(|r| r.get(ok))
                .and_then(Value::as_array)
            {
                for g in arr.iter().filter_map(Value::as_str) {
                    let cand = fonts_dir.join(g);
                    if cand.is_dir() {
                        scan_dirs.push(cand);
                    }
                }
            }
        }
    }
    if scan_dirs.is_empty() {
        let legacy = fonts_dir.join(os_dir);
        if legacy.is_dir() {
            scan_dirs.push(legacy);
        } else {
            scan_dirs.push(fonts_dir);
        }
    }
    let src_conf = fs::read_to_string(fontconfig_path.join("fonts.conf"))?;
    let replacement = scan_dirs
        .iter()
        .map(|d| format!("<dir>{}</dir>", d.display()))
        .collect::<Vec<_>>()
        .join("\n\t");
    let conf_content = src_conf.replace("<dir prefix=\"cwd\">fonts</dir>", &replacement);

    let cache_dir = install_dir().join("fontconfig");
    fs::create_dir_all(&cache_dir)?;
    let hash_hex = hex::encode(Sha256::digest(conf_content.as_bytes()));
    let short_hash = &hash_hex[..12];
    let runtime_conf = cache_dir.join(format!("fonts-{short_hash}.conf"));
    if !runtime_conf.exists() {
        fs::write(&runtime_conf, conf_content)?;
    }
    Ok(runtime_conf.display().to_string())
}

pub fn get_env_vars(
    config_map: &Map<String, Value>,
    user_agent_os: &str,
    executable_path: Option<&Path>,
) -> Result<BTreeMap<String, String>> {
    let mut env_vars = BTreeMap::new();
    let config_str = orjson_dumps(&Value::Object(config_map.clone()), false);
    let chunk_size = if host_os_key() == "win" { 2047 } else { 32767 };
    for (i, chunk) in chunk_code_points(&config_str, chunk_size)
        .into_iter()
        .enumerate()
    {
        env_vars.insert(format!("CAMOU_CONFIG_{}", i + 1), chunk);
    }

    if host_os_key() == "lin" {
        let os_dir = match user_agent_os {
            "lin" => "linux",
            "mac" => "macos",
            "win" => "windows",
            other => other,
        };
        let bundle_path = |part1: &str, part2: &str| -> Result<PathBuf> {
            match executable_path {
                Some(exe) => Ok(exe
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(part1)
                    .join(part2)),
                None => get_path(&format!("{part1}/{part2}")),
            }
        };
        let mut fc_path = bundle_path("fontconfig", os_dir)?;
        if !fc_path.join("fonts.conf").exists() {
            fc_path = bundle_path("fontconfigs", os_dir)?;
        }
        if !fc_path.join("fonts.conf").exists() {
            return Err(CamoufoxError::FileNotFound(format!(
                "fonts.conf not found in {}! Something ain't right with your camoufox bundle.",
                fc_path.display()
            )));
        }
        let runtime_conf = generate_fontconfig(&fc_path, executable_path, os_dir)?;
        env_vars.insert("FONTCONFIG_FILE".into(), runtime_conf);
    }
    Ok(env_vars)
}

pub fn warn_if_executable_predates_playwright(executable_path: Option<&Path>) {
    let Some(exe) = executable_path else {
        return;
    };
    let Some(dir) = exe.parent() else {
        return;
    };
    let Ok(installed) = Version::from_path(dir) else {
        return;
    };
    let required = effective_version_min();
    if installed < required {
        warn(
            &format!(
                "The Camoufox build at {} is {}, but Playwright {} needs at least {}. \
                 Contexts created with an explicit viewport will fail.",
                exe.display(),
                installed.build,
                resolved_playwright_version_str(),
                required.build
            ),
            "RuntimeWarning",
        );
    }
}

pub fn determine_ua_os(user_agent: &str) -> &'static str {
    let lower = user_agent.to_ascii_lowercase();
    if lower.contains("macintosh") || lower.contains("mac os x") || lower.contains("macos") {
        "mac"
    } else if lower.contains("windows") || lower.contains("win64") || lower.contains("win32") {
        "win"
    } else {
        "lin"
    }
}

pub fn get_target_os(config: &Map<String, Value>) -> &'static str {
    if let Some(ua) = config.get("navigator.userAgent").and_then(Value::as_str) {
        if !ua.is_empty() {
            return determine_ua_os(ua);
        }
    }
    host_os_key()
}

pub fn cpu_identity_pref(config: &Map<String, Value>) -> i64 {
    let arch = config.get("cpu:arch").and_then(Value::as_str);
    let maker = config.get("cpu:manufacturer");
    let table = &launcher_constants().cpu_identity_pref;
    if arch == Some("arm") {
        return table.get("arm").and_then(Value::as_i64).unwrap_or(3);
    }
    if arch.is_some() && arch != Some("x86") {
        return 0;
    }
    let key = format!("x86/{}", maker.map(py_str).unwrap_or_else(|| "None".into()));
    table.get(&key).and_then(Value::as_i64).unwrap_or(0)
}

pub fn vpx_realtime_only(config: &Map<String, Value>, target_os: &str) -> bool {
    config.get("cpu:arch").and_then(Value::as_str) == Some("arm")
        && (target_os == "win" || target_os == "lin")
}

pub fn check_valid_os(os_values: &[String]) -> Result<()> {
    for os_val in os_values {
        let is_lower = os_val.chars().any(|c| c.is_ascii_alphabetic())
            && os_val.as_str() == os_val.to_ascii_lowercase();
        if !is_lower {
            return Err(CamoufoxError::InvalidOS(format!(
                "OS values must be lowercase: '{os_val}'"
            )));
        }
        if !matches!(os_val.as_str(), "windows" | "macos" | "linux") {
            return Err(CamoufoxError::InvalidOS(format!(
                "Camoufox does not support the OS: '{os_val}'"
            )));
        }
    }
    Ok(())
}

pub fn os_for_cpu(cpu: &CpuOption, os_values: &[String]) -> Result<Vec<String>> {
    let asked: Vec<String> = if os_values.is_empty() {
        vec!["windows".into(), "macos".into(), "linux".into()]
    } else {
        os_values.to_vec()
    };
    check_valid_os(&asked)?;
    let mut errors = Vec::new();
    let mut fits = Vec::new();

    for name in &asked {
        let short = match name.as_str() {
            "windows" => "win",
            "macos" => "mac",
            _ => "lin",
        };
        let resolved = match coherence::resolve_cpu(
            cpu.arch.as_deref(),
            cpu.manufacturer.as_deref(),
            short,
        ) {
            Ok(r) => r,
            Err(e) => {
                errors.push(e.to_string());
                continue;
            }
        };
        if os_values.is_empty() {
            let gpus = firefox_gpus(short)?;
            let arch = resolved.get("cpu:arch").and_then(Value::as_str);
            let maker = resolved.get("cpu:manufacturer").and_then(Value::as_str);
            if !gpus
                .iter()
                .any(|(_, r)| coherence::gpu_fits_cpu(Some(r.as_str()), short, arch, maker))
            {
                errors.push(format!("fpgen has recorded no {name} GPU beside that CPU"));
                continue;
            }
        }
        fits.push(name.clone());
    }
    if fits.is_empty() {
        errors.sort();
        errors.dedup();
        return Err(CamoufoxError::ValueError(format!(
            "cpu: {}",
            errors.join("; ")
        )));
    }
    Ok(fits)
}

pub fn is_domain_set(config: &Map<String, Value>, properties: &[&str]) -> bool {
    for &prop in properties {
        if prop.ends_with('.') || prop.ends_with(':') {
            if config.keys().any(|k| k.starts_with(prop)) {
                return true;
            }
        } else if config.contains_key(prop) {
            return true;
        }
    }
    false
}

fn warn_manual_config(config: &Map<String, Value>) {
    if is_domain_set(config, &["navigator.language", "headers.Accept-Language", "locale:"]) {
        LeakWarning::warn("locale", Some(false));
    }
    if is_domain_set(config, &["geolocation:", "timezone"]) {
        LeakWarning::warn("geolocation", Some(false));
    }
    if is_domain_set(config, &["headers.User-Agent"]) {
        LeakWarning::warn("header-ua", Some(false));
    }
    if is_domain_set(config, &["navigator."]) {
        LeakWarning::warn("navigator", Some(false));
    }
    if is_domain_set(config, &["navigator.maxTouchPoints"]) {
        LeakWarning::warn("max_touch_points", Some(false));
    }
    if config.get("instantAnimations").is_some_and(py_truthy) {
        LeakWarning::warn("instant_animations", Some(false));
    }
    if is_domain_set(config, &["screen.", "window.", "document.body."]) {
        LeakWarning::warn("viewport", Some(false));
    }
}

fn warn_kept_incoherent(violations: &[Violation]) {
    let tmpl = load_warnings()
        .get("caller_value_incoherent")
        .cloned()
        .unwrap_or_else(|| "Kept incoherent caller keys ({keys}): {detail}".into());
    for v in violations {
        if v.kept.is_empty() {
            continue;
        }
        let keys = v.kept.join(", ");
        let msg = tmpl
            .replace("{keys}", &keys)
            .replace("{detail}", &v.detail);
        warn(&msg, "LeakWarning");
    }
}

fn update_fonts(config: &mut Map<String, Value>, target_os: &str) {
    let base_fonts = load_os_fonts()
        .get(target_os)
        .cloned()
        .unwrap_or_default();
    let mut set: BTreeSet<String> = base_fonts.into_iter().collect();
    if let Some(arr) = config.get("fonts").and_then(Value::as_array) {
        for f in arr.iter().filter_map(Value::as_str) {
            set.insert(f.to_owned());
        }
    }
    let mut sorted: Vec<String> = set.into_iter().collect();
    sorted.sort_by(|a, b| compare_py_str(a, b));
    config.insert("fonts".into(), Value::Array(sorted.into_iter().map(Value::String).collect()));
}

pub fn check_custom_fingerprint(fp: &Value) -> Result<()> {
    let ua = fp
        .get("navigator")
        .and_then(|n| n.get("userAgent"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !ua.contains("Firefox") {
        let browser = if ua.contains("Chrome") {
            "Chrome"
        } else if ua.contains("Safari") {
            "Safari"
        } else {
            "Other"
        };
        return Err(CamoufoxError::NonFirefoxFingerprint(format!(
            "\"{browser}\" fingerprints are not supported in Camoufox. \
             Using fingerprints from a browser other than Firefox WILL lead to detection. \
             If this is intentional, pass `i_know_what_im_doing: true`."
        )));
    }
    LeakWarning::warn("custom_fingerprint", Some(false));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn plan_rendering(
    target_os: &str,
    canvas_noise: Option<bool>,
    named_gpu: bool,
    webgl_disabled: bool,
    executable_path: &Path,
    headless: bool,
    env: &BTreeMap<String, String>,
    prefs: &Map<String, Value>,
) -> Result<(bool, Option<Gpu>)> {
    if canvas_noise == Some(true) {
        return Ok((true, None));
    }
    if target_os == host_os_key() && !named_gpu {
        let host = if webgl_disabled {
            None
        } else {
            host_gpu(executable_path, headless, env, prefs).await?
        };
        let Some(h) = host else {
            return Ok((false, None));
        };
        let recorded = firefox_gpus(target_os)?;
        if recorded.iter().any(|(v, r)| v == &h.0 && r == &h.1) {
            return Ok((false, Some(h)));
        }
    }
    Ok((canvas_noise.is_none(), None))
}

pub async fn host_identity(
    opts: &PreparedLaunchOptions,
    canvas_noise: Option<bool>,
) -> Result<Option<(String, Option<Gpu>)>> {
    if canvas_noise.is_some() || has_canvas_placeholder(&opts.firefox_user_prefs) {
        return Ok(None);
    }
    let host_os = host_os_key();
    let webgl_disabled = opts
        .firefox_user_prefs
        .get("webgl.disabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let (covered, gpu) = plan_rendering(
        host_os,
        None,
        false,
        webgl_disabled,
        &opts.executable_path,
        opts.headless,
        &opts.env,
        &opts.firefox_user_prefs,
    )
    .await?;
    if covered {
        Ok(None)
    } else {
        Ok(Some((host_os.to_owned(), gpu)))
    }
}

pub fn context_identity(
    host_ident: Option<&(String, Option<Gpu>)>,
    os: Option<&str>,
    preset: Option<&Value>,
) -> Result<(Option<String>, Option<Gpu>)> {
    let Some((host_short, gpu)) = host_ident else {
        return Ok((os.map(str::to_owned), None));
    };
    let host_name = short_os_to_full(host_short);
    let preset_plat = preset
        .and_then(|p| p.get("navigator"))
        .and_then(|n| n.get("platform"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let preset_gpu_v = preset
        .and_then(|p| p.get("webgl"))
        .and_then(|w| w.get("unmaskedVendor"))
        .and_then(Value::as_str);
    let preset_gpu_r = preset
        .and_then(|p| p.get("webgl"))
        .and_then(|w| w.get("unmaskedRenderer"))
        .and_then(Value::as_str);

    let os_mismatch = os.unwrap_or(host_name) != host_name;
    let preset_os_mismatch =
        preset.is_some_and(|_| target_os_from_platform(preset_plat) != host_name);
    let gpu_mismatch = gpu.as_ref().is_some_and(|(gv, gr)| {
        preset.is_some()
            && (preset_gpu_v != Some(gv.as_str()) || preset_gpu_r != Some(gr.as_str()))
    });

    if os_mismatch || preset_os_mismatch || gpu_mismatch {
        let gpu_desc = gpu
            .as_ref()
            .map(|g| format!(" and its GPU [\"{}\", \"{}\"]", g.0, g.1))
            .unwrap_or_default();
        return Err(CamoufoxError::ValueError(format!(
            "This browser renders its canvas as this {host_name} machine, so its contexts claim it{gpu_desc}. \
             Launch it with canvas_noise: true to give a context another OS or GPU."
        )));
    }
    Ok((Some(host_name.to_owned()), gpu.clone()))
}

pub fn camou_config_blob(env: &BTreeMap<String, String>) -> String {
    let mut parts: Vec<(usize, &str)> = env
        .iter()
        .filter_map(|(k, v)| {
            k.strip_prefix("CAMOU_CONFIG_")
                .and_then(|idx| idx.parse::<usize>().ok())
                .map(|i| (i, v.as_str()))
        })
        .collect();
    parts.sort_by_key(|(i, _)| *i);
    parts.into_iter().map(|(_, v)| v).collect()
}

pub fn launch_os(env: &BTreeMap<String, String>) -> &'static str {
    let blob = camou_config_blob(env);
    if blob.is_empty() {
        return short_os_to_full(host_os_key());
    }
    let parsed: Map<String, Value> = serde_json::from_str(&blob).unwrap_or_default();
    short_os_to_full(get_target_os(&parsed))
}

pub fn pinned_core_count(env: &BTreeMap<String, String>) -> Option<usize> {
    let blob = camou_config_blob(env);
    if blob.is_empty() || !cpu_affinity::supported() {
        return None;
    }
    let parsed: Map<String, Value> = serde_json::from_str(&blob).ok()?;
    let val = parsed.get("navigator.hardwareConcurrency")?.as_u64()? as usize;
    let host = cpu_affinity::host_core_count();
    if val >= 1 && val < host {
        Some(val)
    } else {
        None
    }
}

pub fn spoofs_window_dimensions(env: &BTreeMap<String, String>) -> bool {
    let blob = camou_config_blob(env);
    !blob.is_empty() && WINDOW_DIM_KEYS.iter().any(|k| blob.contains(k))
}

pub async fn start_virtual_display(
    opts: &mut CamoufoxOptions,
) -> Result<(Option<ActiveDisplay>, bool)> {
    let mut display = match opts.headless {
        HeadlessMode::VirtualGpu => {
            let gpu_dev = select_gpu(opts.gpu.as_deref(), None)?;
            let disp = GpuDisplay::new(
                gpu_dev,
                opts.debug,
                opts.require_gpu.unwrap_or(true),
            );
            for (k, v) in gpu_prefs() {
                opts.firefox_user_prefs.entry(k).or_insert(v);
            }
            ActiveDisplay::Gpu(disp)
        }
        HeadlessMode::Virtual => {
            if opts.gpu.is_some() || opts.require_gpu.is_some() {
                return Err(CamoufoxError::ValueError(
                    "gpu and require_gpu apply to headless='virtual-gpu' only".into(),
                ));
            }
            ActiveDisplay::Xvfb(VirtualDisplay::new(opts.debug, None, None)?)
        }
        HeadlessMode::Bool(b) => {
            if opts.gpu.is_some() || opts.require_gpu.is_some() {
                return Err(CamoufoxError::ValueError(
                    "gpu and require_gpu apply to headless='virtual-gpu' only".into(),
                ));
            }
            return Ok((None, b));
        }
    };

    let display_str = match &mut display {
        ActiveDisplay::Xvfb(v) => v.get()?,
        ActiveDisplay::Gpu(g) => g.get()?,
    };
    opts.virtual_display = Some(display_str);
    Ok((Some(display), false))
}

async fn keeps_ipv6(
    geo_config: &Map<String, Value>,
    geoip_db: Option<&str>,
    i_know_what_im_doing: bool,
) -> Result<bool> {
    let Some(ipv6) = public_ipv6().await else {
        return Ok(false);
    };
    let ipv6_cfg = match get_geolocation(&ipv6, geoip_db).await {
        Ok(g) => g.as_config()?,
        Err(CamoufoxError::UnknownIPLocation(_)) => Map::new(),
        Err(e) => return Err(e),
    };
    let same = ["timezone", "locale:region"]
        .iter()
        .all(|k| ipv6_cfg.get(*k) == geo_config.get(*k));
    if same {
        return Ok(true);
    }
    LeakWarning::warn("ipv6_elsewhere", Some(i_know_what_im_doing));
    Ok(false)
}

pub fn resolve_verstr(executable_path: Option<&Path>) -> Result<String> {
    let Some(exe) = executable_path else {
        return installed_verstr(None);
    };
    let ini = build_file(exe, "application.ini");
    let raw = fs::read_to_string(&ini)?;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("Version=") {
            return Ok(rest.trim().to_owned());
        }
    }
    Err(CamoufoxError::ValueError(format!(
        "{} names no Version",
        ini.display()
    )))
}

fn merge_into(target: &mut Map<String, Value>, source: Map<String, Value>) {
    for (k, v) in source {
        target.entry(k).or_insert(v);
    }
}

fn set_into(target: &mut Map<String, Value>, key: &str, value: Value) {
    target.entry(key.to_owned()).or_insert(value);
}

/// Resolves full Camoufox launch options (`executablePath`, `args`, `env`,
/// `firefoxUserPrefs`, `headless`, `proxy`) matching `utils.launch_options`.
pub async fn launch_options(opts: CamoufoxOptions) -> Result<PreparedLaunchOptions> {
    let mut config = opts.config.clone();
    let mut addons = opts.addons.clone();
    let args = opts.args.clone();
    let mut firefox_user_prefs = opts.firefox_user_prefs.clone();
    let mut target_os_opt = opts.os.clone();

    let headless_bool = match opts.headless {
        HeadlessMode::Bool(b) => b,
        HeadlessMode::Virtual | HeadlessMode::VirtualGpu => false,
    };

    let mut env: BTreeMap<String, String> = match opts.env {
        Some(e) => e,
        None => std::env::vars().collect(),
    };
    ensure_browser_profile_dir(Some(&env))?;

    let env_executable = std::env::var("CAMOUFOX_EXECUTABLE_PATH")
        .unwrap_or_default()
        .trim()
        .to_owned();
    let mut executable_path = opts.executable_path.clone();

    if let Some(ref browser_spec) = opts.browser {
        if executable_path.is_some() || !env_executable.is_empty() {
            let which = if executable_path.is_some() {
                "executable_path"
            } else {
                "CAMOUFOX_EXECUTABLE_PATH"
            };
            return Err(CamoufoxError::ValueError(format!(
                "browser '{browser_spec}' selects an installed build, but {which} names a binary too. Pass one of them."
            )));
        }
        executable_path = Some(selected_launch_path(browser_spec)?);
    } else if executable_path.is_none() && !env_executable.is_empty() {
        executable_path = Some(PathBuf::from(env_executable));
    }

    if let Some(ref p) = executable_path {
        if let Ok(abs) = fs::canonicalize(p) {
            executable_path = Some(abs);
        }
    }

    if let Some(ref stream_opts) = opts.headless_stream {
        apply_headless_stream_env(
            stream_opts,
            headless_bool,
            opts.virtual_display.as_deref(),
            &mut env,
        )?;
    }

    if let Some(ref vd) = opts.virtual_display {
        env.insert("DISPLAY".into(), vd.clone());
        env.insert("GDK_BACKEND".into(), "x11".into());
        env.remove("WAYLAND_DISPLAY");
        env.insert("MOZ_ENABLE_WAYLAND".into(), "0".into());
    }

    if !opts.i_know_what_im_doing {
        warn_manual_config(&config);
    }
    if let Some(ref cpu) = opts.cpu {
        target_os_opt = os_for_cpu(cpu, &target_os_opt)?;
    }

    let caller_keys: BTreeSet<String> = config.keys().cloned().collect();
    let user_set_navigator = is_domain_set(&config, &["navigator."]);
    let user_set_arch = is_domain_set(&config, &["navigator.platform", "navigator.oscpu"]);
    let user_set_screen_window = is_domain_set(&config, &["screen.", "window."]);
    let user_set_media_devices = is_domain_set(&config, &["mediaDevices:"]);
    let user_set_fonts = !opts.fonts.is_empty() || is_domain_set(&config, &["fonts"]);
    let user_set_voices = is_domain_set(&config, &["voices", "voices:speechd"]);
    let user_set_dnt = config.contains_key("navigator.doNotTrack");
    let user_set_gpc = config.contains_key("navigator.globalPrivacyControl");
    let user_set_accept_encoding = config.contains_key("headers.Accept-Encoding");
    drop_audio_seed(&mut config);

    let salt = if let Some(ref fp) = opts.fingerprint {
        identity_salt(Some(fp))
    } else if let Some(PresetOption::Custom(ref p)) = opts.fingerprint_preset {
        identity_salt(Some(p))
    } else if config.contains_key("navigator.userAgent") {
        identity_salt(Some(&Value::Object(config.clone())))
    } else {
        identity_salt(None)
    };

    if !target_os_opt.is_empty() {
        check_valid_os(&target_os_opt)?;
    } else if opts.webgl_config.is_some() {
        return Err(CamoufoxError::ValueError(
            "OS must be set when using webgl_config".into(),
        ));
    }

    if executable_path.is_none() {
        ensure_camoufox_installed().await?;
    }
    warn_if_package_outdated();

    let resolved_executable = match executable_path.as_ref() {
        Some(p) => p.clone(),
        None => launch_path(None)?,
    };

    add_default_addons(&mut addons, Some(&opts.exclude_addons), &resolved_executable);
    if !addons.is_empty() {
        confirm_paths(&addons)?;
        config.insert(
            "addons".into(),
            Value::Array(
                addons
                    .iter()
                    .map(|p| Value::String(p.display().to_string()))
                    .collect(),
            ),
        );
    }

    let ff_version_str = if let Some(ff) = opts.ff_version {
        LeakWarning::warn("ff_version", Some(opts.i_know_what_im_doing));
        ff.to_string()
    } else {
        let ver = resolve_verstr(executable_path.as_deref())?;
        ver.split('.').next().unwrap_or("159").to_owned()
    };

    let mut used_preset = false;
    let mut fingerprint = opts.fingerprint.clone();
    if let Some(ref fp) = fingerprint {
        if !opts.i_know_what_im_doing {
            check_custom_fingerprint(fp)?;
        }
    } else if let Some(ref preset_opt) = opts.fingerprint_preset {
        let maybe_preset = match preset_opt {
            PresetOption::Custom(v) => Some(v.clone()),
            PresetOption::Random => {
                get_random_preset(Some(&target_os_opt), Some(&ff_version_str))
            }
        };
        if let Some(preset) = maybe_preset {
            let preset_cfg = from_preset(&preset, Some(&ff_version_str), Some(salt));
            merge_into(&mut config, preset_cfg);
            used_preset = true;
        }
    }

    let screen_cons = if let Some(s) = opts.screen {
        Some(s)
    } else if has_display(&env) && !headless_bool {
        largest_display().map(|d| Screen::new(None, Some(d.width), None, Some(d.height)))
    } else {
        None
    };

    if !used_preset && fingerprint.is_none() {
        let _ = ensure_fpgen_model(false).await?;
        let fp = generate_fingerprint(
            opts.window,
            screen_cons.as_ref(),
            Some(&target_os_opt),
        )?;
        fingerprint = Some(fp);
    }

    if !used_preset {
        if let Some(ref fp) = fingerprint {
            merge_into(&mut config, from_fpgen(fp, Some(&ff_version_str)));
        }
    }

    let target_os = get_target_os(&config);
    if let Some(ref cpu) = opts.cpu {
        for (k, v) in coherence::resolve_cpu(
            cpu.arch.as_deref(),
            cpu.manufacturer.as_deref(),
            target_os,
        )? {
            config.insert(k, v);
        }
    }

    if !used_preset && !user_set_screen_window {
        if let Some(drawn_dpr) = fingerprint
            .as_ref()
            .and_then(|fp| fp.get("window"))
            .and_then(|w| w.get("devicePixelRatio"))
            .and_then(Value::as_f64)
        {
            if !config.contains_key("window.devicePixelRatio")
                && coherence::is_zoomed_ratio(Some(drawn_dpr), target_os)
            {
                let caller_scale = firefox_user_prefs
                    .get("layout.css.devPixelsPerPx")
                    .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
                    .unwrap_or(-1.0);
                config.insert("window.devicePixelRatio".into(), serde_json::json!(drawn_dpr));
                repair_device_pixel_ratio(
                    &mut config,
                    target_os,
                    if caller_scale > 0.0 {
                        Some(caller_scale)
                    } else {
                        None
                    },
                );
            }
        }
    }

    if !user_set_screen_window && screen_is_implausible(&config) {
        repair_screen_orientation(&mut config);
        raise_screen_to_modern_floor(&mut config);
    }

    if !user_set_arch {
        fix_navigator_arch(&mut config, target_os);
    }
    if !user_set_navigator {
        fix_hardware_concurrency(&mut config, Some(opts.pin_cpu_cores));
    }
    if !user_set_screen_window {
        if !used_preset {
            raise_screen_to_modern_floor(&mut config);
        }
        if !headless_bool && opts.virtual_display.is_none() {
            if let Some(ref sc) = screen_cons {
                let fit = physical_scale(&config, &firefox_user_prefs);
                clamp_screen_to_display(
                    &mut config,
                    sc.max_width.map(|w| ((w as f64) / fit) as i64),
                    sc.max_height.map(|h| ((h as f64) / fit) as i64),
                );
            }
        }
        follow_baseline_protection(&mut config, target_os);
        clamp_window_dimensions(&mut config);
        clamp_window_position(&mut config);
    }

    if !opts.fonts.is_empty() {
        config.insert(
            "fonts".into(),
            Value::Array(opts.fonts.iter().cloned().map(Value::String).collect()),
        );
    }

    if opts.custom_fonts_only {
        firefox_user_prefs.insert("gfx.bundled-fonts.activate".into(), Value::from(0));
        if !opts.fonts.is_empty() {
            LeakWarning::warn("custom_fonts_only", Some(opts.i_know_what_im_doing));
        } else {
            return Err(CamoufoxError::ValueError(
                "No custom fonts were passed, but `custom_fonts_only` is enabled.".into(),
            ));
        }
    } else if !user_set_fonts || !config.get("fonts").is_some_and(py_truthy) {
        let full_os = short_os_to_full(target_os);
        let native = (target_os == "mac" || target_os == "win") && host_os_key() == target_os;
        let seed = identity_seed(&config, salt) as u64;
        let drawn = generate_random_font_subset(full_os, Some(seed), native, None);
        if drawn.is_empty() {
            update_fonts(&mut config, target_os);
        } else {
            config.insert(
                "fonts".into(),
                Value::Array(drawn.into_iter().map(Value::String).collect()),
            );
        }
    }

    if !user_set_media_devices {
        set_media_devices_defaults(&mut config, salt);
    }

    if target_os == "win" {
        let win11_markers = windows11_marker_fonts();
        let is_win11 = config
            .get("fonts")
            .and_then(Value::as_array)
            .is_none_or(|arr| {
                arr.is_empty()
                    || arr
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|f| win11_markers.contains(f))
            });
        set_into(
            &mut firefox_user_prefs,
            "ui.useOverlayScrollbars",
            Value::from(i64::from(is_win11)),
        );
    } else {
        set_into(&mut firefox_user_prefs, "ui.useOverlayScrollbars", Value::from(1));
    }

    set_into(
        &mut firefox_user_prefs,
        "mousewheel.system_scroll_override.enabled",
        Value::Bool(target_os != "mac"),
    );

    if target_os == "win" && host_os_key() != "win" {
        set_into(
            &mut firefox_user_prefs,
            "javascript.options.use_ucrt_math",
            Value::Bool(true),
        );
        set_into(&mut config, "input:windowsKeyboard", Value::Bool(true));
        for (k, v) in windows_behaviour_prefs() {
            set_into(&mut firefox_user_prefs, k, v.clone());
        }
        let overlay = firefox_user_prefs
            .get("ui.useOverlayScrollbars")
            .and_then(Value::as_i64)
            .unwrap_or(1)
            != 0;
        set_into(
            &mut firefox_user_prefs,
            "widget.non-native-theme.scrollbar.style",
            Value::from(if overlay { 5 } else { 4 }),
        );
    }

    if target_os == "lin" {
        set_into(
            &mut firefox_user_prefs,
            "gfx.font_rendering.fallback.async",
            Value::Bool(false),
        );
    }

    if let Some(quota_kb) = stock_profile_disk_capacity_kb() {
        set_into(
            &mut firefox_user_prefs,
            QUOTA_FIXED_LIMIT_PREF,
            Value::from(quota_kb),
        );
    }

    if !user_set_dnt {
        config.remove("navigator.doNotTrack");
    }
    if !user_set_gpc {
        config.remove("navigator.globalPrivacyControl");
    }
    let dnt_enabled = config
        .get("navigator.doNotTrack")
        .filter(|v| !v.is_null())
        .is_some_and(|v| py_str(v) == "1");
    firefox_user_prefs.insert(
        "privacy.donottrackheader.enabled".into(),
        Value::Bool(dnt_enabled),
    );
    let gpc_enabled = config
        .get("navigator.globalPrivacyControl")
        .filter(|v| !v.is_null())
        .is_some_and(py_truthy);
    firefox_user_prefs.insert(
        "privacy.globalprivacycontrol.enabled".into(),
        Value::Bool(gpc_enabled),
    );

    if !user_set_accept_encoding {
        config.remove("headers.Accept-Encoding");
    }

    if let Some(ref geo_opt) = opts.geoip {
        geoip_allowed()?;
        let geo_ip = match geo_opt {
            GeoipOption::Auto => match &opts.proxy {
                Some(p) => {
                    let s = crate::ip::ProxyHelper::as_string(p)?;
                    public_ip(Some(&s)).await?
                }
                None => public_ip(None).await?,
            },
            GeoipOption::Ip(ip) => ip.clone(),
        };
        if !opts.block_webrtc {
            if valid_ipv4(&geo_ip) {
                set_into(&mut config, "webrtc:ipv4", Value::String(geo_ip.clone()));
            } else if valid_ipv6(&geo_ip) {
                set_into(&mut config, "webrtc:ipv6", Value::String(geo_ip.clone()));
            }
        }
        let geo = get_geolocation(&geo_ip, opts.geoip_db.as_deref()).await?;
        let geo_cfg = geo.as_config()?;
        for (k, v) in &geo_cfg {
            if matches!(
                k.as_str(),
                "timezone" | "locale:language" | "locale:region" | "locale:script"
            ) {
                set_into(&mut config, k, v.clone());
            } else {
                config.insert(k.clone(), v.clone());
            }
        }
        if opts.proxy.is_none()
            && valid_ipv4(&geo_ip)
            && !keeps_ipv6(&geo_cfg, opts.geoip_db.as_deref(), opts.i_know_what_im_doing).await?
        {
            config.insert("network:ipv4Only".into(), Value::Bool(true));
        }
    }

    if config.contains_key("geolocation:latitude") && config.contains_key("geolocation:longitude") {
        set_into(&mut firefox_user_prefs, "permissions.default.geo", Value::from(1));
    } else if let Some(ref p) = opts.proxy {
        if !p.server.contains("localhost") && !is_domain_set(&config, &["geolocation"]) {
            LeakWarning::warn("proxy_without_geoip", None);
        }
    }

    if !opts.locale.is_empty() {
        handle_locales(&opts.locale, &mut config)?;
    }

    let requested = if config.get("locale:language").is_some_and(py_truthy) {
        [
            config.get("locale:language"),
            config.get("locale:script"),
            config.get("locale:region"),
        ]
        .into_iter()
        .flatten()
        .filter(|v| py_truthy(v))
        .map(py_str)
        .collect::<Vec<_>>()
        .join("-")
    } else {
        "en-US".to_owned()
    };
    set_into(
        &mut firefox_user_prefs,
        "intl.locale.requested",
        Value::String(requested),
    );

    if let Some(region) = config.get("locale:region").filter(|v| py_truthy(v)) {
        set_into(
            &mut firefox_user_prefs,
            "browser.search.region",
            region.clone(),
        );
    }

    if !user_set_voices || (!config.contains_key("voices") && !config.contains_key("voices:speechd"))
    {
        let full_os = short_os_to_full(target_os);
        let voice_locale = if config.get("locale:language").is_some_and(py_truthy) {
            let joined = [config.get("locale:language"), config.get("locale:region")]
                .into_iter()
                .flatten()
                .filter(|v| py_truthy(v))
                .map(py_str)
                .collect::<Vec<_>>()
                .join("-");
            Some(joined)
        } else {
            config
                .get("navigator.language")
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let seed = identity_seed(&config, salt) as u64;
        match draw_voice_config(full_os, voice_locale.as_deref(), Some(seed)) {
            Ok(vm) => {
                for (k, v) in vm {
                    config.insert(k, v);
                }
            }
            Err(err) => {
                let ua_s = config.get("navigator.userAgent").and_then(Value::as_str);
                FallbackWarning::warn(
                    "Drawing the speech voices",
                    "no speech voices",
                    &err.to_string(),
                    ua_s,
                );
                config.insert("voices".into(), Value::Array(Vec::new()));
            }
        }
    }

    set_into(&mut config, "voices:blockIfNotDefined", Value::Bool(true));

    if let Some(ref h) = opts.humanize {
        let _ = normalize_humanize(h)?;
    }
    if opts.main_world_eval {
        set_into(&mut config, "allowMainWorld", Value::Bool(true));
    }
    if opts.allow_addon_new_tab {
        set_into(&mut config, "allowAddonNewtab", Value::Bool(true));
    }
    if opts.block_images {
        LeakWarning::warn("block_images", Some(opts.i_know_what_im_doing));
    }
    if opts.disable_coop {
        LeakWarning::warn("disable_coop", Some(opts.i_know_what_im_doing));
    }

    let profile_dir = opts.user_data_dir.as_deref();
    set_toggle_pref(
        &mut firefox_user_prefs,
        "permissions.default.image",
        opts.block_images,
        Value::from(1),
        Value::from(2),
        profile_dir,
    );
    set_toggle_pref(
        &mut firefox_user_prefs,
        "media.peerconnection.enabled",
        opts.block_webrtc,
        Value::Bool(true),
        Value::Bool(false),
        profile_dir,
    );
    set_toggle_pref(
        &mut firefox_user_prefs,
        "browser.tabs.remote.useCrossOriginOpenerPolicy",
        opts.disable_coop,
        Value::Bool(true),
        Value::Bool(false),
        profile_dir,
    );

    if opts.is_mobile {
        LeakWarning::warn("is_mobile", Some(opts.i_know_what_im_doing));
    }

    warn_kept_incoherent(&drop_incoherent_source_values(
        &mut config,
        target_os,
        &caller_keys,
    ));

    let has_named_gpu = opts.webgl_config.is_some()
        || (config.get("webGl:vendor").is_some_and(py_truthy)
            && config.get("webGl:renderer").is_some_and(py_truthy));
    let (cover_canvas, host) = plan_rendering(
        target_os,
        opts.canvas_noise,
        has_named_gpu,
        opts.block_webgl,
        &resolved_executable,
        headless_bool,
        &env,
        &firefox_user_prefs,
    )
    .await?;

    if cover_canvas {
        add_canvas_placeholder(&mut firefox_user_prefs);
        LeakWarning::warn("canvas_noise", Some(opts.i_know_what_im_doing));
    }

    if opts.block_webgl {
        firefox_user_prefs.insert("webgl.disabled".into(), Value::Bool(true));
        LeakWarning::warn("block_webgl", Some(opts.i_know_what_im_doing));
    } else {
        let _ = ensure_fpgen_model(false).await?;
        let seed = Some(identity_seed(&config, salt) as i64);
        let mut wgl_fp = if let Some((ref vendor, ref renderer)) = opts.webgl_config {
            webgl_for_gpu(target_os, vendor, renderer, seed)?
        } else if config.get("webGl:vendor").is_some_and(py_truthy)
            && config.get("webGl:renderer").is_some_and(py_truthy)
        {
            let v = config["webGl:vendor"].as_str().unwrap_or("");
            let r = config["webGl:renderer"].as_str().unwrap_or("");
            webgl_for_gpu(target_os, v, r, seed)?
        } else if let Some(ref h) = host {
            webgl_for_gpu(target_os, &h.0, &h.1, seed)?
        } else {
            let cpu_map: BTreeMap<String, Value> = config
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            sample_webgl_for_screen(
                target_os,
                config.get("screen.width").and_then(Value::as_i64),
                config.get("screen.height").and_then(Value::as_i64),
                seed,
                config.get("navigator.hardwareConcurrency").and_then(Value::as_i64),
                Some(&cpu_map),
            )?
        };
        let enable_webgl2 = wgl_fp
            .remove("webGl2Enabled")
            .unwrap_or(Value::Bool(true));
        merge_into(&mut config, wgl_fp);
        set_into(
            &mut firefox_user_prefs,
            "webgl.enable-webgl2",
            enable_webgl2,
        );
        set_into(
            &mut firefox_user_prefs,
            "webgl.force-enabled",
            Value::Bool(true),
        );
    }

    if opts.cpu.is_some() {
        if let Some(conflict) = cpu_conflict(&config, target_os) {
            return Err(CamoufoxError::ValueError(format!("cpu: {conflict}")));
        }
    }
    set_cpu_default(&mut config, target_os, salt);

    let incoherent = coherence::apply(&mut config, target_os, &caller_keys);
    warn_kept_incoherent(&incoherent);
    if opts.debug && !incoherent.is_empty() {
        for v in &incoherent {
            println!("Incoherent identity ({}): {}", v.rule, v.detail);
        }
    }

    let cpu_pref = cpu_identity_pref(&config);
    if cpu_pref != 0 && !firefox_user_prefs.contains_key("general.cpu_identity") {
        firefox_user_prefs.insert("general.cpu_identity".into(), Value::from(cpu_pref));
    }
    if vpx_realtime_only(&config, target_os)
        && !firefox_user_prefs.contains_key("media.libvpx.realtime_only")
    {
        firefox_user_prefs.insert("media.libvpx.realtime_only".into(), Value::Bool(true));
    }

    if let Some(dpr) = config.get("window.devicePixelRatio").and_then(Value::as_f64) {
        if dpr > 0.0 && !firefox_user_prefs.contains_key("layout.css.devPixelsPerPx") {
            firefox_user_prefs.insert(
                "layout.css.devPixelsPerPx".into(),
                Value::String(dpr.to_string()),
            );
        }
    }

    if !headless_bool && opts.virtual_display.is_none() {
        if let Some(ref sc) = screen_cons {
            let fit = physical_scale(&config, &firefox_user_prefs);
            for (axis, cap) in [("Width", sc.max_width), ("Height", sc.max_height)] {
                if let (Some(c), Some(sz)) = (
                    cap,
                    config
                        .get(&format!("window.outer{axis}"))
                        .and_then(Value::as_f64),
                ) {
                    if sz * fit > c as f64 {
                        return Err(CamoufoxError::ValueError(format!(
                            "window.outer{axis} {sz} at display scale {fit} needs {} pixels, \
                             but this monitor has {c}. Pass a smaller screen and window, or run headless.",
                            sz * fit
                        )));
                    }
                }
            }
        }
    }

    set_webgpu(
        &mut firefox_user_prefs,
        target_os,
        (
            config.get("webGl:vendor").and_then(Value::as_str),
            config.get("webGl:renderer").and_then(Value::as_str),
        ),
        host.as_ref(),
    );

    if opts.enable_cache {
        merge_into(&mut firefox_user_prefs, cache_prefs().clone());
    }

    if opts.debug {
        println!(
            "[DEBUG] Config:\n{}",
            serde_json::to_string_pretty(&config).unwrap_or_default()
        );
    }

    warn_if_executable_predates_playwright(executable_path.as_deref());
    validate_config(&config, executable_path.as_deref())?;

    let (launch_proxy, proxy_auth) = socks5_auth(opts.proxy.as_ref())?;

    let mut final_env = get_env_vars(&config, target_os, executable_path.as_deref())?;
    final_env.extend(get_pref_env_vars(&firefox_user_prefs));
    if let Some(ref auth) = proxy_auth {
        final_env.extend(proxy_auth_env(auth));
    }
    for (k, v) in env {
        final_env.insert(k, v);
    }

    Ok(PreparedLaunchOptions {
        executable_path: resolved_executable,
        args,
        env: final_env,
        firefox_user_prefs,
        headless: headless_bool,
        proxy: launch_proxy,
        target_os: short_os_to_full(target_os).to_owned(),
        config,
        timeout_ms: opts.timeout_ms,
        slow_mo_ms: opts.slow_mo_ms,
        downloads_path: opts.downloads_path,
    })
}
