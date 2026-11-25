//! Identity generation (`fpgen` + presets), font/voice/media-device draws, and geometry/arch alignment,
//! matching `fingerprints.ts` / `fingerprints.py`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use fpgen_rs::{generate, FpgenError, GenerateOptions};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::coherence::{hardware_cpu, Config};
use crate::cpu_affinity;
use crate::exceptions::{CamoufoxError, Result};
use crate::ip::{valid_ipv4, validate_ip};
use crate::locales::normalize_locale;
use crate::paths::{configure_fpgen_model_dir, data_file_str, launcher_constants, load_data_value};
use crate::pycompat::{
    compare_py_str, crc32, num_f64, orjson_dumps, py_str, py_sum_floats, py_truthy, PyRandom,
};
use crate::warnings::{load_warnings, warn, FallbackWarning};
use crate::webgl::{sample_webgl_for_screen, webgl_for_gpu};

pub const SUPPORTED_OS: [&str; 3] = ["linux", "macos", "windows"];
pub const PRESETS_V150_MIN_FF: u32 = 149;
pub const MODERN_SCREEN_FLOOR: (i64, i64) = (1366, 768);

const SOFTWARE_RENDERERS: [&str; 4] = [
    "llvmpipe",
    "Microsoft Basic Render Driver",
    "SwiftShader",
    "Generic Renderer",
];
const DISCRETE_GPU_BUCKETS: [&str; 4] = [
    "GeForce 8800 GTX",
    "GeForce GTX 480",
    "GeForce GTX 980",
    "Radeon R9 200 Series",
];
const NETBOOK_MAX_PIXELS: i64 = 1024 * 600;
const APP_VERSION_DROPPED: [&str; 4] = ["Win64", "x64", "Mobile", "Tablet"];

pub fn fpgen_os_name(os_key: &str) -> Result<&'static str> {
    match os_key.to_ascii_lowercase().as_str() {
        "lin" | "linux" => Ok("Linux"),
        "mac" | "macos" => Ok("macOS"),
        "win" | "windows" => Ok("Windows"),
        other => Err(CamoufoxError::InvalidOS(format!(
            "Unknown OS for fingerprint generation: '{other}'"
        ))),
    }
}

pub fn os_key_of(target_os: &str) -> &'static str {
    match target_os {
        "windows" | "win" => "win",
        "linux" | "lin" => "lin",
        _ => "mac",
    }
}

pub fn target_os_from_platform(plat: &str) -> &'static str {
    if plat == "Win32" {
        "windows"
    } else if plat.contains("Linux") || plat.contains("linux") {
        "linux"
    } else {
        "macos"
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    pub min_width: Option<u32>,
    pub max_width: Option<u32>,
    pub min_height: Option<u32>,
    pub max_height: Option<u32>,
}

impl Screen {
    pub fn new(
        min_width: Option<u32>,
        max_width: Option<u32>,
        min_height: Option<u32>,
        max_height: Option<u32>,
    ) -> Self {
        Self {
            min_width,
            max_width,
            min_height,
            max_height,
        }
    }

    pub fn matches(&self, fp: &Value) -> bool {
        let screen = fp.get("screen").and_then(Value::as_object);
        let check = |key: &str, lo: Option<u32>, hi: Option<u32>| -> bool {
            if lo.is_none() && hi.is_none() {
                return true;
            }
            let Some(v) = screen.and_then(|s| s.get(key)).and_then(Value::as_i64) else {
                return false;
            };
            lo.is_none_or(|l| v >= l as i64) && hi.is_none_or(|h| v <= h as i64)
        };
        check("width", self.min_width, self.max_width)
            && check("height", self.min_height, self.max_height)
    }

    pub fn to_py_value(&self) -> Value {
        serde_json::json!({
            "min_width": self.min_width,
            "max_width": self.max_width,
            "min_height": self.min_height,
            "max_height": self.max_height,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceObject {
    pub name: String,
    pub lang: String,
    #[serde(rename = "voiceUri")]
    pub voice_uri: String,
    #[serde(rename = "isDefault")]
    pub is_default: bool,
    #[serde(rename = "isLocalService")]
    pub is_local_service: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeechdGrid {
    pub languages: Vec<[String; 2]>,
    pub variants: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ContextFingerprint {
    pub init_script: String,
    pub context_options: Map<String, Value>,
    pub config: Config,
    pub preset: Value,
}

fn os_fonts_data() -> &'static HashMap<String, Vec<String>> {
    static CACHE: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let raw = data_file_str("fonts.json").expect("fonts.json bundled");
        serde_json::from_str(raw).expect("fonts.json valid")
    })
}

pub fn load_os_fonts() -> &'static HashMap<String, Vec<String>> {
    os_fonts_data()
}

fn essential_fonts() -> &'static HashMap<String, Vec<String>> {
    static CACHE: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let raw = data_file_str("essential-fonts.json").expect("essential-fonts.json bundled");
        serde_json::from_str(raw).expect("essential-fonts.json valid")
    })
}

pub fn windows11_marker_fonts() -> HashSet<String> {
    launcher_constants()
        .windows11_marker_fonts
        .iter()
        .cloned()
        .collect()
}

pub fn marker_fonts_for(target_os: &str) -> Vec<String> {
    let key = match target_os {
        "windows" | "win" => "win",
        "linux" | "lin" => "lin",
        _ => "mac",
    };
    launcher_constants()
        .marker_fonts
        .get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn ensure_marker_fonts(fonts: &mut Vec<String>, markers: &[String]) {
    let existing: HashSet<String> = fonts.iter().cloned().collect();
    for m in markers {
        if !existing.contains(m) {
            fonts.push(m.clone());
        }
    }
}

/// First 8 big-endian bytes of SHA-256 of sorted canonical JSON, or random 64-bit salt.
pub fn identity_salt(pinned: Option<&Value>) -> u64 {
    match pinned {
        Some(v) if !v.is_null() => {
            let blob = orjson_dumps(v, true);
            let digest = Sha256::digest(blob.as_bytes());
            let mut arr = [0u8; 8];
            arr.copy_from_slice(&digest[..8]);
            u64::from_be_bytes(arr)
        }
        _ => {
            let mut rng = PyRandom::new(None);
            let hi = (rng.random() * ((1u64 << 32) as f64)) as u64;
            let lo = (rng.random() * ((1u64 << 32) as f64)) as u64;
            (hi << 32) | lo
        }
    }
}

pub fn versionless_ua(user_agent: &str) -> String {
    let re = Regex::new(r"(rv:|Firefox/)[0-9][0-9.]*").expect("regex");
    re.replace_all(user_agent, "$1").into_owned()
}

pub fn identity_seed(config: &Config, salt: u64) -> u32 {
    let get_s = |k: &str| config.get(k).map(py_str).unwrap_or_default();
    let parts = [
        versionless_ua(&get_s("navigator.userAgent")),
        get_s("navigator.platform"),
        get_s("screen.width"),
        get_s("screen.height"),
        get_s("navigator.hardwareConcurrency"),
        salt.to_string(),
    ];
    crc32(parts.join("|").as_bytes())
}

pub fn drop_audio_seed(config: &mut Config) {
    if config.remove("audio:seed").is_some() {
        if let Some(msg) = load_warnings().get("audio_seed_removed") {
            warn(msg, "FutureWarning");
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct FontUnit {
    kind: String,
    prob: Option<f64>,
    fonts: Vec<String>,
    #[serde(rename = "requiresLocale")]
    requires_locale: Option<String>,
    sizes: Option<Vec<SizeWeight>>,
}

#[derive(Debug, Clone, Deserialize)]
struct SizeWeight {
    n: usize,
    w: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct FontBase {
    id: String,
    weight: Option<f64>,
    fonts: Vec<String>,
}

fn load_font_groups() -> &'static HashMap<String, Vec<FontUnit>> {
    static CACHE: OnceLock<HashMap<String, Vec<FontUnit>>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let raw = data_file_str("font-groups.json").expect("font-groups.json bundled");
        serde_json::from_str(raw).expect("font-groups.json valid")
    })
}

fn load_font_bases() -> &'static HashMap<String, Vec<FontBase>> {
    static CACHE: OnceLock<HashMap<String, Vec<FontBase>>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let raw = data_file_str("font-bases.json").expect("font-bases.json bundled");
        serde_json::from_str(raw).expect("font-bases.json valid")
    })
}

fn pick_release<'a>(os_key: &str, r: &mut PyRandom) -> Option<&'a FontBase> {
    let bases = load_font_bases().get(os_key)?;
    if bases.is_empty() {
        return None;
    }
    let roll = r.random();
    let mut cum = 0.0;
    for b in bases {
        cum += b.weight.unwrap_or(0.0);
        if roll < cum {
            return Some(b);
        }
    }
    bases.last()
}

fn locale_matches(locale: Option<&str>, required: &str) -> bool {
    locale
        .unwrap_or("")
        .to_ascii_lowercase()
        .starts_with(&required.to_ascii_lowercase())
}

