//! Whole-identity coherence checks and repairs, matching `coherence.ts` / `coherence.py`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::exceptions::{CamoufoxError, Result};
use crate::paths::launcher_constants;
use crate::pycompat::{compare_py_str, is_py_int, num_f64, py_repr, py_str, py_truthy};

pub type Config = Map<String, Value>;

pub const MAX_PLAUSIBLE_TOUCH_POINTS: i64 = 10;
pub const BROWSER_CHROME_HEIGHT: i64 = 86;
pub const CPU_ARCHES: [&str; 2] = ["x86", "arm"];
pub const CPU_VENDORS: [&str; 2] = ["intel", "amd"];

const NOT_A_MAC_GPU: [&str; 2] = ["ANGLE", "llvmpipe"];
const INTEL_IGPUS: [&str; 4] = [
    "Intel(R) HD Graphics, or similar",
    "Intel(R) HD Graphics 400, or similar",
    "Intel(R) HD Graphics Direct3D11",
    "Intel(R) HD Graphics 400 Direct3D11",
];
const AMD_IGPUS: [&str; 1] = ["Radeon HD 3200 Graphics"];

const ARM_GPUS: [(&str, &[&str]); 8] = [
    ("Apple M", &["apple"]),
    ("Apple GPU", &["apple"]),
    ("Adreno", &["qualcomm"]),
    ("V3D", &["broadcom"]),
    ("VideoCore", &["broadcom"]),
    ("Mali", &["rockchip", "mediatek", "amlogic", "allwinner"]),
    ("Immortalis", &["mediatek"]),
    ("Tegra", &["nvidia"]),
];

const CSS_GEOMETRY_KEYS: [&str; 12] = [
    "screen.width",
    "screen.height",
    "screen.availWidth",
    "screen.availHeight",
    "screen.availLeft",
    "screen.availTop",
    "window.outerWidth",
    "window.outerHeight",
    "window.innerWidth",
    "window.innerHeight",
    "window.screenX",
    "window.screenY",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub rule: String,
    pub detail: String,
    pub kept: Vec<String>,
}

fn renderer_str(config: &Config) -> String {
    match config.get("webGl:renderer") {
        Some(v) if py_truthy(v) => py_str(v),
        _ => String::new(),
    }
}

fn is_apple_silicon(config: &Config) -> bool {
    renderer_str(config).contains("Apple M")
}

fn apple_silicon_cores() -> Vec<u32> {
    launcher_constants().apple_silicon_cores.clone()
}

fn plausible_dpr(target_os: &str) -> Vec<f64> {
    launcher_constants()
        .plausible_dpr
        .get(target_os)
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default()
}

fn intel_mac_igp_cores() -> Vec<u32> {
    launcher_constants().intel_mac_igp_cores.clone()
}

fn intel_mac_dgpu_cores() -> Vec<u32> {
    let mut out = launcher_constants().intel_mac_igp_cores.clone();
    out.extend(&launcher_constants().intel_mac_dgpu_extra_cores);
    out
}

fn is_apple_silicon_panel(width: Option<&Value>, height: Option<&Value>) -> bool {
    let (Some(w), Some(h)) = (width.and_then(num_f64), height.and_then(num_f64)) else {
        return false;
    };
    let needle = format!("{}x{}", w as i64, h as i64);
    launcher_constants()
        .apple_silicon_panels
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .any(|pair| {
            pair.as_array().is_some_and(|arr| {
                arr.len() == 2
                    && format!(
                        "{}x{}",
                        arr[0].as_i64().unwrap_or(0),
                        arr[1].as_i64().unwrap_or(0)
                    ) == needle
            })
        })
}

pub fn gpu_fits_os(renderer: Option<&str>, target_os: &str) -> bool {
    let Some(r) = renderer.filter(|s| !s.is_empty()) else {
        return true;
    };
    match target_os {
        "mac" => !NOT_A_MAC_GPU.iter().any(|bad| r.contains(bad)),
        "win" => r.starts_with("ANGLE"),
        "lin" => !r.contains("ANGLE") && !r.contains("Apple M"),
        _ => true,
    }
}

