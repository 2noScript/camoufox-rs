//! Host GPU probing, WebGPU blocklist prefs, and canvas placeholder protection, matching `host_rendering.ts` / `host_rendering.py`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Map, Value};

use crate::exceptions::{CamoufoxError, Result};
use crate::fingerprints::is_software_renderer;
use crate::gpudisplay::gpu_prefs;

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

const LAUNCHER_ENV: [&str; 2] = ["CAMOU_", "FONTCONFIG_FILE"];

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

pub async fn host_gpu(
    executable_path: &Path,
    headless: bool,
    env: &BTreeMap<String, String>,
    prefs: &Map<String, Value>,
) -> Result<Option<Gpu>> {
    let mut host_env: Vec<(String, String)> = env
        .iter()
        .filter(|(k, _)| !LAUNCHER_ENV.iter().any(|p| k.starts_with(p)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    host_env.sort_by(|a, b| a.0.cmp(&b.0));

    let gpu_pref_keys = gpu_prefs();
    let mut host_prefs: Vec<(String, Value)> = Vec::new();
    for k in gpu_pref_keys.keys() {
        if let Some(v) = prefs.get(k) {
            host_prefs.push((k.clone(), v.clone()));
        }
    }

    let cache_key = serde_json::to_string(&(
        executable_path.display().to_string(),
        headless,
        &host_env,
        &host_prefs,
    ))
    .unwrap_or_default();

    let cache = PROBE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(hit) = guard.get(&cache_key) {
            return Ok(hit.clone());
        }
    }

    let env_map: HashMap<String, String> = host_env.into_iter().collect();
    let mut launch_prefs: HashMap<String, Value> = host_prefs.into_iter().collect();
    launch_prefs.insert("webgl.force-enabled".into(), Value::Bool(true));

    let gpu = read_gpu(executable_path, headless, env_map, launch_prefs).await?;
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