fn draw_units(
    os_key: &str,
    r: &mut PyRandom,
    exclude: &HashSet<String>,
    locale: Option<&str>,
) -> Vec<String> {
    let mut out = Vec::new();
    let Some(units) = load_font_groups().get(os_key) else {
        return out;
    };
    for unit in units {
        if let Some(req) = &unit.requires_locale {
            if !locale_matches(locale, req) {
                continue;
            }
        }
        if r.random() >= unit.prob.unwrap_or(0.0) {
            continue;
        }
        let mut members: Vec<String> = unit
            .fonts
            .iter()
            .filter(|f| !exclude.contains(*f))
            .cloned()
            .collect();
        if members.is_empty() {
            continue;
        }
        if unit.kind == "alacarte" {
            let sizes = unit
                .sizes
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(&[]);
            let fallback = [SizeWeight {
                n: members.len(),
                w: 1.0,
            }];
            let active_sizes = if sizes.is_empty() { &fallback[..] } else { sizes };
            let weights: Vec<f64> = active_sizes.iter().map(|s| s.w).collect();
            let roll = r.random() * py_sum_floats(&weights);
            let mut cum = 0.0;
            let mut count = active_sizes.last().map(|s| s.n).unwrap_or(1);
            for s in active_sizes {
                cum += s.w;
                if roll < cum {
                    count = s.n;
                    break;
                }
            }
            let k = count.max(1).min(members.len());
            members = r.sample(&members, k);
        }
        out.extend(members);
    }
    out
}

fn host_has_variant_fonts(target_os: &str) -> bool {
    if target_os != "windows" {
        return false;
    }
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".to_owned());
    PathBuf::from(windir)
        .join("Fonts")
        .join("SegUIVar.ttf")
        .exists()
}

pub fn generate_random_font_subset(
    target_os: &str,
    seed: Option<u64>,
    native: bool,
    locale: Option<&str>,
) -> Vec<String> {
    let mut r = PyRandom::new(seed);
    let os_key = os_key_of(target_os);
    let full_list = os_fonts_data()
        .get(os_key)
        .or_else(|| os_fonts_data().get("mac"))
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let essential: HashSet<String> = essential_fonts()
        .get(os_key)
        .map(|v| v.iter().cloned().collect())
        .unwrap_or_default();
    let markers = marker_fonts_for(target_os);

    if native {
        let mut result: Vec<String> = full_list
            .iter()
            .filter(|f| essential.contains(*f))
            .cloned()
            .collect();
        let full_set: HashSet<&String> = full_list.iter().collect();
        let mut extra: Vec<String> = essential
            .iter()
            .filter(|f| !full_set.contains(*f))
            .cloned()
            .collect();
        extra.sort_by(|a, b| compare_py_str(a, b));
        result.extend(extra);
        if target_os == "windows" && !host_has_variant_fonts(target_os) {
            let win11 = windows11_marker_fonts();
            result.retain(|f| !win11.contains(f));
        }
        return result;
    }

    let release = pick_release(os_key, &mut r);
    let mut base: Vec<String> = release.map(|rel| rel.fonts.clone()).unwrap_or_default();
    if base.is_empty() {
        base = full_list
            .iter()
            .filter(|f| essential.contains(*f))
            .cloned()
            .collect();
    }
    let mut result = base;
    let mut chosen: HashSet<String> = result.iter().cloned().collect();

    for font in full_list {
        if essential.contains(font) && chosen.insert(font.clone()) {
            result.push(font.clone());
        }
    }
    for font in draw_units(os_key, &mut r, &chosen, locale) {
        if chosen.insert(font.clone()) {
            result.push(font);
        }
    }
    ensure_marker_fonts(&mut result, &markers);
    result
}

// ---------------------------------------------------------------------------
// Voices
// ---------------------------------------------------------------------------

fn load_voice_uris() -> &'static HashMap<String, HashMap<String, String>> {
    static CACHE: OnceLock<HashMap<String, HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| {
        data_file_str("voice-uris.json")
            .ok()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    })
}

fn load_voice_manifests() -> &'static Map<String, Value> {
    static CACHE: OnceLock<Map<String, Value>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let raw = data_file_str("voice-manifests.json").expect("voice-manifests.json bundled");
        serde_json::from_str(raw).expect("voice-manifests.json valid")
    })
}

pub fn voice_uri(os_key: &str, name: &str, lang: &str) -> String {
    match os_key {
        "lin" => {
            let mut escaped = String::new();
            for ch in name.chars() {
                if ch == ' ' {
                    escaped.push_str("%20");
                } else if (ch as u32) <= 0x7f {
                    escaped.push(ch);
                } else {
                    let mut buf = [0u8; 4];
                    for b in ch.encode_utf8(&mut buf).as_bytes() {
                        escaped.push_str(&format!("%{b:02X}"));
                    }
                }
            }
            format!("urn:moz-tts:speechd:{escaped}?{lang}")
        }
        "win" => format!("urn:moz-tts:sapi:{name}?{lang}"),
        "mac" => {
            if let Some(uri) = load_voice_uris()
                .get("mac")
                .and_then(|m| m.get(&format!("{name}|{lang}")))
            {
                return uri.clone();
            }
            let ascii_name: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            let lc = launcher_constants();
            if lc.mac_novelty_voices.iter().any(|v| v == name) {
                format!("urn:moz-tts:osx:com.apple.speech.synthesis.voice.{ascii_name}")
            } else if lc.mac_eloquence_voices.iter().any(|v| v == name) {
                format!("urn:moz-tts:osx:com.apple.eloquence.{lang}.{ascii_name}")
            } else {
                format!("urn:moz-tts:osx:com.apple.voice.compact.{lang}.{ascii_name}")
            }
        }
        _ => {
            let slug: String = name
                .to_ascii_lowercase()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '.' })
                .collect();
            slug.trim_matches('.').to_owned()
        }
    }
}

fn split_voice_entry(entry: &str) -> Result<(String, String, String)> {
    let mut it = entry.rsplitn(3, ':');
    let vtype = it.next().unwrap_or("");
    let lang = it.next();
    let name = it.next();
    match (name, lang) {
        (Some(n), Some(l)) => Ok((n.to_owned(), l.to_owned(), vtype.to_owned())),
        _ => Err(CamoufoxError::ValueError(format!(
            "not enough values to unpack (voice entry '{entry}')"
        ))),
    }
}

fn weighted_pick<'a>(r: &mut PyRandom, items: &'a [Value], wkey: &str) -> &'a Value {
    let weights: Vec<f64> = items
        .iter()
        .map(|i| i.get(wkey).and_then(Value::as_f64).unwrap_or(0.0))
        .collect();
    let total = py_sum_floats(&weights);
    let mut x = r.random() * total;
    for item in items {
        x -= item.get(wkey).and_then(Value::as_f64).unwrap_or(0.0);
        if x <= 0.0 {
            return item;
        }
    }
    &items[items.len() - 1]
}

fn weighted_sample<T: Clone, F: Fn(&T) -> f64>(
    r: &mut PyRandom,
    items: &[T],
    k: usize,
    weight_fn: F,
) -> Vec<T> {
    let mut pool = items.to_vec();
    let mut out = Vec::new();
    while !pool.is_empty() && out.len() < k {
        let weights: Vec<f64> = pool.iter().map(&weight_fn).collect();
        let total = py_sum_floats(&weights);
        let mut x = r.random() * total;
        let mut picked_idx = None;
        for (idx, item) in pool.iter().enumerate() {
            x -= weight_fn(item);
            if x <= 0.0 {
                picked_idx = Some(idx);
                break;
            }
        }
        let chosen = match picked_idx {
            Some(i) => pool.remove(i),
            None => pool.pop().unwrap(),
        };
        out.push(chosen);
    }
    out
}

fn resolve_display_pack(packs: &Map<String, Value>, fallback: &str, locale: Option<&str>) -> String {
    if let Some(loc) = locale {
        if packs.contains_key(loc) {
            return loc.to_owned();
        }
        let lang = loc.split('-').next().unwrap_or("").to_ascii_lowercase();
        for k in packs.keys() {
            if k.split('-').next().unwrap_or("").eq_ignore_ascii_case(&lang) {
                return k.clone();
            }
        }
    }
    if packs.contains_key(fallback) {
        fallback.to_owned()
    } else {
        packs.keys().next().cloned().unwrap_or_default()
    }
}

fn speechd_grid(seed: Option<u64>) -> Result<SpeechdGrid> {
    let mut r = PyRandom::new(seed);
    let release = pick_release("lin", &mut r);
    let lin = load_voice_manifests()
        .get("lin")
        .and_then(|v| v.get("releases"))
        .and_then(Value::as_object)
        .ok_or_else(|| CamoufoxError::ValueError("missing lin releases in voice-manifests.json".into()))?;
    let grid_val = release
        .and_then(|rel| lin.get(&rel.id))
        .or_else(|| lin.values().next())
        .and_then(|v| v.get("speechd"))
        .cloned()
        .ok_or_else(|| CamoufoxError::ValueError("missing speechd grid".into()))?;
    serde_json::from_value(grid_val).map_err(|e| CamoufoxError::ValueError(e.to_string()))
}

