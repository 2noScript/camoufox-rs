use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

/// Chunk size used when splitting serialized config across `CAMOU_CONFIG_*` and
/// `CAMOU_PREFS_*` environment variables, matching `python/src/camoufox/utils.py`.
const CHUNK_SIZE_WINDOWS: usize = 2047;
const CHUNK_SIZE_UNIX: usize = 32767;

/// Supported target operating systems for Camoufox browser identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetOs {
    Windows,
    MacOs,
    Linux,
}

impl TargetOs {
    /// Returns the canonical lowercase OS identifier expected by Camoufox.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::MacOs => "macos",
            Self::Linux => "linux",
        }
    }

    /// Parses a target OS name (`windows`, `macos`, or `linux`).
    pub fn parse(value: &str) -> Result<Self, CamoufoxError> {
        match value {
            "windows" => Ok(Self::Windows),
            "macos" => Ok(Self::MacOs),
            "linux" => Ok(Self::Linux),
            other => Err(CamoufoxError::InvalidOs(other.to_owned())),
        }
    }
}

/// Optional screen dimension bounds when constraining a generated fingerprint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    pub min_width: Option<u32>,
    pub max_width: Option<u32>,
    pub min_height: Option<u32>,
    pub max_height: Option<u32>,
}

/// Launch configuration for building Camoufox browser environment variables.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LaunchOptions {
    /// Target OS identity (`windows`, `macos`, or `linux`).
    pub os: Option<TargetOs>,
    /// Screen dimension constraints.
    pub screen: Option<Screen>,
    /// Explicit path to a Camoufox binary (`camoufox` / `camoufox-bin`).
    pub executable_path: Option<PathBuf>,
    /// Whether to run headless.
    pub headless: bool,
    /// BCP 47 locale(s) to apply.
    pub locale: Vec<String>,
    /// Arbitrary `CAMOU_CONFIG` key/value overrides.
    pub config: Map<String, Value>,
    /// Startup Firefox preferences passed via `CAMOU_PREFS_*`.
    pub firefox_user_prefs: Map<String, Value>,
}

/// Errors produced while validating or serializing Camoufox launch options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CamoufoxError {
    InvalidOs(String),
    Serialization(String),
}

impl fmt::Display for CamoufoxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOs(os) => write!(
                f,
                "Invalid OS '{os}'. Expected one of: windows, macos, linux."
            ),
            Self::Serialization(msg) => write!(f, "Failed to serialize config: {msg}"),
        }
    }
}

impl std::error::Error for CamoufoxError {}

/// Splits `payload` into indexed `<prefix>_1`, `<prefix>_2`, ... environment
/// variables respecting platform environment variable length limits.
pub fn chunk_env_vars(prefix: &str, payload: &str) -> BTreeMap<String, String> {
    let chunk_size = if cfg!(windows) {
        CHUNK_SIZE_WINDOWS
    } else {
        CHUNK_SIZE_UNIX
    };
    let mut vars = BTreeMap::new();
    let mut index = 1usize;
    let mut start = 0usize;

    while start < payload.len() {
        let mut end = (start + chunk_size).min(payload.len());
        // Never split inside a multi-byte UTF-8 code point.
        while !payload.is_char_boundary(end) {
            end -= 1;
        }
        vars.insert(format!("{prefix}_{index}"), payload[start..end].to_owned());
        start = end;
        index += 1;
    }
    vars
}

/// Builds the `CAMOU_CONFIG_*` and `CAMOU_PREFS_*` environment variables for a
/// Camoufox browser launch from `options`.
pub fn launch_env(options: &LaunchOptions) -> Result<BTreeMap<String, String>, CamoufoxError> {
    let mut env = BTreeMap::new();
    let config_json = serde_json::to_string(&options.config)
        .map_err(|err| CamoufoxError::Serialization(err.to_string()))?;
    env.extend(chunk_env_vars("CAMOU_CONFIG", &config_json));

    if !options.firefox_user_prefs.is_empty() {
        let prefs_json = serde_json::to_string(&options.firefox_user_prefs)
            .map_err(|err| CamoufoxError::Serialization(err.to_string()))?;
        env.extend(chunk_env_vars("CAMOU_PREFS", &prefs_json));
    }

    Ok(env)
}
