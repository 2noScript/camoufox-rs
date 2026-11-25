//! Rust launcher SDK for the Camoufox anti-detect Firefox browser.
//!
//! Full Rust counterpart to `python/src/camoufox/` and `typescript/src/`, backed by
//! `fpgen-rs`, `myuri-rs`, and `playwright-rs`.
//!
//! ```no_run
//! use camoufox_rs::{camoufox, CamoufoxOptions, HeadlessMode};
//!
//! # async fn example() -> camoufox_rs::Result<()> {
//! let mut browser = camoufox(CamoufoxOptions {
//!     os: vec!["macos".into()],
//!     headless: HeadlessMode::Bool(true),
//!     ..Default::default()
//! }).await?;
//! let page = browser.new_page().await?;
//! page.goto("https://example.com", None).await?;
//! browser.close().await?;
//! # Ok(())
//! # }
//! ```

pub mod addons;
pub mod async_api;
pub mod browser_pin;
pub mod coherence;
pub mod config;
pub mod cpu_affinity;
pub mod display;
pub mod driver_defaults;
pub mod exceptions;
pub mod fingerprints;
pub mod fpgen_model;
pub mod geolocation;
pub mod gpudisplay;
pub mod host_rendering;
pub mod humanize;
pub mod humanize_playwright;
pub mod ip;
pub mod locales;
pub mod multiversion;
pub mod paths;
pub mod pkgman;
pub mod pycompat;
pub mod server;
pub mod sync_api;
pub mod utils;
pub mod version;
pub mod virtdisplay;
pub mod warnings;
pub mod webgl;

pub use addons::{add_default_addons, confirm_paths, get_addon_path, DefaultAddons};
pub use async_api::{camoufox, new_browser, new_context, CamoufoxBrowser, Headless, NewContextOptions};
pub use config::{
    chunk_env_vars, launch_env, validate_config, validate_type, LaunchOptions, TargetOs,
};
pub use exceptions::{CamoufoxError, Result};
pub use fingerprints::{generate_context_fingerprint, generate_fingerprint, Screen};
pub use fpgen_model::{ensure_fpgen_model, is_model_installed};
pub use humanize::{cursory, driven_channels, myuri, normalize as normalize_humanize, raw, Channel, Engine, HumanizeConfig, HumanizeSetting};
pub use humanize_playwright::{cursor_for_page, HumanizedPage};
pub use ip::{socks5_auth, validate_ip, ProxyAuth, ProxyConfig, ProxyHelper};
pub use locales::{handle_locale, handle_locales, normalize_locale, Geolocation, Locale};
pub use server::{launch_server, CamoufoxServer};
pub use sync_api::{camoufox as camoufox_sync, camoufox_async, camel_case, SyncCamoufox};
pub use utils::{
    check_valid_os, launch_options, os_for_cpu, CamoufoxOptions, CpuOption, GeoipOption,
    HeadlessMode, PreparedLaunchOptions, PresetOption,
};
pub use version::{Constraints, VERSION};