pub fn speechd_voices(grid: &SpeechdGrid) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for [name, lang] in &grid.languages {
        out.push((name.clone(), lang.clone()));
        for variant in &grid.variants {
            out.push((format!("{name}+{variant}"), lang.clone()));
        }
    }
    out
}

pub fn draw_voice_config(
    target_os: &str,
    locale: Option<&str>,
    seed: Option<u64>,
) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    if os_key_of(target_os) == "lin" {
        let grid = speechd_grid(seed)?;
        out.insert(
            "voices:speechd".into(),
            serde_json::to_value(grid).map_err(|e| CamoufoxError::Serialization(e.to_string()))?,
        );
    } else {
        let voices = generate_random_voice_subset(target_os, locale, seed)?;
        out.insert(
            "voices".into(),
            serde_json::to_value(voices).map_err(|e| CamoufoxError::Serialization(e.to_string()))?,
        );
    }
    Ok(out)
}

pub fn generate_random_voice_subset(
    target_os: &str,
    locale: Option<&str>,
    seed: Option<u64>,
) -> Result<Vec<VoiceObject>> {
    let os_key = os_key_of(target_os);
    if os_key == "lin" {
        let grid = speechd_grid(seed)?;
        return Ok(speechd_voices(&grid)
            .into_iter()
            .map(|(name, lang)| {
                let uri = voice_uri("lin", &name, &lang);
                VoiceObject {
                    name,
                    lang,
                    voice_uri: uri,
                    is_default: false,
                    is_local_service: false,
                }
            })
            .collect());
    }

    let mut r = PyRandom::new(seed);
    let manifests = load_voice_manifests();
    let manifest = manifests
        .get(os_key)
        .or_else(|| manifests.get("mac"))
        .ok_or_else(|| CamoufoxError::ValueError("missing mac voice manifest".into()))?;

    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut legacy: Vec<String> = Vec::new();

    let mut add_entries = |entries: Option<&Vec<Value>>| {
        if let Some(arr) = entries {
            for e in arr {
                if let Some(s) = e.as_str() {
                    if seen.insert(s.to_owned()) {
                        out.push(s.to_owned());
                    }
                }
            }
        }
    };

    add_entries(manifest.get("base").and_then(Value::as_array));

    let packs_opt = manifest.get("langPacks").and_then(Value::as_object);
    let mut chosen = HashSet::new();

    fn take_pack(
        pack: &Value,
        seen: &mut HashSet<String>,
        out: &mut Vec<String>,
        legacy: &mut Vec<String>,
        r: &mut PyRandom,
    ) {
        if let Some(arr) = pack.get("oneCore").and_then(Value::as_array) {
            for e in arr {
                if let Some(s) = e.as_str() {
                    if seen.insert(s.to_owned()) {
                        out.push(s.to_owned());
                    }
                }
            }
        }
        let dprob = pack.get("desktopProb").and_then(Value::as_f64).unwrap_or(0.0);
        if let Some(darr) = pack.get("desktop").and_then(Value::as_array) {
            if !darr.is_empty() && r.random() < dprob {
                for e in darr {
                    if let Some(s) = e.as_str() {
                        if !legacy.iter().any(|x| x == s) {
                            legacy.push(s.to_owned());
                        }
                    }
                }
            }
        }
    }

    if let Some(packs) = packs_opt {
        if !packs.is_empty() {
            let fallback = manifest
                .get("fallbackLocale")
                .and_then(Value::as_str)
                .unwrap_or("en-US");
            let key = resolve_display_pack(packs, fallback, locale);
            if let Some(pack) = packs.get(&key) {
                chosen.insert(key);
                take_pack(pack, &mut seen, &mut out, &mut legacy, &mut r);
            }
        }
    }

    if let Some(additions) = manifest.get("additions").and_then(Value::as_array) {
        for addition in additions {
            if addition.get("deferred").is_some_and(py_truthy) {
                continue;
            }
            if let Some(req) = addition.get("requiresLocale").and_then(Value::as_str) {
                if !locale_matches(locale, req) {
                    continue;
                }
            }
            let prob = addition.get("prob").and_then(Value::as_f64).unwrap_or(0.0);
            if r.random() >= prob {
                continue;
            }
            let kind = addition.get("kind").and_then(Value::as_str).unwrap_or("");
            if kind == "bundle" {
                if let Some(arr) = addition.get("voices").and_then(Value::as_array) {
                    for e in arr {
                        if let Some(s) = e.as_str() {
                            if seen.insert(s.to_owned()) {
                                out.push(s.to_owned());
                            }
                        }
                    }
                }
            } else if kind == "alacarte" {
                let default_sizes = [serde_json::json!({"n": 1, "w": 1.0})];
                let sizes = addition
                    .get("sizes")
                    .and_then(Value::as_array)
                    .filter(|a| !a.is_empty())
                    .map(Vec::as_slice)
                    .unwrap_or(&default_sizes);
                let picked_size = weighted_pick(&mut r, sizes, "w");
                let k = picked_size.get("n").and_then(Value::as_u64).unwrap_or(1) as usize;
                let voices: Vec<String> = addition
                    .get("voices")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                for e in weighted_sample(&mut r, &voices, k, |_| 1.0) {
                    if seen.insert(e.clone()) {
                        let el = e.to_ascii_lowercase();
                        let idx = out
                            .iter()
                            .position(|v| compare_py_str(&v.to_ascii_lowercase(), &el).is_gt())
                            .unwrap_or(out.len());
                        out.insert(idx, e);
                    }
                }
            } else if kind == "groups" {
                if let Some(packs) = packs_opt {
                    let eligible: Vec<String> = addition
                        .get("groups")
                        .and_then(Value::as_array)
                        .map(|arr| {
                            arr.iter()
                                .filter_map(Value::as_str)
                                .filter(|g| packs.contains_key(*g) && !chosen.contains(*g))
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default();
                    if eligible.is_empty() {
                        continue;
                    }
                    let k = addition
                        .get("sizes")
                        .and_then(Value::as_array)
                        .filter(|a| !a.is_empty())
                        .and_then(|sizes| weighted_pick(&mut r, sizes, "w").get("n").and_then(Value::as_u64))
                        .map(|n| n as usize)
                        .unwrap_or(eligible.len());
                    for g in weighted_sample(&mut r, &eligible, k, |grp| {
                        packs
                            .get(grp)
                            .and_then(|p| p.get("weight"))
                            .and_then(Value::as_f64)
                            .unwrap_or(0.01)
                    }) {
                        chosen.insert(g.clone());
                        if let Some(p) = packs.get(&g) {
                            take_pack(p, &mut seen, &mut out, &mut legacy, &mut r);
                        }
                    }
                }
            }
        }
    }

    let mut voices = Vec::with_capacity(out.len() + legacy.len());
    for entry in out.into_iter().chain(legacy) {
        let (name, lang, vtype) = split_voice_entry(&entry)?;
        let uri = voice_uri(os_key, &name, &lang);
        voices.push(VoiceObject {
            name,
            lang,
            voice_uri: uri,
            is_default: false,
            is_local_service: vtype == "local",
        });
    }
    if let Some(sys_def) = manifest
        .get("systemVoice")
        .and_then(|s| s.get("default"))
        .and_then(Value::as_str)
    {
        if let Some(v) = voices.iter_mut().find(|v| v.name == sys_def) {
            v.is_default = true;
        }
    }
    Ok(voices)
}

pub fn normalize_preset_voices(entries: &[Value], target_os: &str) -> Vec<VoiceObject> {
    let os_key = os_key_of(target_os);
    let mut result = Vec::new();
    for entry in entries {
        if let Ok(obj) = serde_json::from_value::<VoiceObject>(entry.clone()) {
            result.push(obj);
            continue;
        }
        if let Some(s) = entry.as_str() {
            if let Ok((name, lang, vtype)) = split_voice_entry(s) {
                if !name.is_empty() && !lang.is_empty() {
                    let uri = voice_uri(os_key, &name, &lang);
                    result.push(VoiceObject {
                        name,
                        lang,
                        voice_uri: uri,
                        is_default: false,
                        is_local_service: vtype == "local",
                    });
                }
            }
        }
    }
    if !result.is_empty() && !result.iter().any(|v| v.is_default) {
        result[0].is_default = true;
    }
    result
}

// ---------------------------------------------------------------------------
// Hardware concurrency and navigator arch
// ---------------------------------------------------------------------------

pub fn fix_hardware_concurrency(config: &mut Config, can_pin: Option<bool>) {
    let cap = cpu_affinity::host_core_count() as u32;
    if cap == 0 {
        return;
    }
    let pinnable = cpu_affinity::supported() && can_pin != Some(false);
    let plausible = &launcher_constants().plausible_core_counts;
    let host_val = plausible
        .iter()
        .copied()
        .filter(|&c| c <= cap)
        .max()
        .or_else(|| plausible.first().copied())
        .unwrap_or(4);

    let drawn = config
        .get("navigator.hardwareConcurrency")
        .and_then(|v| {
            if let Some(b) = v.as_bool() {
                Some(i64::from(b))
            } else {
                v.as_i64()
            }
        });
    if pinnable {
        if let Some(d) = drawn.filter(|&x| x >= 1) {
            let target = (d as u32).min(cap);
            let chosen = plausible
                .iter()
                .copied()
                .filter(|&c| c <= target)
                .max()
                .or_else(|| plausible.first().copied())
                .unwrap_or(4);
            config.insert("navigator.hardwareConcurrency".into(), Value::from(chosen));
            return;
        }
    }
    config.insert("navigator.hardwareConcurrency".into(), Value::from(host_val));
}

pub fn fix_navigator_arch(config: &mut Config, target_os: &str) {
    if target_os != "lin" {
        return;
    }
    let Some(ua) = config
        .get("navigator.userAgent")
        .filter(|v| py_truthy(v))
        .and_then(Value::as_str)
    else {
        return;
    };
    let target = if ua.contains("Linux x86_64") {
        "Linux x86_64"
    } else if ua.contains("Linux i686") {
        "Linux i686"
    } else {
        return;
    };
    config.insert("navigator.platform".into(), Value::String(target.to_owned()));
    config.insert("navigator.oscpu".into(), Value::String(target.to_owned()));
}

// ---------------------------------------------------------------------------
// Screen / window geometry
// ---------------------------------------------------------------------------

pub fn follow_baseline_protection(config: &mut Config, target_os: &str) {
    let taskbar_h = launcher_constants()
        .baseline_taskbar_height
        .get(target_os)
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if let (Some(w), Some(h)) = (
        config.get("screen.width").filter(|v| py_truthy(v)).cloned(),
        config.get("screen.height").filter(|v| py_truthy(v)).and_then(num_f64),
    ) {
        config.insert("screen.availWidth".into(), w);
        config.insert(
            "screen.availHeight".into(),
            Value::from((h as i64) - taskbar_h),
        );
    }
    if config
        .get("navigator.maxTouchPoints")
        .and_then(num_f64)
        .is_some_and(|n| n > 1.0)
    {
        config.insert(
            "navigator.maxTouchPoints".into(),
            Value::from(launcher_constants().baseline_multi_touch_points),
        );
    }
}

pub fn clamp_window_dimensions(config: &mut Config) {
    for (axis, lower) in [("Width", "width"), ("Height", "height")] {
        let screen_key = format!("screen.{lower}");
        let avail_key = format!("screen.avail{axis}");
        let outer_key = format!("window.outer{axis}");
        let inner_key = format!("window.inner{axis}");

        let screen = config.get(&screen_key).and_then(num_f64);
        let avail = config.get(&avail_key).and_then(num_f64);
        if let (Some(s), Some(a)) = (screen, avail) {
            if a > s {
                config.insert(avail_key.clone(), Value::from(s as i64));
            }
        }
        let avail_clamped = config.get(&avail_key).and_then(num_f64).or(screen);
        let outer = config.get(&outer_key).and_then(num_f64);
        let inner = config.get(&inner_key).and_then(num_f64);
        if let (Some(o), Some(cap)) = (outer, avail_clamped) {
            if o > cap {
                let chrome = inner.map(|i| (o - i).max(0.0)).unwrap_or(0.0);
                config.insert(outer_key.clone(), Value::from(cap as i64));
                if inner.is_some() {
                    config.insert(inner_key.clone(), Value::from(((cap - chrome) as i64).max(1)));
                }
            }
        }
        let outer_clamped = config.get(&outer_key).and_then(num_f64);
        let inner_now = config.get(&inner_key).and_then(num_f64);
        if let (Some(i), Some(o)) = (inner_now, outer_clamped) {
            if i > o {
                config.insert(inner_key, Value::from(o as i64));
            }
        }
    }
}

pub fn clamp_screen_to_display(config: &mut Config, max_width: Option<i64>, max_height: Option<i64>) {
    for (axis, cap) in [("width", max_width), ("height", max_height)] {
        let Some(c) = cap.filter(|&v| v > 0) else {
            continue;
        };
        let screen_key = format!("screen.{axis}");
        let Some(s) = config.get(&screen_key).and_then(num_f64).map(|n| n as i64) else {
            continue;
        };
        if s <= c {
            continue;
        }
        let avail_key = if axis == "width" {
            "screen.availWidth"
        } else {
            "screen.availHeight"
        };
        let avail = config.get(avail_key).and_then(num_f64).map(|n| n as i64);
        config.insert(screen_key, Value::from(c));
        if let Some(a) = avail {
            let gap = (s - a).max(0);
            config.insert(avail_key.into(), Value::from((c - gap).max(1)));
        }
    }
}

pub fn clamp_window_position(config: &mut Config) {
    for (axis, lower, pos_key) in [
        ("Width", "width", "window.screenX"),
        ("Height", "height", "window.screenY"),
    ] {
        let Some(pos) = config.get(pos_key).and_then(num_f64).map(|n| n as i64) else {
            continue;
        };
        let Some(screen) = config
            .get(&format!("screen.{lower}"))
            .and_then(num_f64)
            .map(|n| n as i64)
        else {
            continue;
        };
        let Some(outer) = config
            .get(&format!("window.outer{axis}"))
            .and_then(num_f64)
            .map(|n| n as i64)
        else {
            continue;
        };
        let clamped = pos.min(screen - outer).max(0);
        config.insert(pos_key.into(), Value::from(clamped));
    }
}

pub fn raise_screen_to_modern_floor(config: &mut Config) {
    let (min_w, min_h) = MODERN_SCREEN_FLOOR;
    let Some(sw) = config.get("screen.width").and_then(num_f64).map(|n| n as i64) else {
        return;
    };
    let Some(sh) = config.get("screen.height").and_then(num_f64).map(|n| n as i64) else {
        return;
    };
    if sw >= min_w && sh >= min_h {
        return;
    }
    let gap_w = config
        .get("screen.availWidth")
        .and_then(num_f64)
        .map(|aw| (sw - (aw as i64)).max(0));
    let gap_h = config
        .get("screen.availHeight")
        .and_then(num_f64)
        .map(|ah| (sh - (ah as i64)).max(0));
    let new_w = sw.max(min_w);
    let new_h = sh.max(min_h);
    config.insert("screen.width".into(), Value::from(new_w));
    config.insert("screen.height".into(), Value::from(new_h));
    if let Some(gw) = gap_w {
        config.insert("screen.availWidth".into(), Value::from((new_w - gw).max(1)));
    }
    if let Some(gh) = gap_h {
        config.insert("screen.availHeight".into(), Value::from((new_h - gh).max(1)));
    }
}

// ---------------------------------------------------------------------------
// Media devices
// ---------------------------------------------------------------------------

fn load_media_devices() -> &'static Value {
    static CACHE: OnceLock<Value> = OnceLock::new();
    CACHE.get_or_init(|| {
        let raw = data_file_str("media-devices.json").expect("media-devices.json bundled");
        serde_json::from_str(raw).expect("media-devices.json valid")
    })
}

fn weighted_choice_val<'a>(r: &mut PyRandom, items: &'a [Value]) -> &'a Value {
    let weights: Vec<f64> = items
        .iter()
        .map(|i| i.get("w").and_then(Value::as_f64).unwrap_or(1.0))
        .collect();
    let total: f64 = weights.iter().sum();
    let mut x = r.random() * total;
    for item in items {
        x -= item.get("w").and_then(Value::as_f64).unwrap_or(1.0);
        if x < 0.0 {
            return item;
        }
    }
    &items[items.len() - 1]
}

