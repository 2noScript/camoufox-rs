//! Virtual Xvfb display manager for `headless="virtual"`, matching `virtdisplay.ts` / `virtdisplay.py`.

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::exceptions::{CamoufoxError, Result};
use crate::paths::OS_NAME;

pub const DEFAULT_SCREEN: &str = "1x1x24";
pub const SCREEN_ENV_VAR: &str = "CAMOUFOX_VIRTUAL_DISPLAY_SIZE";
pub const COMPOSITE_ENV_VAR: &str = "CAMOUFOX_VIRTUAL_DISPLAY_COMPOSITE";
const DISPLAYFD_TIMEOUT: Duration = Duration::from_secs(30);

pub fn resolve_screen() -> Result<String> {
    let value = std::env::var(SCREEN_ENV_VAR).unwrap_or_default();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(DEFAULT_SCREEN.to_owned());
    }
    let lower = trimmed.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split('x').collect();
    let valid = (parts.len() == 2 || parts.len() == 3)
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u32>().unwrap_or(0) > 0);
    if !valid {
        return Err(CamoufoxError::VirtualDisplayNotSupported(format!(
            "{SCREEN_ENV_VAR} must look like '1920x1080' or '1920x1080x24', got '{trimmed}'"
        )));
    }
    if parts.len() == 2 {
        Ok(format!("{lower}x24"))
    } else {
        Ok(lower)
    }
}

pub fn assert_linux() -> Result<()> {
    if OS_NAME != "lin" {
        return Err(CamoufoxError::VirtualDisplayNotSupported(
            "Virtual display is only supported on Linux.".into(),
        ));
    }
    Ok(())
}

fn which_cmd(name: &str) -> Option<PathBuf> {
    let out = Command::new("which").arg(name).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

#[derive(Debug)]
pub struct VirtualDisplay {
    pub debug: bool,
    pub screen: String,
    pub composite: bool,
    pub proc: Option<Child>,
    pub display: Option<u32>,
}

impl VirtualDisplay {
    pub fn new(debug: bool, screen: Option<String>, composite: Option<bool>) -> Result<Self> {
        let resolved_screen = match screen {
            Some(s) => s,
            None => resolve_screen()?,
        };
        let resolved_comp = composite.unwrap_or_else(|| {
            matches!(
                std::env::var(COMPOSITE_ENV_VAR)
                    .unwrap_or_else(|_| "0".into())
                    .trim(),
                "1" | "true"
            )
        });
        Ok(Self {
            debug,
            screen: resolved_screen,
            composite: resolved_comp,
            proc: None,
            display: None,
        })
    }

    pub fn xvfb_args(&self) -> Vec<String> {
        let comp_flag = if self.composite {
            "+extension"
        } else {
            "-extension"
        };
        [
            "-screen",
            "0",
            &self.screen,
            "-ac",
            "-nolisten",
            "tcp",
            "-extension",
            "RENDER",
            "+extension",
            "GLX",
            comp_flag,
            "COMPOSITE",
            "-extension",
            "XVideo",
            "-extension",
            "XVideo-MotionCompensation",
            "-extension",
            "XINERAMA",
            "-fp",
            "built-ins",
            "-nocursor",
            "-br",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    pub fn xvfb_path(&self) -> Result<PathBuf> {
        let path = which_cmd("Xvfb").ok_or_else(|| {
            CamoufoxError::CannotFindXvfb("Please install Xvfb to use headless mode.".into())
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = fs::metadata(&path).map_err(|_| {
                CamoufoxError::CannotExecuteXvfb(format!(
                    "I do not have permission to execute Xvfb: {}",
                    path.display()
                ))
            })?;
            if meta.permissions().mode() & 0o111 == 0 {
                return Err(CamoufoxError::CannotExecuteXvfb(format!(
                    "I do not have permission to execute Xvfb: {}",
                    path.display()
                )));
            }
        }
        Ok(path)
    }

    pub fn setpriv_path(&self) -> Result<PathBuf> {
        which_cmd("setpriv").ok_or_else(|| {
            CamoufoxError::CannotFindXvfb(
                "Please install util-linux (for setpriv) to use headless mode.".into(),
            )
        })
    }

    pub fn get(&mut self) -> Result<String> {
        assert_linux()?;
        if let Some(d) = self.display {
            return Ok(format!(":{d}"));
        }

        let setpriv = self.setpriv_path()?;
        let xvfb = self.xvfb_path()?;
        let mut cmd = Command::new(setpriv);
        cmd.arg("--pdeathsig")
            .arg("TERM")
            .arg(xvfb)
            .arg("-displayfd")
            .arg("1")
            .args(self.xvfb_args())
            .env("__GLX_VENDOR_LIBRARY_NAME", "mesa")
            .env("LIBGL_ALWAYS_SOFTWARE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(if self.debug {
                Stdio::inherit()
            } else {
                Stdio::null()
            });

        let mut child = cmd.spawn().map_err(|e| {
            CamoufoxError::CannotExecuteXvfb(format!("Could not start Xvfb: {e}"))
        })?;
        let mut stdout = child.stdout.take().ok_or_else(|| {
            CamoufoxError::CannotExecuteXvfb("Could not open Xvfb -displayfd pipe".into())
        })?;

        let deadline = Instant::now() + DISPLAYFD_TIMEOUT;
        let mut buf = String::new();
        let mut byte = [0u8; 1];
        while Instant::now() < deadline {
            match stdout.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    buf.push(byte[0] as char);
                    if byte[0] == b'\n' {
                        break;
                    }
                }
                Err(e) => {
                    let _ = child.kill();
                    return Err(CamoufoxError::CannotExecuteXvfb(format!(
                        "Failed to read Xvfb display: {e}"
                    )));
                }
            }
        }
        let parsed: u32 = buf.trim().parse().map_err(|_| {
            let _ = child.kill();
            CamoufoxError::CannotExecuteXvfb(format!("Xvfb wrote non-integer display: '{buf}'"))
        })?;
        self.proc = Some(child);
        self.display = Some(parsed);
        Ok(format!(":{parsed}"))
    }

    pub fn kill(&mut self) {
        if let Some(mut child) = self.proc.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(d) = self.display.take() {
            let _ = fs::remove_file(format!("/tmp/.X{d}-lock"));
            let _ = fs::remove_file(format!("/tmp/.X11-unix/X{d}"));
        }
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        self.kill();
    }
}
