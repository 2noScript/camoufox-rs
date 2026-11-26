//! Host GPU probing, WebGPU blocklist prefs, and canvas placeholder protection, matching `host_rendering.ts` / `host_rendering.py`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::exceptions::{CamoufoxError, Result};
use crate::fingerprints::is_software_renderer;
use crate::gpudisplay::gpu_prefs;
use crate::paths::install_dir;

pub const BASELINE_OVERRIDES_PREF: &str = "privacy.baselineFingerprintingProtection.overrides";
pub const CANVAS_PLACEHOLDER_TARGETS: [&str; 3] = [
    "+CanvasImageExtractionPrompt",
    "+CanvasExtractionBeforeUserInputIsBlocked",
    "+CanvasExtractionFromThirdPartiesIsBlocked",
];
pub const WEBGPU_BLOCKLIST_PREF: &str = "gfx.blacklist.webgpu";
pub const FEATURE_STATUS_OK: i64 = 1;
pub const FEATURE_BLOCKED_DEVICE: i64 = 4;
pub const HOST_GPU_PROBE_TIMEOUT_MS: u64 = 30_000;

const LAUNCHER_ENV: [&str; 4] = ["CAMOU_", "FONTCONFIG_FILE", "TZ", "MOZ_NO_REMOTE"];
const HOST_GPU_CACHE_FILE: &str = "host_gpu_cache.json";

pub type Gpu = (String, String);

pub fn add_canvas_placeholder(prefs: &mut Map<String, Value>) {
    let mut parts: Vec<String> = prefs
        .get(BASELINE_OVERRIDES_PREF)
        .and_then(Value::as_str)
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    for target in CANVAS_PLACEHOLDER_TARGETS {
        parts.push(target.to_owned());
    }
    prefs.insert(BASELINE_OVERRIDES_PREF.into(), Value::String(parts.join(",")));
}

pub fn has_canvas_placeholder(prefs: &Map<String, Value>) -> bool {
    prefs
        .get(BASELINE_OVERRIDES_PREF)
        .and_then(Value::as_str)
        .is_some_and(|s| s.contains(CANVAS_PLACEHOLDER_TARGETS[0]))
}