pub fn draw_media_devices(os_key: &str, seed: Option<u64>) -> Map<String, Value> {
    let mut r = PyRandom::new(seed);
    let catalogue = load_media_devices();
    let cat = catalogue
        .get(os_key)
        .filter(|v| py_truthy(v))
        .or_else(|| catalogue.get("win"))
        .expect("media-devices win category");

    let mut mics: Vec<(String, String)> = Vec::new();
    let mut outs: Vec<(String, String)> = Vec::new();
    let mut cams: Vec<(String, String)> = Vec::new();
    let mut counter = 0usize;
    let mut next_group = || {
        counter += 1;
        format!("hw-{counter}")
    };

    let empty_arr: Vec<Value> = Vec::new();
    let cards = cat
        .get("cards")
        .and_then(Value::as_array)
        .unwrap_or(&empty_arr);
    let card = weighted_choice_val(&mut r, cards);
    let p_no_mic = match os_key {
        "win" => 0.08,
        "lin" => 0.2,
        _ => 0.0,
    };
    let no_mic = r.random() < p_no_mic;
    let card_grp = next_group();

    if !no_mic {
        if let Some(arr) = card.get("mics").and_then(Value::as_array) {
            for m in arr.iter().filter_map(Value::as_str) {
                mics.push((m.to_owned(), card_grp.clone()));
            }
        }
    }
    if let Some(arr) = card.get("outs").and_then(Value::as_array) {
        for o in arr.iter().filter_map(Value::as_str) {
            outs.push((o.to_owned(), card_grp.clone()));
        }
    }
    if let Some(cam) = card.get("cam").and_then(Value::as_str) {
        if !cam.is_empty() {
            cams.push((cam.to_owned(), next_group()));
        }
    }

    let cameras = cat
        .get("cameras")
        .and_then(Value::as_array)
        .unwrap_or(&empty_arr);
    let base_p_cam = match os_key {
        "win" => 0.78,
        "lin" => 0.45,
        _ => 0.0,
    };
    let p_cam = if no_mic { base_p_cam * 0.3 } else { base_p_cam };
    if r.random() < p_cam {
        let builtin: Vec<Value> = cameras
            .iter()
            .filter(|c| !c.get("mic").is_some_and(py_truthy))
            .cloned()
            .collect();
        if !builtin.is_empty() {
            if let Some(cam) = weighted_choice_val(&mut r, &builtin)
                .get("cam")
                .and_then(Value::as_str)
            {
                cams.push((cam.to_owned(), next_group()));
            }
        }
    }

    let headsets = cat
        .get("headsets")
        .and_then(Value::as_array)
        .unwrap_or(&empty_arr);
    let p_headset = cat.get("p_headset").and_then(Value::as_f64).unwrap_or(0.0);
    if r.random() < p_headset && !headsets.is_empty() {
        let item = weighted_choice_val(&mut r, headsets);
        let grp = next_group();
        for m in item
            .get("mics")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            mics.push((m.to_owned(), grp.clone()));
        }
        for o in item
            .get("outs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            outs.push((o.to_owned(), grp.clone()));
        }
        if let Some(cam) = item.get("cam").and_then(Value::as_str) {
            if !cam.is_empty() {
                cams.push((cam.to_owned(), next_group()));
            }
        }
    }

    let displays = cat
        .get("displays")
        .and_then(Value::as_array)
        .unwrap_or(&empty_arr);
    let p_display = cat.get("p_display").and_then(Value::as_f64).unwrap_or(0.0);
    if r.random() < p_display && !displays.is_empty() {
        let item = weighted_choice_val(&mut r, displays);
        let grp = next_group();
        for o in item
            .get("outs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            outs.push((o.to_owned(), grp.clone()));
        }
    }

    let p_extra_cam = cat
        .get("p_extra_camera")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    if r.random() < p_extra_cam && !cameras.is_empty() {
        let with_mic: Vec<Value> = cameras
            .iter()
            .filter(|c| c.get("mic").is_some_and(py_truthy))
            .cloned()
            .collect();
        let external = if with_mic.is_empty() {
            cameras
        } else {
            &with_mic[..]
        };
        let cam = weighted_choice_val(&mut r, external);
        let grp = next_group();
        if let Some(name) = cam.get("cam").and_then(Value::as_str) {
            cams.push((name.to_owned(), grp.clone()));
        }
        if let Some(mic) = cam.get("mic").and_then(Value::as_str) {
            if !mic.is_empty() {
                mics.push((mic.to_owned(), grp));
            }
        }
    }

    if cat.get("monitor_sources").is_some_and(py_truthy) {
        for (label, grp) in outs.clone() {
            mics.push((format!("Monitor of {label}"), grp));
        }
    }

    let mut out = Map::new();
    out.insert("mediaDevices:enabled".into(), Value::Bool(true));
    out.insert("mediaDevices:micros".into(), Value::from(mics.len()));
    out.insert("mediaDevices:webcams".into(), Value::from(cams.len()));
    out.insert("mediaDevices:speakers".into(), Value::from(outs.len()));
    out.insert(
        "mediaDevices:microphoneLabels".into(),
        Value::Array(mics.iter().map(|(l, _)| Value::String(l.clone())).collect()),
    );
    out.insert(
        "mediaDevices:microphoneGroups".into(),
        Value::Array(mics.iter().map(|(_, g)| Value::String(g.clone())).collect()),
    );
    out.insert(
        "mediaDevices:webcamLabels".into(),
        Value::Array(cams.iter().map(|(l, _)| Value::String(l.clone())).collect()),
    );
    out.insert(
        "mediaDevices:webcamGroups".into(),
        Value::Array(cams.iter().map(|(_, g)| Value::String(g.clone())).collect()),
    );
    out.insert(
        "mediaDevices:speakerLabels".into(),
        Value::Array(outs.iter().map(|(l, _)| Value::String(l.clone())).collect()),
    );
    out.insert(
        "mediaDevices:speakerGroups".into(),
        Value::Array(outs.iter().map(|(_, g)| Value::String(g.clone())).collect()),
    );
    out
}