fn intel_mac_misfit(
    renderer: &str,
    cores: Option<&Value>,
    width: Option<&Value>,
    height: Option<&Value>,
) -> Option<String> {
    if renderer.is_empty() || renderer.contains("Apple M") {
        return None;
    }
    let allowed = if renderer.contains("Intel") {
        intel_mac_igp_cores()
    } else {
        intel_mac_dgpu_cores()
    };
    if let Some(c) = cores.filter(|v| is_py_int(v)) {
        let n = num_f64(c).unwrap_or(0.0) as u32;
        if !allowed.contains(&n) {
            return Some(format!(
                "{} with hardwareConcurrency {}; no Intel Mac with that GPU reports it",
                py_repr(&Value::String(renderer.to_owned())),
                py_str(c)
            ));
        }
    }
    if is_apple_silicon_panel(width, height) {
        return Some(format!(
            "{} behind a {}x{} panel, which only Apple Silicon Macs have",
            py_repr(&Value::String(renderer.to_owned())),
            width.map(py_str).unwrap_or_default(),
            height.map(py_str).unwrap_or_default()
        ));
    }
    None
}

pub fn gpu_fits_machine(
    renderer: Option<&str>,
    target_os: &str,
    cores: Option<i64>,
    width: Option<i64>,
    height: Option<i64>,
) -> bool {
    if target_os != "mac" {
        return true;
    }
    let Some(r) = renderer.filter(|s| !s.is_empty()) else {
        return true;
    };
    let c_val = cores.map(Value::from);
    let w_val = width.map(Value::from);
    let h_val = height.map(Value::from);
    intel_mac_misfit(r, c_val.as_ref(), w_val.as_ref(), h_val.as_ref()).is_none()
}

pub fn cpu_os(target_os: &str) -> &'static str {
    match target_os {
        "win" | "windows" => "windows",
        "mac" | "macos" => "mac",
        _ => "linux",
    }
}

