//! `fpgen` Bayesian network model management, matching `fpgen_model.py`.

use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::exceptions::{CamoufoxError, Result};
use crate::paths::{configure_fpgen_model_dir, data_file_str};

pub const CUSTOM_MODEL_ENV: &str = "CAMOUFOX_FPGEN_DATA";

#[derive(Debug, Clone, Deserialize)]
pub struct FpgenModelPin {
    pub repo: String,
    pub tag: String,
    pub asset: String,
    pub sha256: String,
}

pub fn load_model_pin() -> Result<FpgenModelPin> {
    let raw = data_file_str("fpgen-model.json")?;
    serde_json::from_str(raw).map_err(|e| CamoufoxError::FpgenModelError(e.to_string()))
}

pub fn model_dir() -> PathBuf {
    configure_fpgen_model_dir();
    fpgen_rs::model_dir()
}

pub fn is_model_installed() -> bool {
    configure_fpgen_model_dir();
    fpgen_rs::is_model_installed(&fpgen_rs::model_dir(), &fpgen_rs::MODEL_PIN)
}

/// Ensures the pinned `fpgen` Bayesian network model is downloaded and unpacked.
pub async fn ensure_fpgen_model(force: bool) -> Result<PathBuf> {
    configure_fpgen_model_dir();
    let custom_env = std::env::var(CUSTOM_MODEL_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty());

    if let Some(custom_dir) = custom_env {
        let dir = PathBuf::from(custom_dir);
        if !dir.join("header.json").is_file() {
            return Err(CamoufoxError::FpgenModelError(format!(
                "{CUSTOM_MODEL_ENV}='{}' is missing header.json",
                dir.display()
            )));
        }
        return Ok(dir);
    }

    let pin = load_model_pin()?;
    let marker_file = fpgen_rs::model_dir().join(".pinned-model");

    let needs_install = force
        || !fpgen_rs::is_model_installed(&fpgen_rs::model_dir(), &fpgen_rs::MODEL_PIN)
        || fs::read_to_string(&marker_file).ok().as_deref().map(str::trim)
            != Some(pin.sha256.as_str());

    if needs_install {
        let opts = fpgen_rs::EnsureModelOptions {
            dir: None,
            force: true,
        };
        let path = tokio::task::spawn_blocking(move || fpgen_rs::ensure_model(opts))
            .await
            .map_err(|e| CamoufoxError::FpgenModelError(e.to_string()))??;
        Ok(path)
    } else {
        Ok(fpgen_rs::model_dir())
    }
}