pub fn set_media_devices_defaults(config: &mut Config, salt: u64) {
    if config.keys().any(|k| k.starts_with("mediaDevices:")) {
        return;
    }
    let plat = config
        .get("navigator.platform")
        .map(py_str)
        .unwrap_or_default();
    let os_key = if plat.starts_with("Win") {
        "win"
    } else if plat.starts_with("Mac") {
        "mac"
    } else {
        "lin"
    };
    let seed = identity_seed(config, salt) as u64;
    for (k, v) in draw_media_devices(os_key, Some(seed)) {
        config.insert(k, v);
    }
}

pub fn set_cpu_default(config: &mut Config, target_os: &str, salt: u64) {
    let (implied_arch, implied_maker) = hardware_cpu(config, target_os);
    let existing_arch = config
        .get("cpu:arch")
        .filter(|v| py_truthy(v))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let arch = existing_arch
        .or(implied_arch)
        .unwrap_or_else(|| "x86".to_owned());
    config
        .entry("cpu:arch".to_owned())
        .or_insert_with(|| Value::String(arch.clone()));

    if config.contains_key("cpu:manufacturer")
        || config.get("cpu:arch").and_then(Value::as_str) != Some(arch.as_str())
    {
        return;
    }
    let mut maker = implied_maker;
    if maker.is_none() && arch == "x86" {
        let seed = ((identity_seed(config, salt) ^ 0xc9d0) as u64) & 0xffff_ffff;
        let mut r = PyRandom::new(Some(seed));
        let draw = r.random();
        maker = Some(if draw < launcher_constants().x86_intel_share {
            "intel".to_owned()
        } else {
            "amd".to_owned()
        });
    }
    if let Some(m) = maker {
        config.insert("cpu:manufacturer".into(), Value::String(m));
    }
}

// ---------------------------------------------------------------------------
// WebGL <-> screen helpers
// ---------------------------------------------------------------------------

pub fn renderer_bucket(renderer: &str) -> String {
    let mut core = renderer
        .strip_suffix(", or similar")
        .unwrap_or(renderer)
        .to_owned();
    let d3d_re = Regex::new(r"^ANGLE \([^,]*, (.*?) Direct3D.*\)$").expect("regex");
    let vk_re = Regex::new(r"^ANGLE \((.*)\) on Vulkan$").expect("regex");
    let pcie_re = Regex::new(r"^(.*)/PCIe?/SSE2$").expect("regex");
    if let Some(c) = d3d_re.captures(&core) {
        core = c[1].to_owned();
    } else if let Some(c) = vk_re.captures(&core) {
        core = c[1].to_owned();
    }
    if let Some(c) = pcie_re.captures(&core) {
        core = c[1].to_owned();
    }
    core.strip_prefix("NVIDIA ")
        .map(str::to_owned)
        .unwrap_or(core)
}

pub fn is_software_renderer(renderer: Option<&str>) -> bool {
    let Some(r) = renderer.filter(|s| !s.is_empty()) else {
        return false;
    };
    SOFTWARE_RENDERERS.iter().any(|name| r.contains(name))
}

pub fn gpu_screen_is_plausible(
    renderer: Option<&str>,
    width: Option<i64>,
    height: Option<i64>,
) -> bool {
    let (Some(r), Some(w), Some(h)) = (renderer.filter(|s| !s.is_empty()), width, height) else {
        return true;
    };
    if is_software_renderer(Some(r)) {
        return true;
    }
    let bucket = renderer_bucket(r);
    if !DISCRETE_GPU_BUCKETS.contains(&bucket.as_str()) {
        return true;
    }
    w * h > NETBOOK_MAX_PIXELS
}

// ---------------------------------------------------------------------------
// Presets and UA helpers
// ---------------------------------------------------------------------------

fn presets_cache() -> &'static Mutex<HashMap<String, Value>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Value>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn select_presets_file(ff_version: Option<&str>) -> &'static str {
    let major = ff_version
        .and_then(|s| s.split('.').next())
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    if major >= PRESETS_V150_MIN_FF {
        "fingerprint-presets-v150.json"
    } else {
        "fingerprint-presets.json"
    }
}

pub fn load_presets(ff_version: Option<&str>) -> Option<Value> {
    let file = select_presets_file(ff_version);
    if let Ok(guard) = presets_cache().lock() {
        if let Some(hit) = guard.get(file) {
            return Some(hit.clone());
        }
    }
    let raw = data_file_str(file).ok()?;
    let val: Value = serde_json::from_str(raw).ok()?;
    if let Ok(mut guard) = presets_cache().lock() {
        guard.insert(file.to_owned(), val.clone());
    }
    Some(val)
}

