//! Error types matching `python/src/camoufox/exceptions.py` and `typescript/src/exceptions.ts`.

use thiserror::Error;

/// Unified error hierarchy across `camoufox-rs`.
#[derive(Debug, Error)]
pub enum CamoufoxError {
    #[error("{0}")]
    FileNotFound(String),

    #[error("{0}")]
    UnsupportedVersion(String),

    #[error("{0}")]
    MissingRelease(String),

    #[error("{0}")]
    CorruptedDownload(String),

    #[error("{0}")]
    UnsupportedArchitecture(String),

    #[error("{0}")]
    UnsupportedOS(String),

    #[error("{0}")]
    InvalidPropertyType(String),

    #[error("{0}")]
    InvalidAddonPath(String),

    #[error("{0}")]
    LocaleError(String),

    #[error("{0}")]
    InvalidIP(String),

    #[error("{0}")]
    InvalidProxy(String),

    #[error("{0}")]
    UnknownIPLocation(String),

    #[error("{0}")]
    InvalidLocale(String),

    #[error("{0}")]
    UnknownTerritory(String),

    #[error("{0}")]
    UnknownLanguage(String),

    #[error("{0}")]
    NotInstalledGeoIPExtra(String),

    #[error("{0}")]
    NonFirefoxFingerprint(String),

    #[error("{0}")]
    InvalidOS(String),

    #[error("{0}")]
    VirtualDisplayError(String),

    #[error("{0}")]
    CannotFindXvfb(String),

    #[error("{0}")]
    CannotExecuteXvfb(String),

    #[error("{0}")]
    VirtualDisplayNotSupported(String),

    #[error("{0}")]
    GpuDisplayError(String),

    #[error("{0}")]
    NoGpuFound(String),

    #[error("{0}")]
    AmbiguousGpuSelection(String),

    #[error("{0}")]
    GpuNotFound(String),

    #[error("{0}")]
    CannotFindXorg(String),

    #[error("{0}")]
    CannotExecuteXorg(String),

    #[error("{0}")]
    GpuVtTakeover(String),

    #[error("{0}")]
    GpuSoftwareFallback(String),

    #[error("{0}")]
    CamoufoxNotInstalled(String),

    #[error("{0}")]
    ProfileDirectoryError(String),

    #[error("{0}")]
    FpgenModelError(String),

    #[error("{0}")]
    ValueError(String),

    #[error("Failed to serialize config: {0}")]
    Serialization(String),

    #[error(transparent)]
    Fpgen(#[from] fpgen_rs::FpgenError),

    #[error(transparent)]
    Myuri(#[from] myuri_rs::MyuriError),

    #[error(transparent)]
    Playwright(#[from] playwright_rs::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl CamoufoxError {
    pub fn invalid_locale_input(locale: &str) -> Self {
        Self::InvalidLocale(format!(
            "Invalid locale: '{locale}'. Must be either a region, language, language-region, or language-script-region."
        ))
    }

    pub fn is_virtual_display_error(&self) -> bool {
        matches!(
            self,
            Self::VirtualDisplayError(_)
                | Self::CannotFindXvfb(_)
                | Self::CannotExecuteXvfb(_)
                | Self::VirtualDisplayNotSupported(_)
                | Self::GpuDisplayError(_)
                | Self::NoGpuFound(_)
                | Self::AmbiguousGpuSelection(_)
                | Self::GpuNotFound(_)
                | Self::CannotFindXorg(_)
                | Self::CannotExecuteXorg(_)
                | Self::GpuVtTakeover(_)
                | Self::GpuSoftwareFallback(_)
        )
    }
}

pub type Result<T> = std::result::Result<T, CamoufoxError>;