pub fn cpu_manufacturers(target_os: &str, arch: &str) -> Vec<String> {
    launcher_constants()
        .cpu_manufacturers
        .get(cpu_os(target_os))
        .and_then(|v| v.get(arch))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Validates and expands a `cpu` specification `{arch, manufacturer}` for `target_os`.
pub fn resolve_cpu(
    arch: Option<&str>,
    manufacturer: Option<&str>,
    target_os: &str,
) -> Result<Map<String, Value>> {
    if let Some(a) = arch {
        if !CPU_ARCHES.contains(&a) {
            return Err(CamoufoxError::ValueError(format!(
                "cpu arch must be one of ('x86', 'arm'), not '{a}'"
            )));
        }
    }
    let os_name = cpu_os(target_os);
    let mut resolved_arch = arch.map(str::to_owned);

    if let Some(maker) = manufacturer {
        let matching_arches: Vec<&str> = CPU_ARCHES
            .iter()
            .copied()
            .filter(|a| cpu_manufacturers(os_name, a).iter().any(|m| m == maker))
            .collect();
        if matching_arches.is_empty() {
            let mut known = BTreeSet::new();
            for a in CPU_ARCHES {
                for m in cpu_manufacturers(os_name, a) {
                    known.insert(m);
                }
            }
            let list: Vec<String> = known.into_iter().collect();
            return Err(CamoufoxError::ValueError(format!(
                "no {os_name} machine has a CPU made by '{maker}'; on {os_name} it is one of {}",
                list.join(", ")
            )));
        }
        match &resolved_arch {
            None => resolved_arch = Some(matching_arches[0].to_owned()),
            Some(a) if !matching_arches.contains(&a.as_str()) => {
                return Err(CamoufoxError::ValueError(format!(
                    "no {os_name} machine has an {a} CPU made by '{maker}'; {os_name} {a} CPUs are made by {}",
                    cpu_manufacturers(os_name, a).join(", ")
                )));
            }
            _ => {}
        }
    } else if let Some(ref a) = resolved_arch {
        if cpu_manufacturers(os_name, a).is_empty() {
            return Err(CamoufoxError::ValueError(format!(
                "no {os_name} machine has an {a} CPU"
            )));
        }
    }

    let mut out = Map::new();
    if let Some(a) = resolved_arch {
        out.insert("cpu:arch".into(), Value::String(a));
    }
    if let Some(m) = manufacturer {
        out.insert("cpu:manufacturer".into(), Value::String(m.to_owned()));
    }
    Ok(out)
}

pub fn gpu_cpu(renderer: Option<&str>, target_os: &str) -> (Option<&'static str>, Vec<&'static str>) {
    let Some(r) = renderer.filter(|s| !s.is_empty()) else {
        return (None, Vec::new());
    };
    for &(marker, makers) in &ARM_GPUS {
        if r.contains(marker) {
            return (Some("arm"), makers.to_vec());
        }
    }
    if cpu_os(target_os) == "mac" {
        return (Some("x86"), vec!["intel"]);
    }
    if INTEL_IGPUS.iter().any(|igpu| r.contains(igpu)) {
        return (Some("x86"), vec!["intel"]);
    }
    if AMD_IGPUS.iter().any(|igpu| r.contains(igpu)) {
        return (Some("x86"), vec!["amd"]);
    }
    (None, Vec::new())
}

pub fn gpu_fits_cpu(
    renderer: Option<&str>,
    target_os: &str,
    arch: Option<&str>,
    maker: Option<&str>,
) -> bool {
    let (by_gpu, makers) = gpu_cpu(renderer, target_os);
    let mut resolved_arch = arch.map(str::to_owned);
    if resolved_arch.is_none() {
        if let Some(m) = maker {
            resolved_arch = CPU_ARCHES
                .iter()
                .copied()
                .find(|a| cpu_manufacturers(target_os, a).iter().any(|x| x == m))
                .map(str::to_owned);
        }
    }
    let Some(a) = resolved_arch else {
        return true;
    };
    if renderer.unwrap_or("").contains("llvmpipe") {
        return true;
    }
    let Some(gpu_arch) = by_gpu else {
        return a == "x86" && cpu_os(target_os) != "mac";
    };
    if gpu_arch != a {
        return false;
    }
    maker.is_none_or(|m| makers.is_empty() || makers.contains(&m))
}

pub fn cores_cpu_vendor(cores: Option<&Value>) -> Option<&'static str> {
    let c = cores.and_then(num_f64)? as u32;
    if launcher_constants().intel_only_core_counts.contains(&c) {
        Some("intel")
    } else {
        None
    }
}

pub fn hardware_cpu(config: &Config, target_os: &str) -> (Option<String>, Option<String>) {
    let r = renderer_str(config);
    let (arch, makers) = gpu_cpu(if r.is_empty() { None } else { Some(&r) }, target_os);
    if arch == Some("arm") {
        let maker = if makers.len() == 1 {
            Some(makers[0].to_owned())
        } else {
            None
        };
        return (Some("arm".to_owned()), maker);
    }
    let vendor = makers
        .first()
        .copied()
        .or_else(|| cores_cpu_vendor(config.get("navigator.hardwareConcurrency")));
    let final_arch = if vendor.is_some() || arch.is_some() {
        Some("x86".to_owned())
    } else {
        None
    };
    (final_arch, vendor.map(str::to_owned))
}

pub fn cpu_conflict(config: &Config, target_os: &str) -> Option<String> {
    let arch = config.get("cpu:arch").and_then(Value::as_str);
    let maker = config.get("cpu:manufacturer").and_then(Value::as_str);
    if arch.is_none() && maker.is_none() {
        return None;
    }
    if let Err(err) = resolve_cpu(arch, maker, target_os) {
        return Some(err.to_string());
    }
    let r = renderer_str(config);
    let claimed = [maker, arch]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    if !r.is_empty() && !gpu_fits_cpu(Some(&r), target_os, arch, maker) {
        return Some(format!(
            "CPU {claimed} beside {}",
            py_repr(&Value::String(r))
        ));
    }
    let cores = config.get("navigator.hardwareConcurrency");
    if let Some(by_cores) = cores_cpu_vendor(cores) {
        if arch.unwrap_or("x86") == "x86" {
            if let Some(m) = maker {
                if m != by_cores {
                    return Some(format!(
                        "CPU {claimed} with hardwareConcurrency {}, which only an Intel CPU reports",
                        cores.map(py_str).unwrap_or_default()
                    ));
                }
            }
        }
    }
    None
}