pub fn get_random_preset(os: Option<&[String]>, ff_version: Option<&str>) -> Option<Value> {
    let bundle = load_presets(ff_version)?;
    let os_keys: Vec<&str> = match os {
        Some(list) if !list.is_empty() => list
            .iter()
            .map(|o| match o.as_str() {
                "windows" | "win" => "windows",
                "linux" | "lin" => "linux",
                _ => "macos",
            })
            .collect(),
        _ => vec!["macos", "windows", "linux"],
    };
    let presets_obj = bundle.get("presets")?.as_object()?;
    let mut candidates = Vec::new();
    for key in os_keys {
        if let Some(arr) = presets_obj.get(key).and_then(Value::as_array) {
            candidates.extend(arr.iter().cloned());
        }
    }
    if candidates.is_empty() {
        return None;
    }
    let mut r = PyRandom::new(None);
    let idx = (r.random() * candidates.len() as f64) as usize % candidates.len();
    candidates.into_iter().nth(idx)
}

pub fn app_version_from_user_agent(user_agent: Option<&str>) -> Option<String> {
    let ua = user_agent?;
    let re = Regex::new(r"^Mozilla/5\.0 \(([^)]*)\)").ok()?;
    let caps = re.captures(ua)?;
    let mut kept = Vec::new();
    for part in caps[1].split(';') {
        let token = part.trim();
        if token.starts_with("rv:")
            || APP_VERSION_DROPPED.contains(&token)
            || token.starts_with("Linux ")
            || token.starts_with("Intel Mac OS X")
        {
            continue;
        }
        if token.starts_with("Windows") {
            kept.push("Windows");
        } else {
            kept.push(token);
        }
    }
    if kept.is_empty() {
        None
    } else {
        Some(format!("5.0 ({})", kept.join("; ")))
    }
}

fn set_app_version(config: &mut Config) {
    if let Some(derived) = app_version_from_user_agent(
        config.get("navigator.userAgent").and_then(Value::as_str),
    ) {
        config.insert("navigator.appVersion".into(), Value::String(derived));
    }
}

fn oscpu_from_platform(plat: &str) -> Option<&'static str> {
    if plat == "MacIntel" {
        Some("Intel Mac OS X 10.15")
    } else if plat == "Win32" {
        Some("Windows NT 10.0; Win64; x64")
    } else if plat.contains("Linux") || plat.contains("linux") {
        Some("Linux x86_64")
    } else {
        None
    }
}

pub fn from_preset(preset: &Value, ff_version: Option<&str>, salt: Option<u64>) -> Config {
    let s = salt.unwrap_or_else(|| identity_salt(None));
    let mut config = Config::new();

    let nav = preset.get("navigator").and_then(Value::as_object);
    if let Some(ua_raw) = nav
        .and_then(|n| n.get("userAgent"))
        .and_then(Value::as_str)
    {
        let mut ua = ua_raw.to_owned();
        if let Some(ff) = ff_version {
            let ff_re = Regex::new(r"Firefox/\d+\.0").expect("regex");
            let rv_re = Regex::new(r"rv:\d+\.0").expect("regex");
            ua = ff_re.replace_all(&ua, format!("Firefox/{ff}.0")).into_owned();
            ua = rv_re.replace_all(&ua, format!("rv:{ff}.0")).into_owned();
        }
        config.insert("navigator.userAgent".into(), Value::String(ua));
    }
    if let Some(plat) = nav.and_then(|n| n.get("platform")).filter(|v| py_truthy(v)) {
        config.insert("navigator.platform".into(), plat.clone());
    }
    if let Some(cores) = nav
        .and_then(|n| n.get("hardwareConcurrency"))
        .filter(|v| py_truthy(v))
    {
        config.insert("navigator.hardwareConcurrency".into(), cores.clone());
    }
    if let Some(oscpu) = nav.and_then(|n| n.get("oscpu")).filter(|v| py_truthy(v)) {
        config.insert("navigator.oscpu".into(), oscpu.clone());
    } else if let Some(plat_str) = nav.and_then(|n| n.get("platform")).and_then(Value::as_str) {
        if let Some(oscpu) = oscpu_from_platform(plat_str) {
            config.insert("navigator.oscpu".into(), Value::String(oscpu.to_owned()));
        }
    }
    set_app_version(&mut config);
    if let Some(mtp) = nav.and_then(|n| n.get("maxTouchPoints")) {
        config.insert("navigator.maxTouchPoints".into(), mtp.clone());
    }

    let scr = preset.get("screen").and_then(Value::as_object);
    for (k, cfg_k) in [
        ("width", "screen.width"),
        ("height", "screen.height"),
        ("availWidth", "screen.availWidth"),
        ("availHeight", "screen.availHeight"),
    ] {
        if let Some(v) = scr.and_then(|s| s.get(k)).filter(|v| py_truthy(v)) {
            config.insert(cfg_k.into(), v.clone());
        }
    }
    if let Some(cd) = scr.and_then(|s| s.get("colorDepth")).filter(|v| py_truthy(v)) {
        config.insert("screen.colorDepth".into(), cd.clone());
        config.insert("screen.pixelDepth".into(), cd.clone());
    }

    let wgl = preset.get("webgl").and_then(Value::as_object);
    if let Some(v) = wgl.and_then(|w| w.get("unmaskedVendor")).filter(|v| py_truthy(v)) {
        config.insert("webGl:vendor".into(), v.clone());
    }
    if let Some(r) = wgl
        .and_then(|w| w.get("unmaskedRenderer"))
        .filter(|v| py_truthy(v))
    {
        config.insert("webGl:renderer".into(), r.clone());
    }
    if let Some(tz) = preset.get("timezone").filter(|v| py_truthy(v)) {
        config.insert("timezone".into(), tz.clone());
    }

    let plat = nav
        .and_then(|n| n.get("platform"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let target_os = target_os_from_platform(plat);
    let seed = identity_seed(&config, s) as u64;
    let fonts = generate_random_font_subset(target_os, Some(seed), false, None);
    config.insert(
        "fonts".into(),
        Value::Array(fonts.into_iter().map(Value::String).collect()),
    );
    match draw_voice_config(target_os, None, Some(seed)) {
        Ok(voices_map) => {
            for (k, v) in voices_map {
                config.insert(k, v);
            }
        }
        Err(err) => {
            let ua_s = config.get("navigator.userAgent").and_then(Value::as_str);
            FallbackWarning::warn(
                "Drawing the speech voices",
                "the preset's recorded voices",
                &err.to_string(),
                ua_s,
            );
            if let Some(arr) = preset.get("speechVoices").and_then(Value::as_array) {
                let normalized = normalize_preset_voices(arr, target_os);
                if let Ok(val) = serde_json::to_value(normalized) {
                    config.insert("voices".into(), val);
                }
            }
        }
    }
    config
}

// ---------------------------------------------------------------------------
// Per-context fingerprint & init script
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct InitValues {
    pub navigator_platform: Option<String>,
    pub navigator_oscpu: Option<String>,
    pub navigator_user_agent: Option<String>,
    pub navigator_app_version: Option<String>,
    pub hardware_concurrency: Option<i64>,
    pub webgl_vendor: Option<String>,
    pub webgl_renderer: Option<String>,
    pub screen_width: Option<i64>,
    pub screen_height: Option<i64>,
    pub screen_color_depth: Option<i64>,
    pub timezone: Option<String>,
    pub font_list: Option<Vec<String>>,
    pub speech_voices: Option<Value>,
    pub webrtc_ip: Option<String>,
}

fn ascii_json(val: &Value) -> String {
    let raw = serde_json::to_string(val).unwrap_or_else(|_| "null".into());
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        let code = ch as u32;
        if (0x7f..=0xffff).contains(&code) {
            out.push_str(&format!("\\u{code:04x}"));
        } else {
            out.push(ch);
        }
    }
    out
}

pub fn build_init_script(values: &InitValues) -> Result<String> {
    let mut lines = vec!["(function(v) {".to_owned(), "  var w = window;".to_owned()];

    let str_setters = [
        (&values.navigator_platform, "setNavigatorPlatform"),
        (&values.navigator_oscpu, "setNavigatorOscpu"),
        (&values.navigator_user_agent, "setNavigatorUserAgent"),
        (&values.navigator_app_version, "setNavigatorAppVersion"),
        (&values.webgl_vendor, "setWebGLVendor"),
        (&values.webgl_renderer, "setWebGLRenderer"),
    ];
    for (opt, fn_name) in str_setters {
        if let Some(s) = opt {
            lines.push(format!(
                "  if (typeof w.{fn_name} === \"function\") w.{fn_name}({});",
                ascii_json(&Value::String(s.clone()))
            ));
        }
    }
    if let Some(hc) = values.hardware_concurrency {
        lines.push(format!(
            "  if (typeof w.setNavigatorHardwareConcurrency === \"function\") w.setNavigatorHardwareConcurrency({hc});"
        ));
    }

    if let (Some(sw), Some(sh)) = (
        values.screen_width.filter(|&w| w != 0),
        values.screen_height.filter(|&h| h != 0),
    ) {
        lines.push(format!(
            "  if (typeof w.setScreenDimensions === \"function\") w.setScreenDimensions({sw}, {sh});"
        ));
        if let Some(cd) = values.screen_color_depth.filter(|&d| d != 0) {
            lines.push(format!(
                "  if (typeof w.setScreenColorDepth === \"function\") w.setScreenColorDepth({cd});"
            ));
        }
    }

    if let Some(tz) = values.timezone.as_deref().filter(|s| !s.is_empty()) {
        lines.push(format!(
            "  if (typeof w.setTimezone === \"function\") w.setTimezone({});",
            ascii_json(&Value::String(tz.to_owned()))
        ));
    }

    if let Some(ip) = values.webrtc_ip.as_deref().filter(|s| !s.is_empty()) {
        validate_ip(ip)?;
        let fn_name = if valid_ipv4(ip) {
            "setWebRTCIPv4"
        } else {
            "setWebRTCIPv6"
        };
        lines.push(format!(
            "  if (typeof w.{fn_name} === \"function\") w.{fn_name}({});",
            ascii_json(&Value::String(ip.to_owned()))
        ));
    } else {
        lines.push("  if (typeof w.setWebRTCIPv4 === \"function\") w.setWebRTCIPv4(\"\");".to_owned());
    }

    if let Some(fonts) = values.font_list.as_ref().filter(|f| !f.is_empty()) {
        lines.push(format!(
            "  if (typeof w.setFontList === \"function\") w.setFontList({});",
            ascii_json(&Value::String(fonts.join(",")))
        ));
    }

    if let Some(voices) = &values.speech_voices {
        if voices.is_object() {
            lines.push(format!(
                "  if (typeof w.setSpeechVoices === \"function\") w.setSpeechVoices({});",
                ascii_json(&Value::String(ascii_json(voices)))
            ));
        } else if let Some(arr) = voices.as_array().filter(|a| !a.is_empty()) {
            let names: Vec<String> = arr
                .iter()
                .filter_map(|v| {
                    v.get("name")
                        .and_then(Value::as_str)
                        .or_else(|| v.as_str())
                        .map(str::to_owned)
                })
                .collect();
            lines.push(format!(
                "  if (typeof w.setSpeechVoices === \"function\") w.setSpeechVoices({});",
                ascii_json(&Value::String(names.join(",")))
            ));
        }
    }

    lines.push("})();".to_owned());
    Ok(lines.join("\n"))
}

// ---------------------------------------------------------------------------
// fpgen fingerprints
// ---------------------------------------------------------------------------

fn fpgen_data_map() -> &'static Value {
    static CACHE: OnceLock<Value> = OnceLock::new();
    CACHE.get_or_init(|| load_data_value("fpgen.yml").expect("fpgen.yml bundled"))
}

