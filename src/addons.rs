//! Addon resolution and default uBlock Origin management, matching `addons.ts` / `addons.py`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::exceptions::{CamoufoxError, Result};
use crate::pkgman::build_file;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DefaultAddons {
    #[serde(rename = "UBO")]
    Ubo,
}

impl DefaultAddons {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ubo => "UBO",
        }
    }

    pub const fn dir_name(self) -> &'static str {
        match self {
            Self::Ubo => "uBlock0@rayhill.net",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "UBO" => Some(Self::Ubo),
            _ => None,
        }
    }
}

/// Validates that each addon path exists and is a directory containing `manifest.json`.
pub fn confirm_paths(paths: &[PathBuf]) -> Result<()> {
    for addon_path in paths {
        if !addon_path.is_dir() {
            return Err(CamoufoxError::InvalidAddonPath(format!(
                "{} is not a directory (or does not exist)",
                addon_path.display()
            )));
        }
        if !addon_path.join("manifest.json").exists() {
            return Err(CamoufoxError::InvalidAddonPath(format!(
                "{} does not contain a manifest.json file",
                addon_path.display()
            )));
        }
    }
    Ok(())
}

/// Returns the bundled directory for `addon` next to `executable_path`.
pub fn get_addon_path(addon: DefaultAddons, executable_path: &Path) -> PathBuf {
    build_file(executable_path, &format!("addons/{}", addon.dir_name()))
}

/// Appends non-excluded default addons present next to `executable_path`.
pub fn add_default_addons(
    addons_list: &mut Vec<PathBuf>,
    exclude_list: Option<&[DefaultAddons]>,
    executable_path: &Path,
) {
    let excluded = exclude_list.unwrap_or(&[]);
    for addon in [DefaultAddons::Ubo] {
        if excluded.contains(&addon) {
            continue;
        }
        let addon_path = get_addon_path(addon, executable_path);
        if fs::metadata(&addon_path).is_ok_and(|m| m.is_dir()) {
            addons_list.push(addon_path);
        }
    }
}
