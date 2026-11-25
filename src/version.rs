//! Camoufox version and browser interface compatibility constraints.
//! Twin of `python/src/camoufox/__version__.py` and `typescript/src/__version__.ts`.

/// Package version aligned with `python/` and `typescript/`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Supported browser version bounds and protocol interface levels.
pub struct Constraints;

impl Constraints {
    pub const MIN_VERSION: &'static str = "alpha.1";
    pub const MAX_VERSION: &'static str = "1";

    /// `[playwright_version, min_browser_build]`: builds before `min_browser_build` reject newer Playwright fields.
    pub const PLAYWRIGHT_BROWSER_FLOORS: &'static [(&'static [u32], &'static str)] =
        &[(&[1, 61], "beta.30")];

    /// Highest browser interface supported by this library (`manifest.json` `"interface"`).
    pub const INTERFACE: u32 = 1;
    /// Lowest browser interface accepted by this library.
    pub const MIN_INTERFACE: u32 = 1;
}
