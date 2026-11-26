//! Bundled data files (`DATA_FILES`), platform identifiers, and cache paths.

use std::path::PathBuf;
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::exceptions::{CamoufoxError, Result};

/// The 15 shared launcher data files + `properties.json` + `fpgen-model.json`.
pub const DATA_FILES: [&str; 15] = [
    "browser-pin.json",
    "essential-fonts.json",
    "fingerprint-presets.json",
    "fingerprint-presets-v150.json",
    "font-bases.json",
    "font-groups.json",
    "fonts.json",
    "fpgen.yml",
    "launcher-constants.json",
    "media-devices.json",
    "repos.yml",
    "territoryInfo.xml",
    "voice-manifests.json",
    "voice-uris.json",
    "warnings.yml",
];

pub fn data_file_str(name: &str) -> Result<&'static str> {
    match name {
        "browser-pin.json" => Ok(include_str!("../data-files/browser-pin.json")),
        "essential-fonts.json" => Ok(include_str!("../data-files/essential-fonts.json")),
        "fingerprint-presets.json" => Ok(include_str!("../data-files/fingerprint-presets.json")),
        "fingerprint-presets-v150.json" => {
            Ok(include_str!("../data-files/fingerprint-presets-v150.json"))
        }
        "font-bases.json" => Ok(include_str!("../data-files/font-bases.json")),
        "font-groups.json" => Ok(include_str!("../data-files/font-groups.json")),
        "fonts.json" => Ok(include_str!("../data-files/fonts.json")),
        "fpgen.yml" => Ok(include_str!("../data-files/fpgen.yml")),
        "fpgen-model.json" => Ok(include_str!("../data-files/fpgen-model.json")),
        "launcher-constants.json" => Ok(include_str!("../data-files/launcher-constants.json")),
        "media-devices.json" => Ok(include_str!("../data-files/media-devices.json")),
        "properties.json" => Ok(include_str!("../data-files/properties.json")),
        "repos.yml" => Ok(include_str!("../data-files/repos.yml")),
        "territoryInfo.xml" => Ok(include_str!("../data-files/territoryInfo.xml")),
        "voice-manifests.json" => Ok(include_str!("../data-files/voice-manifests.json")),
        "voice-uris.json" => Ok(include_str!("../data-files/voice-uris.json")),
        "warnings.yml" => Ok(include_str!("../data-files/warnings.yml")),
        other => Err(CamoufoxError::FileNotFound(format!(
            "Unknown bundled data file: {other}"
        ))),
    }
}

pub fn load_data_value(name: &str) -> Result<Value> {
    let raw = data_file_str(name)?;
    if name.ends_with(".yml") || name.ends_with(".yaml") {
        serde_yaml_ng::from_str(raw).map_err(|e| CamoufoxError::ValueError(e.to_string()))
    } else {
        serde_json::from_str(raw).map_err(|e| CamoufoxError::ValueError(e.to_string()))
    }
}

pub fn map_os_name(platform: &str) -> Option<&'static str> {
    match platform {
        "darwin" | "macos" => Some("mac"),
        "linux" => Some("lin"),
        "windows" => Some("win"),
        _ => None,
    }
}

pub fn map_arch_name(arch: &str) -> Option<&'static str> {
    match arch {
        "x64" | "amd64" | "x86" | "x86_64" => Some("x86_64"),
        "ia32" | "i686" | "i386" => Some("i686"),
        "arm64" | "aarch64" | "arm" => Some("arm64"),
        _ => None,
    }
}

pub const OS_NAME: &str = if cfg!(target_os = "macos") {
    "mac"
} else if cfg!(target_os = "windows") {
    "win"
} else {
    "lin"
};

pub fn host_arch() -> &'static str {
    map_arch_name(std::env::consts::ARCH).unwrap_or("x86_64")
}

pub fn user_cache_dir(app_name: &str) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var("LOCALAPPDATA")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("AppData")
                    .join("Local")
            });
        base.join(app_name).join(app_name).join("Cache")
    }
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library")
            .join("Caches")
            .join(app_name)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let base = std::env::var("XDG_CACHE_HOME")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".cache")
            });
        base.join(app_name)
    }
}

pub fn install_dir() -> PathBuf {
    user_cache_dir("uncamoufox")
}

pub fn configure_fpgen_model_dir() {
    let dir = std::env::var("CAMOUFOX_FPGEN_DATA")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| install_dir().join("fpgen"));
    fpgen_rs::set_model_dir(Some(dir));
}

pub fn launch_file_for(os_name: &str) -> &'static str {
    match os_name {
        "win" => "camoufox.exe",
        "mac" => "../MacOS/camoufox",
        _ => "camoufox-bin",
    }
}

pub fn supported_arches_for(os_name: &str) -> &'static [&'static str] {
    match os_name {
        "win" => &["x86_64", "i686"],
        "mac" => &["x86_64", "arm64"],
        _ => &["x86_64", "arm64", "i686"],
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LauncherConstants {
    pub marker_fonts: Map<String, Value>,
    pub windows11_marker_fonts: Vec<String>,
    pub plausible_core_counts: Vec<u32>,
    pub apple_silicon_cores: Vec<u32>,
    pub x86_intel_share: f64,
    pub cpu_identity_pref: Map<String, Value>,
    pub cpu_manufacturers: Map<String, Value>,
    pub intel_only_core_counts: Vec<u32>,
    pub apple_silicon_panels: Map<String, Value>,
    pub intel_mac_igp_cores: Vec<u32>,
    pub intel_mac_dgpu_extra_cores: Vec<u32>,
    pub plausible_dpr: Map<String, Value>,
    pub mac_novelty_voices: Vec<String>,
    pub mac_eloquence_voices: Vec<String>,
    pub baseline_taskbar_height: Map<String, Value>,
    pub baseline_multi_touch_points: u32,
    pub cache_prefs: Map<String, Value>,
    pub windows_behaviour_prefs: Map<String, Value>,
}

pub fn launcher_constants() -> &'static LauncherConstants {
    static CONSTANTS: OnceLock<LauncherConstants> = OnceLock::new();
    CONSTANTS.get_or_init(|| {
        serde_json::from_str(
            data_file_str("launcher-constants.json").expect("launcher-constants.json bundled"),
        )
        .expect("launcher-constants.json valid")
    })
}