fn check_apple_silicon_cores(config: &Config, _target_os: &str) -> Option<String> {
    if !is_apple_silicon(config) {
        return None;
    }
    let cores = config.get("navigator.hardwareConcurrency")?;
    if is_py_int(cores) {
        let n = num_f64(cores).unwrap_or(0.0) as u32;
        if !apple_silicon_cores().contains(&n) {
            return Some(format!(
                "{} with hardwareConcurrency {}; Apple Silicon starts at 8",
                py_repr(&Value::String(renderer_str(config))),
                py_str(cores)
            ));
        }
    }
    None
}

fn repair_apple_silicon_cores(config: &mut Config, _target_os: &str) {
    let Some(cores) = config.get("navigator.hardwareConcurrency") else {
        return;
    };
    if !is_py_int(cores) {
        return;
    }
    let n = num_f64(cores).unwrap_or(0.0) as u32;
    let mut sorted = apple_silicon_cores();
    sorted.sort_unstable();
    let replacement = sorted
        .iter()
        .copied()
        .find(|&c| c >= n)
        .or_else(|| sorted.last().copied())
        .unwrap_or(8);
    config.insert(
        "navigator.hardwareConcurrency".into(),
        Value::from(replacement),
    );
}

fn check_gpu_matches_os(config: &Config, target_os: &str) -> Option<String> {
    let r = renderer_str(config);
    if r.is_empty() || gpu_fits_os(Some(&r), target_os) {
        return None;
    }
    match target_os {
        "mac" => Some(format!(
            "macOS identity with {}, which no Mac reports",
            py_repr(&Value::String(r))
        )),
        "win" => Some(format!(
            "Windows identity with {}; Firefox on Windows renders through ANGLE",
            py_repr(&Value::String(r))
        )),
        _ => Some(format!(
            "Linux identity with {}",
            py_repr(&Value::String(r))
        )),
    }
}

fn check_intel_mac_hardware(config: &Config, target_os: &str) -> Option<String> {
    if target_os != "mac" {
        return None;
    }
    intel_mac_misfit(
        &renderer_str(config),
        config.get("navigator.hardwareConcurrency"),
        config.get("screen.width"),
        config.get("screen.height"),
    )
}

fn in_color_depths(depth: Option<&Value>) -> bool {
    let Some(d) = depth else {
        return false;
    };
    if d.is_boolean() {
        return false;
    }
    let Some(n) = num_f64(d) else {
        return false;
    };
    n == 24.0 || n == 30.0
}

fn check_color_depth(config: &Config, target_os: &str) -> Option<String> {
    let depth = config.get("screen.colorDepth").filter(|v| !v.is_null())?;
    if !in_color_depths(Some(depth)) {
        return Some(format!(
            "screen.colorDepth {}; Firefox reports 24 or 30",
            py_str(depth)
        ));
    }
    if target_os == "mac" && num_f64(depth) != Some(30.0) {
        return Some(format!(
            "macOS identity with colorDepth {}; deep colour is the macOS default",
            py_str(depth)
        ));
    }
    None
}

fn repair_color_depth(config: &mut Config, target_os: &str) {
    if target_os == "mac" {
        config.insert("screen.colorDepth".into(), Value::from(30));
    } else if !in_color_depths(config.get("screen.colorDepth")) {
        config.insert("screen.colorDepth".into(), Value::from(24));
    }
    if !config.contains_key("screen.pixelDepth") {
        if let Some(cd) = config.get("screen.colorDepth").cloned() {
            config.insert("screen.pixelDepth".into(), cd);
        }
    }
}