fn cast_to_properties(
    out: &mut Config,
    cast_node: &Value,
    fp_node: &Value,
    ff_version: Option<&str>,
) {
    let (Some(cast_obj), Some(fp_obj)) = (cast_node.as_object(), fp_node.as_object()) else {
        return;
    };
    let ver_re = Regex::new(r"(1[0-9]{2})(\.0)").expect("regex");
    for (key, raw) in fp_obj {
        if !py_truthy(raw) {
            continue;
        }
        let Some(type_key) = cast_obj.get(key) else {
            continue;
        };
        if !py_truthy(type_key) {
            continue;
        }
        if raw.is_object() {
            cast_to_properties(out, type_key, raw, ff_version);
            continue;
        }
        let Some(target_key) = type_key.as_str() else {
            continue;
        };
        let mut data = raw.clone();
        if let Some(arr) = data.as_array() {
            if arr.len() == 1 && arr[0].is_string() {
                data = arr[0].clone();
            } else {
                continue;
            }
        }
        if target_key.starts_with("screen.") {
            if let Some(n) = data.as_i64() {
                if n < 0 {
                    data = Value::from(0);
                }
            }
        }
        if let (Some(ff), Some(s)) = (ff_version, data.as_str()) {
            data = Value::String(ver_re.replace_all(s, format!("{ff}$2")).into_owned());
        }
        out.insert(target_key.to_owned(), data);
    }
}

