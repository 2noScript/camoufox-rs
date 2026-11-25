//! Configuration property validation and `CAMOU_CONFIG_*` / `CAMOU_PREFS_*` serialization.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::exceptions::{CamoufoxError, Result};
use crate::paths::data_file_str;
use crate::pycompat::{py_repr, py_str, py_type_name};
use crate::warnings::LeakWarning;

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

    /// Returns the short OS key (`win`, `mac`, `lin`) used by launcher internals.
    pub const fn short_key(self) -> &'static str {
        match self {
            Self::Windows => "win",
            Self::MacOs => "mac",
            Self::Linux => "lin",
        }
    }

    /// Parses a target OS name (`windows`, `macos`, or `linux`).
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "windows" => Ok(Self::Windows),
            "macos" => Ok(Self::MacOs),
            "linux" => Ok(Self::Linux),
            other => Err(CamoufoxError::InvalidOS(format!(
                "Invalid OS '{other}'. Expected one of: windows, macos, linux."
            ))),
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

/// Low-level launch configuration for building Camoufox browser environment variables.
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

/// Property definition entry from `properties.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct PropertyEntry {
    pub property: String,
    #[serde(rename = "type")]
    pub prop_type: String,
    pub removed: Option<String>,
}

/// Loads `properties.json` from `executable_path` (next to binary or in
/// `Contents/Resources/` for macOS bundles), falling back to bundled `properties.json`.
pub fn load_properties(executable_path: Option<&Path>) -> HashMap<String, PropertyEntry> {
    let raw = executable_path
        .and_then(|exe| {
            let candidate = crate::pkgman::build_file(exe, "properties.json");
            fs::read_to_string(candidate).ok()
        })
        .unwrap_or_else(|| {
            data_file_str("properties.json")
                .expect("properties.json bundled")
                .to_owned()
        });
    let list: Vec<PropertyEntry> = serde_json::from_str(&raw).unwrap_or_default();
    list.into_iter()
        .map(|entry| (entry.property.clone(), entry))
        .collect()
}

/// Validates a single value against a `properties.json` type tag.
pub fn validate_type(value: &Value, expected_type: &str) -> bool {
    match expected_type {
        "str" => value.is_string(),
        "bool" => value.is_boolean(),
        "int" => value.as_i64().is_some() || value.is_boolean(),
        "uint" => {
            value.as_u64().is_some()
                || value.as_i64().is_some_and(|n| n >= 0)
                || value.is_boolean()
        }
        "double" => value.is_number() || value.is_boolean(),
        "array" => value.is_array(),
        "dict" => value.is_object(),
        _ => false,
    }
}

const VOICE_FIELDS: [&str; 5] = ["lang", "name", "voiceUri", "isDefault", "isLocalService"];

/// Validates that every `voices` entry is a complete voice object.
pub fn validate_voices(voices: &Value) -> Result<()> {
    let Some(arr) = voices.as_array() else {
        return Err(CamoufoxError::InvalidPropertyType(format!(
            "Invalid type for property voices. Expected array, got {}",
            py_type_name(voices)
        )));
    };
    for (index, voice) in arr.iter().enumerate() {
        let Some(obj) = voice.as_object() else {
            return Err(CamoufoxError::InvalidPropertyType(format!(
                "Invalid voices[{index}]: expected an object with {{{}}}, got {} ({}). Camoufox needs full voice objects, not names.",
                VOICE_FIELDS.join(", "),
                py_type_name(voice),
                py_repr(voice),
            )));
        };
        let missing: Vec<&str> = VOICE_FIELDS
            .iter()
            .copied()
            .filter(|field| !obj.contains_key(*field))
            .collect();
        if !missing.is_empty() {
            return Err(CamoufoxError::InvalidPropertyType(format!(
                "Invalid voices[{index}]: missing {}. Every voice needs {{{}}}.",
                missing.join(", "),
                VOICE_FIELDS.join(", "),
            )));
        }
    }
    Ok(())
}

/// Validates a `voices:speechd` grid: `{"languages": [[name, lang], ...], "variants": [name, ...]}`.
pub fn validate_speechd_voices(grid: &Value) -> Result<()> {
    let ok = grid.as_object().is_some_and(|obj| {
        let langs_ok = obj.get("languages").and_then(Value::as_array).is_some_and(|arr| {
            arr.iter().all(|item| {
                item.as_array()
                    .is_some_and(|pair| pair.len() == 2 && pair.iter().all(Value::is_string))
            })
        });
        let variants_ok = obj
            .get("variants")
            .and_then(Value::as_array)
            .is_some_and(|arr| arr.iter().all(Value::is_string));
        langs_ok && variants_ok
    });
    if !ok {
        return Err(CamoufoxError::InvalidPropertyType(
            "Invalid voices:speechd: expected {\"languages\": [[name, lang], ...], \"variants\": [name, ...]}"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Validates every property in `config_map` against `properties.json`.
pub fn validate_config(
    config_map: &Map<String, Value>,
    executable_path: Option<&Path>,
) -> Result<()> {
    let properties = load_properties(executable_path);
    for (key, value) in config_map {
        let Some(entry) = properties.get(key) else {
            if key == "voices:speechd" {
                LeakWarning::warn("voices_unread", None);
            }
            println!("Skipping unknown patch {key} : {}", py_str(value));
            continue;
        };
        if let Some(removed) = &entry.removed {
            println!("Skipping {key}: removed in {removed}, it has no effect");
            continue;
        }
        if !validate_type(value, &entry.prop_type) {
            return Err(CamoufoxError::InvalidPropertyType(format!(
                "Invalid type for property {key}. Expected {}, got {}",
                entry.prop_type,
                py_type_name(value)
            )));
        }
        if key == "voices:speechd" {
            validate_speechd_voices(value)?;
        } else if key == "voices" {
            validate_voices(value)?;
        }
    }
    Ok(())
}

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
pub fn launch_env(options: &LaunchOptions) -> Result<BTreeMap<String, String>> {
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