fn check_pixel_depth(config: &Config, _target_os: &str) -> Option<String> {
    let pixel = config.get("screen.pixelDepth").filter(|v| !v.is_null())?;
    let color = config
        .get("screen.colorDepth")
        .and_then(num_f64)
        .unwrap_or(24.0);
    if num_f64(pixel) != Some(color) {
        return Some(format!(
            "screen.pixelDepth {} differs from colorDepth {}",
            py_str(pixel),
            color as i64
        ));
    }
    None
}

fn repair_pixel_depth(config: &mut Config, _target_os: &str) {
    let val = config
        .get("screen.colorDepth")
        .cloned()
        .unwrap_or_else(|| Value::from(24));
    config.insert("screen.pixelDepth".into(), val);
}

fn check_touch_points(config: &Config, target_os: &str) -> Option<String> {
    let touch = config
        .get("navigator.maxTouchPoints")
        .filter(|v| !v.is_null())?;
    let n = num_f64(touch).unwrap_or(-1.0) as i64;
    if !is_py_int(touch) || !(0..=MAX_PLAUSIBLE_TOUCH_POINTS).contains(&n) {
        return Some(format!(
            "navigator.maxTouchPoints {}; a digitiser reports at most {MAX_PLAUSIBLE_TOUCH_POINTS}",
            py_str(touch)
        ));
    }
    if target_os == "mac" && py_truthy(touch) {
        return Some(format!(
            "macOS identity with maxTouchPoints {}; no Mac has a touchscreen",
            py_str(touch)
        ));
    }
    None
}

fn repair_touch_points(config: &mut Config, target_os: &str) {
    let touch = config.get("navigator.maxTouchPoints");
    let too_many = touch
        .filter(|v| is_py_int(v))
        .and_then(num_f64)
        .is_some_and(|n| n as i64 > MAX_PLAUSIBLE_TOUCH_POINTS);
    let non_int = touch.is_some_and(|v| !v.is_null() && !is_py_int(v));
    if target_os == "mac" || too_many || non_int {
        config.insert("navigator.maxTouchPoints".into(), Value::from(0));
    }
}

fn round_half_even(v: f64) -> i64 {
    let r = v.round();
    if (v.fract().abs() - 0.5).abs() < 1e-12 && (r as i64).rem_euclid(2) != 0 {
        (r as i64) - 1
    } else {
        r as i64
    }
}

pub fn is_zoomed_ratio(dpr: Option<f64>, target_os: &str) -> bool {
    let Some(val) = dpr.filter(|&x| x > 0.0) else {
        return false;
    };
    let allowed = plausible_dpr(target_os);
    !allowed.is_empty() && !allowed.iter().any(|&a| (a - val).abs() < 1e-12)
}

fn check_device_pixel_ratio(config: &Config, target_os: &str) -> Option<String> {
    let dpr = config
        .get("window.devicePixelRatio")
        .filter(|v| !v.is_null())?;
    let val = num_f64(dpr)?;
    let allowed = plausible_dpr(target_os);
    if !allowed.is_empty() && !allowed.iter().any(|&a| (a - val).abs() < 1e-12) {
        return Some(format!(
            "window.devicePixelRatio {} is not a display mode {target_os} offers",
            py_str(dpr)
        ));
    }
    None
}

pub fn repair_device_pixel_ratio(config: &mut Config, target_os: &str, target: Option<f64>) {
    let Some(x) = config.get("window.devicePixelRatio").and_then(num_f64) else {
        return;
    };
    let allowed = plausible_dpr(target_os);
    if allowed.is_empty() {
        return;
    }
    let mut best = allowed[0];
    for &v in &allowed[1..] {
        if (v - x).abs() < (best - x).abs() {
            best = v;
        }
    }
    if let Some(t) = target.filter(|&t| t > 0.0) {
        best = t;
    }
    config.insert("window.devicePixelRatio".into(), serde_json::json!(best));
    let scale = x / best;
    for key in CSS_GEOMETRY_KEYS {
        if let Some(v) = config.get(key).and_then(num_f64) {
            config.insert(key.into(), Value::from(round_half_even(v * scale)));
        }
    }
}