pub fn set_webgpu(
    prefs: &mut Map<String, Value>,
    target_os: &str,
    gpu: (Option<&str>, Option<&str>),
    host: Option<&Gpu>,
) {
    if !prefs.contains_key("dom.webgpu.enabled") {
        let enabled = target_os == "win" || (target_os == "mac" && gpu.0 == Some("Apple"));
        prefs.insert("dom.webgpu.enabled".into(), Value::Bool(enabled));
    }
    if prefs
        .get("dom.webgpu.enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && !prefs.contains_key(WEBGPU_BLOCKLIST_PREF)
    {
        let own_adapter = host.is_some_and(|h| {
            renders_on_hardware(Some(h))
                && gpu.0 == Some(h.0.as_str())
                && gpu.1 == Some(h.1.as_str())
        });
        prefs.insert(
            WEBGPU_BLOCKLIST_PREF.into(),
            Value::from(if own_adapter {
                FEATURE_STATUS_OK
            } else {
                FEATURE_BLOCKED_DEVICE
            }),
        );
    }
}

pub fn renders_on_hardware(gpu: Option<&Gpu>) -> bool {
    gpu.is_some_and(|(_, renderer)| !is_software_renderer(Some(renderer.as_str())))
}

static PROBE_CACHE: OnceLock<Mutex<HashMap<String, Option<Gpu>>>> = OnceLock::new();
static PROBE_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

fn probe_headless_mode(headless: bool) -> bool {
    // On macOS, headless WebGL probes report the exact same CoreGL/Metal GPU
    // without flashing a visible temporary browser window on desktop. On Linux,
    // --headless falls back to llvmpipe via glxtest, so keep the display mode.
    if cfg!(target_os = "macos") {
        true
    } else {
        headless
    }
}

fn executable_cache_identity(executable_path: &Path) -> (String, u64, u128) {
    let resolved = std::fs::canonicalize(executable_path)
        .unwrap_or_else(|_| executable_path.to_path_buf());
    let (len, modified_nanos) = std::fs::metadata(&resolved)
        .ok()
        .map(|meta| {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            (meta.len(), mtime)
        })
        .unwrap_or((0, 0));
    (resolved.display().to_string(), len, modified_nanos)
}

fn is_gpu_relevant_env(key: &str) -> bool {
    matches!(
        key,
        "DISPLAY"
            | "WAYLAND_DISPLAY"
            | "XAUTHORITY"
            | "XDG_SESSION_TYPE"
            | "XDG_RUNTIME_DIR"
            | "DRI_PRIME"
            | "SWAYSOCK"
            | "LD_PRELOAD"
    ) || key.starts_with("LIBGL_")
        || key.starts_with("MESA_")
        || key.starts_with("EGL_")
        || key.starts_with("GBM_")
        || key.starts_with("VK_")
        || key.starts_with("__GLX_")
        || key.starts_with("__NV_")
        || key.starts_with("WLR_")
        || key.starts_with("MTL_")
        || key.starts_with("GALLIUM_")
        || key.starts_with("RADV_")
}

fn collect_probe_inputs(
    env: &BTreeMap<String, String>,
    prefs: &Map<String, Value>,
) -> (Vec<(String, String)>, Vec<(String, String)>, Vec<(String, Value)>) {
    let mut host_env: Vec<(String, String)> = env
        .iter()
        .filter(|(k, _)| !LAUNCHER_ENV.iter().any(|p| k.starts_with(p)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    host_env.sort_by(|a, b| a.0.cmp(&b.0));

    let cache_env: Vec<(String, String)> = host_env
        .iter()
        .filter(|(k, _)| is_gpu_relevant_env(k))
        .cloned()
        .collect();

    let gpu_pref_keys = gpu_prefs();
    let mut host_prefs: Vec<(String, Value)> = Vec::new();
    for k in gpu_pref_keys.keys() {
        if let Some(v) = prefs.get(k) {
            host_prefs.push((k.clone(), v.clone()));
        }
    }
    (host_env, cache_env, host_prefs)
}

fn build_host_gpu_cache_key(
    executable_path: &Path,
    probe_headless: bool,
    cache_env: &[(String, String)],
    host_prefs: &[(String, Value)],
) -> String {
    serde_json::to_string(&(
        executable_cache_identity(executable_path),
        probe_headless,
        cache_env,
        host_prefs,
    ))
    .unwrap_or_default()
}

fn host_gpu_cache_path() -> PathBuf {
    install_dir().join(HOST_GPU_CACHE_FILE)
}

fn read_disk_gpu_cache(path: &Path, key: &str) -> Option<Gpu> {
    let raw = std::fs::read_to_string(path).ok()?;
    let map: HashMap<String, Gpu> = serde_json::from_str(&raw).ok()?;
    let hit = map.get(key)?;
    if hit.0.trim().is_empty() || hit.1.trim().is_empty() {
        return None;
    }
    Some(hit.clone())
}

fn write_disk_gpu_cache(path: &Path, key: &str, gpu: &Gpu) {
    if gpu.0.trim().is_empty() || gpu.1.trim().is_empty() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut map: HashMap<String, Gpu> = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    map.insert(key.to_owned(), gpu.clone());
    if let Ok(serialized) = serde_json::to_string_pretty(&map) {
        let _ = std::fs::write(path, serialized);
    }
}

pub async fn host_gpu(
    executable_path: &Path,
    headless: bool,
    env: &BTreeMap<String, String>,
    prefs: &Map<String, Value>,
) -> Result<Option<Gpu>> {
    let probe_headless = probe_headless_mode(headless);
    let (host_env, cache_env, host_prefs) = collect_probe_inputs(env, prefs);
    let cache_key = build_host_gpu_cache_key(executable_path, probe_headless, &cache_env, &host_prefs);

    let cache = PROBE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(hit) = guard.get(&cache_key) {
            return Ok(hit.clone());
        }
    }

    let _probe_guard = PROBE_LOCK.get_or_init(|| tokio::sync::Mutex::new(())).lock().await;
    if let Ok(guard) = cache.lock() {
        if let Some(hit) = guard.get(&cache_key) {
            return Ok(hit.clone());
        }
    }

    let disk_cache_path = host_gpu_cache_path();
    if let Some(disk_hit) = read_disk_gpu_cache(&disk_cache_path, &cache_key) {
        if let Ok(mut guard) = cache.lock() {
            guard.insert(cache_key, Some(disk_hit.clone()));
        }
        return Ok(Some(disk_hit));
    }

    let env_map: HashMap<String, String> = host_env.into_iter().collect();
    let mut launch_prefs: HashMap<String, Value> = host_prefs.into_iter().collect();
    launch_prefs.insert("webgl.force-enabled".into(), Value::Bool(true));

    let gpu = read_gpu(executable_path, probe_headless, env_map, launch_prefs).await?;
    if let Some(ref detected) = gpu {
        write_disk_gpu_cache(&disk_cache_path, &cache_key, detected);
    }
    if let Ok(mut guard) = cache.lock() {
        guard.insert(cache_key, gpu.clone());
    }
    Ok(gpu)
}

async fn read_gpu(
    executable_path: &Path,
    headless: bool,
    env: HashMap<String, String>,
    prefs: HashMap<String, Value>,
) -> Result<Option<Gpu>> {
    let display = env
        .get("DISPLAY")
        .map(|d| format!(" (DISPLAY={d})"))
        .unwrap_or_default();
    let timeout_msg = format!(
        "Host GPU probe timed out after {HOST_GPU_PROBE_TIMEOUT_MS} ms while launching {}{display}.",
        executable_path.display()
    );

    let fut = async {
        let pw = playwright_rs::Playwright::launch().await?;
        let opts = playwright_rs::api::LaunchOptions::new()
            .executable_path(executable_path.display().to_string())
            .headless(headless)
            .env(env)
            .timeout(HOST_GPU_PROBE_TIMEOUT_MS as f64)
            .firefox_user_prefs(prefs);
        let browser = pw.firefox().launch_with_options(opts).await?;
        let page = browser.new_page().await?;
        let res: Option<[String; 2]> = page
            .evaluate::<(), Option<[String; 2]>>(
                "() => { \
                    const gl = document.createElement('canvas').getContext('webgl'); \
                    const info = gl && gl.getExtension('WEBGL_debug_renderer_info'); \
                    return info ? [gl.getParameter(info.UNMASKED_VENDOR_WEBGL), gl.getParameter(info.UNMASKED_RENDERER_WEBGL)] : null; \
                }",
                None,
            )
            .await?;
        let _ = browser.close().await;
        Ok::<Option<Gpu>, CamoufoxError>(res.map(|[v, r]| (v, r)))
    };

    tokio::time::timeout(Duration::from_millis(HOST_GPU_PROBE_TIMEOUT_MS), fut)
        .await
        .map_err(|_| CamoufoxError::GpuDisplayError(timeout_msg))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_headless_mode_avoids_gui_flash_on_macos() {
        if cfg!(target_os = "macos") {
            assert!(probe_headless_mode(false));
            assert!(probe_headless_mode(true));
        } else {
            assert!(!probe_headless_mode(false));
            assert!(probe_headless_mode(true));
        }
    }

    #[test]
    fn cache_key_ignores_per_profile_and_session_noise_env() {
        let mut env_a = BTreeMap::new();
        env_a.insert("DISPLAY".into(), ":0".into());
        env_a.insert("TZ".into(), "America/New_York".into());
        env_a.insert("CAMOU_CONFIG_1".into(), "{}".into());
        env_a.insert("MOZ_NO_REMOTE".into(), "1".into());
        env_a.insert("TMPDIR".into(), "/tmp/a".into());
        env_a.insert("SHLVL".into(), "1".into());

        let mut env_b = BTreeMap::new();
        env_b.insert("DISPLAY".into(), ":0".into());
        env_b.insert("TZ".into(), "Asia/Ho_Chi_Minh".into());
        env_b.insert("CAMOU_PREFS_1".into(), "{\"a\":1}".into());
        env_b.insert("TMPDIR".into(), "/tmp/b".into());
        env_b.insert("SHLVL".into(), "3".into());

        let prefs = Map::new();
        let (ha, ca, pa) = collect_probe_inputs(&env_a, &prefs);
        let (hb, cb, pb) = collect_probe_inputs(&env_b, &prefs);
        assert_ne!(ha, hb);
        assert_eq!(ca, cb);
        assert_eq!(
            build_host_gpu_cache_key(Path::new("/tmp/camoufox"), true, &ca, &pa),
            build_host_gpu_cache_key(Path::new("/tmp/camoufox"), true, &cb, &pb)
        );
    }

    #[test]
    fn disk_gpu_cache_roundtrip_and_invalid_json_recovery() {
        let dir = std::env::temp_dir().join(format!(
            "camoufox-host-gpu-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let cache_file = dir.join("host_gpu_cache.json");

        assert_eq!(read_disk_gpu_cache(&cache_file, "k1"), None);

        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(&cache_file, "not-valid-json").expect("write corrupt cache");
        assert_eq!(read_disk_gpu_cache(&cache_file, "k1"), None);

        let gpu = ("Apple".to_owned(), "Apple M1, or similar".to_owned());
        write_disk_gpu_cache(&cache_file, "k1", &gpu);
        assert_eq!(read_disk_gpu_cache(&cache_file, "k1"), Some(gpu));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