fn handle_screen_xy(camoufox_data: &mut Config, fp: &Value) {
    if camoufox_data.contains_key("window.screenY") {
        return;
    }
    let screen = fp.get("screen").and_then(Value::as_object);
    let window = fp.get("window").and_then(Value::as_object);
    let screen_x = window.and_then(|w| w.get("screenX"));
    let Some(sx_val) = screen_x.filter(|v| py_truthy(v)) else {
        camoufox_data.insert("window.screenX".into(), Value::from(0));
        camoufox_data.insert("window.screenY".into(), Value::from(0));
        return;
    };
    if let Some(sx) = sx_val.as_i64() {
        if (-50..=50).contains(&sx) {
            camoufox_data.insert("window.screenY".into(), Value::from(sx));
            return;
        }
    }
    let avail_h = screen
        .and_then(|s| s.get("availHeight"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let outer_h = window
        .and_then(|w| w.get("outerHeight"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let screen_y = avail_h - outer_h;
    let mut r = PyRandom::new(None);
    let val = if screen_y == 0 {
        0
    } else if screen_y > 0 {
        (r.random() * (screen_y as f64)) as i64
    } else {
        screen_y + (r.random() * ((-screen_y) as f64)) as i64
    };
    camoufox_data.insert("window.screenY".into(), Value::from(val));
}

pub fn from_fpgen(fingerprint: &Value, ff_version: Option<&str>) -> Config {
    let mut camoufox_data = Config::new();
    cast_to_properties(&mut camoufox_data, fpgen_data_map(), fingerprint, ff_version);
    handle_screen_xy(&mut camoufox_data, fingerprint);
    set_app_version(&mut camoufox_data);
    camoufox_data
}

pub fn handle_window_size(fp: &mut Value, outer_width: i64, outer_height: i64) {
    let Some(obj) = fp.as_object_mut() else {
        return;
    };
    let screen = obj
        .get("screen")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let sw = screen
        .get("width")
        .and_then(Value::as_i64)
        .unwrap_or(outer_width);
    let sh = screen
        .get("height")
        .and_then(Value::as_i64)
        .unwrap_or(outer_height);

    let win = obj
        .entry("window".to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("window obj");
    let prev_sx = win.get("screenX").and_then(Value::as_i64).unwrap_or(0);
    win.insert("screenX".into(), Value::from(prev_sx + (sw - outer_width) / 2));
    win.insert("screenY".into(), Value::from((sh - outer_height) / 2));

    let prev_ow = win.get("outerWidth").and_then(Value::as_i64).unwrap_or(0);
    let prev_oh = win.get("outerHeight").and_then(Value::as_i64).unwrap_or(0);
    if let Some(iw) = win
        .get("innerWidth")
        .filter(|v| py_truthy(v))
        .and_then(Value::as_i64)
    {
        win.insert("innerWidth".into(), Value::from((outer_width - prev_ow + iw).max(0)));
    }
    if let Some(ih) = win
        .get("innerHeight")
        .filter(|v| py_truthy(v))
        .and_then(Value::as_i64)
    {
        win.insert("innerHeight".into(), Value::from((outer_height - prev_oh + ih).max(0)));
    }
    win.insert("outerWidth".into(), Value::from(outer_width));
    win.insert("outerHeight".into(), Value::from(outer_height));
}

pub fn generate_fingerprint(
    window: Option<(i64, i64)>,
    screen: Option<&Screen>,
    os: Option<&[String]>,
) -> Result<Value> {
    configure_fpgen_model_dir();
    let mut os_values: Vec<String> = Vec::new();
    if let Some(list) = os.filter(|l| !l.is_empty()) {
        for item in list {
            os_values.push(fpgen_os_name(item)?.to_owned());
        }
    }

    let pick_os = |rng: &mut PyRandom| -> Option<String> {
        if os_values.is_empty() {
            None
        } else if os_values.len() == 1 {
            Some(os_values[0].clone())
        } else {
            let idx = (rng.random() * os_values.len() as f64) as usize % os_values.len();
            Some(os_values[idx].clone())
        }
    };

    let mut rng = PyRandom::new(None);
    let has_screen_bound = screen.is_some_and(|s| {
        s.min_width.is_some()
            || s.max_width.is_some()
            || s.min_height.is_some()
            || s.max_height.is_some()
    });

    let mut chosen_fp = None;
    let max_attempts = if has_screen_bound { 32 } else { 1 };
    for _ in 0..max_attempts {
        let mut conds = Map::new();
        conds.insert("browser".into(), Value::String("Firefox".into()));
        if let Some(target) = pick_os(&mut rng) {
            conds.insert("os".into(), Value::String(target));
        }
        let fp = generate(Some(GenerateOptions {
            conditions: Some(Value::Object(conds)),
            ..Default::default()
        }))
        .map_err(|err| match err {
            FpgenError::InvalidConstraints(m) => CamoufoxError::ValueError(m),
            other => CamoufoxError::Fpgen(other),
        })?;
        if let Some(s) = screen {
            if s.matches(&fp) {
                chosen_fp = Some(fp);
                break;
            }
        } else {
            chosen_fp = Some(fp);
            break;
        }
    }

    let mut fp = match chosen_fp {
        Some(f) => f,
        None => {
            let mut conds = Map::new();
            conds.insert("browser".into(), Value::String("Firefox".into()));
            if let Some(target) = pick_os(&mut rng) {
                conds.insert("os".into(), Value::String(target));
            }
            generate(Some(GenerateOptions {
                conditions: Some(Value::Object(conds)),
                ..Default::default()
            }))?
        }
    };

    if let Some((w, h)) = window {
        handle_window_size(&mut fp, w, h);
    }
    Ok(fp)
}

#[allow(clippy::too_many_arguments)]
pub fn generate_context_fingerprint(
    preset: Option<&Value>,
    os: Option<&str>,
    ff_version: Option<&str>,
    webrtc_ip: Option<&str>,
    timezone: Option<&str>,
    locale: Option<&str>,
    config_overrides: Option<&BTreeMap<String, Value>>,
    webgl_config: Option<(&str, &str)>,
) -> Result<ContextFingerprint> {
    let mut config: Config;
    let nav: Map<String, Value>;
    let scr: Map<String, Value>;
    let wgl: Map<String, Value>;
    let resolved_preset: Value;

    if let Some(p) = preset.filter(|v| !v.is_null()) {
        config = from_preset(p, ff_version, None);
        nav = p
            .get("navigator")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        scr = p
            .get("screen")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        wgl = p
            .get("webgl")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        resolved_preset = p.clone();
    } else {
        let os_slice: Option<Vec<String>> = os.map(|o| vec![o.to_owned()]);
        let fp = generate_fingerprint(None, None, os_slice.as_deref())?;
        config = from_fpgen(&fp, ff_version);
        if config
            .get("navigator.userAgent")
            .and_then(Value::as_str)
            .is_some_and(|s| s.contains("Linux"))
        {
            fix_navigator_arch(&mut config, "lin");
        }
        let salt = identity_salt(None);
        let plat = config
            .get("navigator.platform")
            .map(py_str)
            .unwrap_or_default();
        let os_name = target_os_from_platform(&plat);
        let seed = identity_seed(&config, salt) as u64;

        if !config.contains_key("fonts") {
            let fonts = generate_random_font_subset(os_name, Some(seed), false, None);
            config.insert(
                "fonts".into(),
                Value::Array(fonts.into_iter().map(Value::String).collect()),
            );
        }
        if !config.contains_key("voices") && !config.contains_key("voices:speechd") {
            if let Ok(voices_map) = draw_voice_config(os_name, None, Some(seed)) {
                for (k, v) in voices_map {
                    config.insert(k, v);
                }
            }
        }
        if !config.contains_key("navigator.oscpu") {
            if let Some(oscpu) = oscpu_from_platform(&plat) {
                config.insert("navigator.oscpu".into(), Value::String(oscpu.to_owned()));
            }
        }
        if !config.get("webGl:vendor").is_some_and(py_truthy)
            || !config.get("webGl:renderer").is_some_and(py_truthy)
        {
            let target_short = match os {
                Some("macos") => "mac",
                Some("linux") => "lin",
                Some("windows") => "win",
                _ => os_key_of(&plat),
            };
            raise_screen_to_modern_floor(&mut config);
            let mut wgl_fp = if let Some((v, r)) = webgl_config {
                webgl_for_gpu(target_short, v, r, None)?
            } else {
                sample_webgl_for_screen(
                    target_short,
                    config.get("screen.width").and_then(Value::as_i64),
                    config.get("screen.height").and_then(Value::as_i64),
                    None,
                    config.get("navigator.hardwareConcurrency").and_then(Value::as_i64),
                    None,
                )?
            };
            wgl_fp.remove("webGl2Enabled");
            for (k, v) in wgl_fp {
                config.insert(k, v);
            }
        }

        nav = serde_json::json!({
            "platform": config.get("navigator.platform"),
            "hardwareConcurrency": config.get("navigator.hardwareConcurrency"),
        })
        .as_object()
        .cloned()
        .unwrap_or_default();
        scr = serde_json::json!({
            "width": config.get("screen.width"),
            "height": config.get("screen.height"),
            "colorDepth": config.get("screen.colorDepth"),
            "devicePixelRatio": Value::Null,
        })
        .as_object()
        .cloned()
        .unwrap_or_default();
        wgl = serde_json::json!({
            "unmaskedVendor": config.get("webGl:vendor"),
            "unmaskedRenderer": config.get("webGl:renderer"),
        })
        .as_object()
        .cloned()
        .unwrap_or_default();
        resolved_preset = serde_json::json!({
            "navigator": nav,
            "screen": scr,
            "webgl": wgl,
        });
    }

    if let Some(tz) = timezone {
        config.insert("timezone".into(), Value::String(tz.to_owned()));
    }
    if let Some(loc) = locale {
        let parsed = normalize_locale(loc)?;
        config.insert(
            "locale:language".into(),
            Value::String(parsed.language.clone()),
        );
        if let Some(ref reg) = parsed.region {
            config.insert("locale:region".into(), Value::String(reg.clone()));
        }
        config.insert(
            "navigator.language".into(),
            Value::String(parsed.as_string()),
        );
        if let Some(ref sc) = parsed.script {
            config.insert("locale:script".into(), Value::String(sc.clone()));
        }
    }
    if let Some(overrides) = config_overrides {
        let mut copy = Config::new();
        for (k, v) in overrides {
            copy.insert(k.clone(), v.clone());
        }
        drop_audio_seed(&mut copy);
        for (k, v) in copy {
            config.insert(k, v);
        }
    }

    let font_list = config.get("fonts").and_then(Value::as_array).map(|arr| {
        arr.iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    });
    let speech_voices = config.get("voices:speechd").cloned().or_else(|| config.get("voices").cloned());
    let init_values = InitValues {
        navigator_platform: nav.get("platform").and_then(Value::as_str).map(str::to_owned),
        navigator_oscpu: config.get("navigator.oscpu").and_then(Value::as_str).map(str::to_owned),
        navigator_user_agent: config
            .get("navigator.userAgent")
            .and_then(Value::as_str)
            .map(str::to_owned),
        navigator_app_version: config
            .get("navigator.appVersion")
            .and_then(Value::as_str)
            .map(str::to_owned),
        hardware_concurrency: nav
            .get("hardwareConcurrency")
            .or_else(|| config.get("navigator.hardwareConcurrency"))
            .and_then(Value::as_i64),
        webgl_vendor: wgl
            .get("unmaskedVendor")
            .and_then(Value::as_str)
            .map(str::to_owned),
        webgl_renderer: wgl
            .get("unmaskedRenderer")
            .and_then(Value::as_str)
            .map(str::to_owned),
        screen_width: scr.get("width").and_then(Value::as_i64),
        screen_height: scr.get("height").and_then(Value::as_i64),
        screen_color_depth: scr.get("colorDepth").and_then(Value::as_i64),
        timezone: resolved_preset
            .get("timezone")
            .and_then(Value::as_str)
            .or_else(|| config.get("timezone").and_then(Value::as_str))
            .map(str::to_owned),
        font_list,
        speech_voices,
        webrtc_ip: webrtc_ip.map(str::to_owned),
    };
    let init_script = build_init_script(&init_values)?;

    let mut ctx_opts = Map::new();
    if let Some(ua) = config.get("navigator.userAgent").filter(|v| py_truthy(v)) {
        ctx_opts.insert("userAgent".into(), ua.clone());
    }
    if let (Some(sw), Some(sh)) = (
        scr.get("width").and_then(Value::as_i64).filter(|&w| w > 0),
        scr.get("height").and_then(Value::as_i64).filter(|&h| h > 0),
    ) {
        ctx_opts.insert(
            "viewport".into(),
            serde_json::json!({ "width": sw, "height": (sh - 28).max(600) }),
        );
    }
    if let Some(dpr) = scr.get("devicePixelRatio").filter(|v| py_truthy(v)) {
        ctx_opts.insert("deviceScaleFactor".into(), dpr.clone());
    }
    let tz = config
        .get("timezone")
        .filter(|v| py_truthy(v))
        .or_else(|| resolved_preset.get("timezone").filter(|v| py_truthy(v)));
    if let Some(t) = tz {
        ctx_opts.insert("timezoneId".into(), t.clone());
    }
    if let Some(lang) = config.get("navigator.language").filter(|v| py_truthy(v)) {
        ctx_opts.insert("locale".into(), lang.clone());
    }

    Ok(ContextFingerprint {
        init_script,
        context_options: ctx_opts,
        config,
        preset: resolved_preset,
    })
}