fn check_window_chrome(config: &Config, _target_os: &str) -> Option<String> {
    let inner = config.get("window.innerHeight").filter(|v| py_truthy(v))?;
    let outer = config.get("window.outerHeight").filter(|v| py_truthy(v))?;
    let chrome = (num_f64(outer)? - num_f64(inner)?) as i64;
    if chrome < BROWSER_CHROME_HEIGHT {
        return Some(format!(
            "window.outerHeight {} - innerHeight {} = {chrome}, less than the {BROWSER_CHROME_HEIGHT}px of chrome the window actually has; the bottom {}px of the claimed viewport cannot receive input",
            py_str(outer),
            py_str(inner),
            BROWSER_CHROME_HEIGHT - chrome
        ));
    }
    None
}

fn repair_window_chrome(config: &mut Config, _target_os: &str) {
    let Some(inner) = config
        .get("window.innerHeight")
        .filter(|v| py_truthy(v))
        .and_then(num_f64)
        .map(|n| n as i64)
    else {
        return;
    };
    let Some(outer) = config
        .get("window.outerHeight")
        .filter(|v| py_truthy(v))
        .and_then(num_f64)
        .map(|n| n as i64)
    else {
        return;
    };
    let avail = config
        .get("screen.availHeight")
        .filter(|v| py_truthy(v))
        .or_else(|| config.get("screen.height"))
        .and_then(num_f64)
        .map(|n| n as i64);
    if avail.is_none_or(|a| inner + BROWSER_CHROME_HEIGHT <= a) {
        config.insert(
            "window.outerHeight".into(),
            Value::from(inner + BROWSER_CHROME_HEIGHT),
        );
    } else {
        config.insert(
            "window.innerHeight".into(),
            Value::from((outer - BROWSER_CHROME_HEIGHT).max(1)),
        );
    }
}

fn check_screen_shape(config: &Config, _target_os: &str) -> Option<String> {
    let w = config
        .get("screen.width")
        .filter(|v| py_truthy(v))
        .and_then(num_f64)?;
    let h = config
        .get("screen.height")
        .filter(|v| py_truthy(v))
        .and_then(num_f64)?;
    if h > w {
        return Some(format!(
            "portrait screen {}x{}; desktop panels are landscape",
            w as i64, h as i64
        ));
    }
    if (w as i64) < 1024 {
        return Some(format!(
            "screen {}x{} is smaller than any current desktop panel",
            w as i64, h as i64
        ));
    }
    None
}

fn check_avail_bounds(config: &Config, _target_os: &str) -> Option<String> {
    let w = config.get("screen.width").filter(|v| py_truthy(v));
    let h = config.get("screen.height").filter(|v| py_truthy(v));
    let aw = config.get("screen.availWidth").filter(|v| py_truthy(v));
    let ah = config.get("screen.availHeight").filter(|v| py_truthy(v));
    if let (Some(wv), Some(awv)) = (w.and_then(num_f64), aw.and_then(num_f64)) {
        if awv > wv {
            return Some(format!(
                "screen.availWidth {} exceeds screen.width {}",
                aw.map(py_str).unwrap_or_default(),
                w.map(py_str).unwrap_or_default()
            ));
        }
    }
    if let (Some(hv), Some(ahv)) = (h.and_then(num_f64), ah.and_then(num_f64)) {
        if ahv > hv {
            return Some(format!(
                "screen.availHeight {} exceeds screen.height {}",
                ah.map(py_str).unwrap_or_default(),
                h.map(py_str).unwrap_or_default()
            ));
        }
    }
    None
}

