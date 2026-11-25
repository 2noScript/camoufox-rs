//! Browser build pinning (`data-files/browser-pin.json`), matching `browser_pin.py` / `browser-pin.ts`.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::paths::data_file_str;
use crate::warnings::warn;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserPin {
    pub repo_name: String,
    pub version: String,
    pub build: String,
}

impl BrowserPin {
    pub fn spec(&self) -> String {
        pin_spec(self)
    }

    pub fn tag(&self) -> String {
        pin_tag(self)
    }
}

pub fn pin_spec(pin: &BrowserPin) -> String {
    if pin.version.is_empty() {
        pin.build.clone()
    } else {
        format!("{}-{}", pin.version, pin.build)
    }
}

pub fn pin_tag(pin: &BrowserPin) -> String {
    format!("v{}", pin_spec(pin))
}

#[derive(Debug, Deserialize)]
struct RawBrowserPin {
    repo: String,
    version: Option<String>,
    build: String,
}

pub fn load_pin() -> Option<BrowserPin> {
    let raw = data_file_str("browser-pin.json").ok()?;
    let val: Value = serde_json::from_str(raw).ok()?;
    if val.is_null() {
        return None;
    }
    let parsed: RawBrowserPin = serde_json::from_value(val).ok()?;
    Some(BrowserPin {
        repo_name: parsed.repo,
        version: parsed.version.unwrap_or_default(),
        build: parsed.build,
    })
}

/// Returns the release pin unless the user explicitly selected a channel or pinned version.
pub fn effective_pin(config: &serde_json::Map<String, Value>) -> Option<BrowserPin> {
    let has_channel = config.get("channel").is_some_and(|v| !v.is_null() && v.as_str() != Some(""));
    let has_pinned = config.get("pinned").is_some_and(|v| !v.is_null() && v.as_str() != Some(""));
    if has_channel || has_pinned {
        return None;
    }
    load_pin()
}

pub fn pin_matches(pin: &BrowserPin, repo_name: &str, version: &str, build: &str) -> bool {
    repo_name.eq_ignore_ascii_case(&pin.repo_name)
        && build == pin.build
        && (pin.version.is_empty() || version == pin.version)
}

pub fn warn_if_unpaired(repo_name: &str, version: &str, build: &str) {
    static WARNED: OnceLock<std::sync::Mutex<BTreeSet<String>>> = OnceLock::new();
    let Some(pin) = load_pin() else {
        return;
    };
    if pin_matches(&pin, repo_name, version, build) {
        return;
    }
    let key = format!("{repo_name}/{version}-{build}");
    let cache = WARNED.get_or_init(|| std::sync::Mutex::new(BTreeSet::new()));
    if let Ok(mut guard) = cache.lock() {
        if !guard.insert(key) {
            return;
        }
    }
    let running = if version.is_empty() {
        build.to_owned()
    } else {
        format!("{version}-{build}")
    };
    warn(
        &format!(
            "Launching {repo_name} {running}; this camoufox release pairs with {} {}. \
             Run `camoufox set --release` to switch back.",
            pin.repo_name,
            pin.spec(),
        ),
        "UserWarning",
    );
}
