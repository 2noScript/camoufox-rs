//! Rust launcher SDK for the Camoufox anti-detect Firefox browser.
//!
//! Mirrors the core configuration and environment-variable protocol shared by
//! `python/` and `typescript/`.
//!
//! ```rust
//! use camoufox_rs::{launch_env, LaunchOptions, TargetOs};
//!
//! let mut opts = LaunchOptions {
//!     os: Some(TargetOs::MacOs),
//!     headless: true,
//!     ..Default::default()
//! };
//! opts.config.insert("navigator.platform".into(), "MacIntel".into());
//!
//! let env = launch_env(&opts).expect("valid launch options");
//! assert!(env.contains_key("CAMOU_CONFIG_1"));
//! ```

pub mod config;

pub use config::{
    chunk_env_vars, launch_env, CamoufoxError, LaunchOptions, Screen, TargetOs,
};

/// Package version matching `python/` and `typescript/`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_target_os_variants() {
        assert_eq!(TargetOs::parse("windows"), Ok(TargetOs::Windows));
        assert_eq!(TargetOs::parse("macos"), Ok(TargetOs::MacOs));
        assert_eq!(TargetOs::parse("linux"), Ok(TargetOs::Linux));
        assert_eq!(
            TargetOs::parse("android"),
            Err(CamoufoxError::InvalidOs("android".into()))
        );
    }

    #[test]
    fn chunk_env_vars_splits_and_preserves_utf8() {
        let payload = "a".repeat(40000);
        let chunks = chunk_env_vars("CAMOU_CONFIG", &payload);
        assert!(chunks.contains_key("CAMOU_CONFIG_1"));
        assert!(chunks.contains_key("CAMOU_CONFIG_2"));

        let reassembled: String = chunks.into_values().collect();
        assert_eq!(reassembled, payload);
    }

    #[test]
    fn launch_env_encodes_config_and_prefs() {
        let mut opts = LaunchOptions::default();
        opts.config.insert("debug".into(), json!(true));
        opts.firefox_user_prefs
            .insert("gfx.bundled-fonts.activate".into(), json!(1));

        let env = launch_env(&opts).unwrap();
        assert_eq!(env.get("CAMOU_CONFIG_1").map(String::as_str), Some(r#"{"debug":true}"#));
        assert_eq!(
            env.get("CAMOU_PREFS_1").map(String::as_str),
            Some(r#"{"gfx.bundled-fonts.activate":1}"#)
        );
    }
}