fn repair_avail_bounds(config: &mut Config, _target_os: &str) {
    if let (Some(w), Some(aw)) = (
        config.get("screen.width").filter(|v| py_truthy(v)).cloned(),
        config.get("screen.availWidth").and_then(num_f64),
    ) {
        if num_f64(&w).is_some_and(|wv| aw > wv) {
            config.insert("screen.availWidth".into(), w);
        }
    }
    if let (Some(h), Some(ah)) = (
        config.get("screen.height").filter(|v| py_truthy(v)).cloned(),
        config.get("screen.availHeight").and_then(num_f64),
    ) {
        if num_f64(&h).is_some_and(|hv| ah > hv) {
            config.insert("screen.availHeight".into(), h);
        }
    }
}

fn check_arch_agreement(config: &Config, _target_os: &str) -> Option<String> {
    let get_s = |k: &str| {
        config
            .get(k)
            .filter(|v| py_truthy(v))
            .map(py_str)
            .unwrap_or_default()
    };
    let ua = get_s("navigator.userAgent");
    if ua.is_empty() {
        return None;
    }
    let plat = get_s("navigator.platform");
    let oscpu = get_s("navigator.oscpu");
    if ua.contains("x86_64") && (plat.contains("armv") || oscpu.contains("armv")) {
        return Some(format!(
            "user agent claims x86_64 while platform/oscpu say '{}'/ '{}'",
            plat, oscpu
        ));
    }
    None
}

fn check_cpu(config: &Config, target_os: &str) -> Option<String> {
    cpu_conflict(config, target_os)
}

fn repair_cpu(config: &mut Config, target_os: &str) {
    let (implied, maker) = hardware_cpu(config, target_os);
    let arch = implied.unwrap_or_else(|| "x86".to_owned());
    config.insert("cpu:arch".into(), Value::String(arch.clone()));
    if let Some(m) = maker {
        config.insert("cpu:manufacturer".into(), Value::String(m));
    } else {
        let current_maker = config
            .get("cpu:manufacturer")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let valid_maker = current_maker
            .as_deref()
            .is_some_and(|m| cpu_manufacturers(target_os, &arch).iter().any(|x| x == m));
        if !valid_maker || cpu_conflict(config, target_os).is_some() {
            config.remove("cpu:manufacturer");
        }
    }
}

pub fn screen_is_implausible(config: &Config) -> bool {
    check_screen_shape(config, "").is_some()
}

pub fn repair_screen_orientation(config: &mut Config) -> bool {
    let Some(w) = config.get("screen.width").filter(|v| py_truthy(v)).cloned() else {
        return false;
    };
    let Some(h) = config.get("screen.height").filter(|v| py_truthy(v)).cloned() else {
        return false;
    };
    if num_f64(&h) <= num_f64(&w) {
        return false;
    }
    config.insert("screen.width".into(), h);
    config.insert("screen.height".into(), w);
    if let (Some(aw), Some(ah)) = (
        config.get("screen.availWidth").filter(|v| py_truthy(v)).cloned(),
        config.get("screen.availHeight").filter(|v| py_truthy(v)).cloned(),
    ) {
        config.insert("screen.availWidth".into(), ah);
        config.insert("screen.availHeight".into(), aw);
    }
    true
}

pub fn drop_incoherent_source_values(
    config: &mut Config,
    target_os: &str,
    keep: &BTreeSet<String>,
) -> Vec<Violation> {
    let r = renderer_str(config);
    if r.is_empty() {
        return Vec::new();
    }
    let rule = if !gpu_fits_os(Some(&r), target_os) {
        "gpu-matches-os"
    } else if check_intel_mac_hardware(config, target_os).is_some() {
        "intel-mac-hardware"
    } else {
        return Vec::new();
    };
    let mut kept: Vec<String> = ["webGl:renderer", "webGl:vendor"]
        .iter()
        .filter(|&&k| keep.contains(k) && config.contains_key(k))
        .map(|s| (*s).to_owned())
        .collect();
    kept.sort_by(|a, b| compare_py_str(a, b));
    if !kept.is_empty() {
        return vec![Violation {
            rule: rule.to_owned(),
            detail: format!(
                "{} does not fit this {target_os} identity",
                py_repr(&Value::String(r))
            ),
            kept,
        }];
    }
    config.remove("webGl:renderer");
    config.remove("webGl:vendor");
    vec![Violation {
        rule: rule.to_owned(),
        detail: format!(
            "dropped {} for a {target_os} identity",
            py_repr(&Value::String(r))
        ),
        kept: Vec::new(),
    }]
}

