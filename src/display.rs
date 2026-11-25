//! Host display geometry in CSS pixels, matching `display.ts` / `display.py`.

use std::collections::BTreeMap;
use std::process::Command;

use regex::Regex;
use serde_json::Value;

use crate::paths::OS_NAME;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplaySize {
    pub width: u32,
    pub height: u32,
}

pub fn has_display(env: &BTreeMap<String, String>) -> bool {
    if OS_NAME != "lin" {
        return true;
    }
    env.get("DISPLAY").is_some_and(|s| !s.is_empty())
        || env.get("WAYLAND_DISPLAY").is_some_and(|s| !s.is_empty())
}

fn run_cmd(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

fn enumerate_monitors() -> Vec<DisplaySize> {
    match OS_NAME {
        "lin" => enumerate_linux(),
        "mac" => enumerate_mac(),
        _ => enumerate_windows(),
    }
}

fn enumerate_linux() -> Vec<DisplaySize> {
    let Some(out) = run_cmd("xrandr", &["--current"]) else {
        return Vec::new();
    };
    let connected_re = Regex::new(r"\bconnected\b").expect("regex");
    let geom_re = Regex::new(r"\b(\d+)x(\d+)\+\d+\+\d+").expect("regex");
    let mut monitors = Vec::new();
    for line in out.lines() {
        if !connected_re.is_match(line) {
            continue;
        }
        if let Some(caps) = geom_re.captures(line) {
            if let (Ok(w), Ok(h)) = (caps[1].parse::<u32>(), caps[2].parse::<u32>()) {
                monitors.push(DisplaySize {
                    width: w,
                    height: h,
                });
            }
        }
    }
    monitors
}

fn enumerate_mac() -> Vec<DisplaySize> {
    let Some(out) = run_cmd("system_profiler", &["-json", "SPDisplaysDataType"]) else {
        return Vec::new();
    };
    let Ok(data) = serde_json::from_str::<Value>(&out) else {
        return Vec::new();
    };
    let res_re = Regex::new(r"(\d+)\s*x\s*(\d+)").expect("regex");
    let mut monitors = Vec::new();
    for gpu in data
        .get("SPDisplaysDataType")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for display in gpu
            .get("spdisplays_ndrvs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let raw = display
                .get("_spdisplays_resolution")
                .or_else(|| display.get("spdisplays_resolution"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Some(caps) = res_re.captures(raw) {
                if let (Ok(w), Ok(h)) = (caps[1].parse::<u32>(), caps[2].parse::<u32>()) {
                    monitors.push(DisplaySize {
                        width: w,
                        height: h,
                    });
                }
            }
        }
    }
    monitors
}

fn enumerate_windows() -> Vec<DisplaySize> {
    let script = "Add-Type -AssemblyName System.Windows.Forms; \
                  [System.Windows.Forms.Screen]::AllScreens | \
                  ForEach-Object { \"$($_.Bounds.Width)x$($_.Bounds.Height)\" }";
    let Some(out) = run_cmd("powershell", &["-NoProfile", "-Command", script]) else {
        return Vec::new();
    };
    let line_re = Regex::new(r"^(\d+)x(\d+)$").expect("regex");
    let mut monitors = Vec::new();
    for line in out.lines() {
        if let Some(caps) = line_re.captures(line.trim()) {
            if let (Ok(w), Ok(h)) = (caps[1].parse::<u32>(), caps[2].parse::<u32>()) {
                monitors.push(DisplaySize {
                    width: w,
                    height: h,
                });
            }
        }
    }
    monitors
}

pub fn largest_display() -> Option<DisplaySize> {
    let monitors = enumerate_monitors();
    if monitors.is_empty() {
        return None;
    }
    let best = monitors
        .into_iter()
        .reduce(|prev, curr| {
            if (curr.width as u64) * (curr.height as u64)
                > (prev.width as u64) * (prev.height as u64)
            {
                curr
            } else {
                prev
            }
        })?;
    Some(DisplaySize {
        width: best.width.max(1),
        height: best.height.max(1),
    })
}