struct Rule {
    name: &'static str,
    check: fn(&Config, &str) -> Option<String>,
    repair: Option<fn(&mut Config, &str)>,
    adjusts: &'static [&'static str],
}

const RULES: [Rule; 12] = [
    Rule {
        name: "apple-silicon-cores",
        check: check_apple_silicon_cores,
        repair: Some(repair_apple_silicon_cores),
        adjusts: &[],
    },
    Rule {
        name: "gpu-matches-os",
        check: check_gpu_matches_os,
        repair: None,
        adjusts: &[],
    },
    Rule {
        name: "intel-mac-hardware",
        check: check_intel_mac_hardware,
        repair: None,
        adjusts: &[],
    },
    Rule {
        name: "color-depth",
        check: check_color_depth,
        repair: Some(repair_color_depth),
        adjusts: &[],
    },
    Rule {
        name: "pixel-depth",
        check: check_pixel_depth,
        repair: Some(repair_pixel_depth),
        adjusts: &[],
    },
    Rule {
        name: "touch-points",
        check: check_touch_points,
        repair: Some(repair_touch_points),
        adjusts: &[],
    },
    Rule {
        name: "device-pixel-ratio",
        check: check_device_pixel_ratio,
        repair: Some(|cfg, os| repair_device_pixel_ratio(cfg, os, None)),
        adjusts: &CSS_GEOMETRY_KEYS,
    },
    Rule {
        name: "window-chrome",
        check: check_window_chrome,
        repair: Some(repair_window_chrome),
        adjusts: &[],
    },
    Rule {
        name: "screen-shape",
        check: check_screen_shape,
        repair: None,
        adjusts: &[],
    },
    Rule {
        name: "avail-bounds",
        check: check_avail_bounds,
        repair: Some(repair_avail_bounds),
        adjusts: &[],
    },
    Rule {
        name: "arch-agreement",
        check: check_arch_agreement,
        repair: None,
        adjusts: &[],
    },
    Rule {
        name: "cpu-matches-hardware",
        check: check_cpu,
        repair: Some(repair_cpu),
        adjusts: &[],
    },
];

pub fn validate(config: &Config, target_os: &str) -> Vec<Violation> {
    let mut violations = Vec::new();
    for rule in &RULES {
        if let Some(detail) = (rule.check)(config, target_os) {
            violations.push(Violation {
                rule: rule.name.to_owned(),
                detail,
                kept: Vec::new(),
            });
        }
    }
    violations
}

pub fn apply(config: &mut Config, target_os: &str, keep: &BTreeSet<String>) -> Vec<Violation> {
    let mut kept_map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for rule in &RULES {
        let Some(repair_fn) = rule.repair else {
            continue;
        };
        if (rule.check)(config, target_os).is_none() {
            continue;
        }
        let mut repaired = config.clone();
        repair_fn(&mut repaired, target_os);
        if !rule.adjusts.is_empty() && rule.adjusts.iter().any(|k| keep.contains(*k)) {
            for &key in rule.adjusts {
                match config.get(key) {
                    Some(v) => {
                        repaired.insert(key.to_owned(), v.clone());
                    }
                    None => {
                        repaired.remove(key);
                    }
                }
            }
        }
        let mut blocked: Vec<String> = keep
            .iter()
            .filter(|k| config.get(*k) != repaired.get(*k))
            .cloned()
            .collect();
        blocked.sort();
        if !blocked.is_empty() {
            kept_map.insert(rule.name.to_owned(), blocked);
        } else {
            *config = repaired;
        }
    }
    validate(config, target_os)
        .into_iter()
        .map(|mut v| {
            if let Some(k) = kept_map.get(&v.rule) {
                v.kept = k.clone();
            }
            v
        })
        .collect()
}
