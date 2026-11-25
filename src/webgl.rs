//! WebGL hardware/extension distributions drawn from `fpgen-rs`, matching `webgl.ts` / `webgl.py`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use fpgen_rs::bayesian_network::AllowedValues;
use fpgen_rs::model::get_model;
use fpgen_rs::pyjson::{parse_py_typed, py_equals};
use fpgen_rs::{trace_with_evidence, TraceEntry};
use indexmap::{IndexMap, IndexSet};
use serde_json::{Map, Value};

use crate::coherence::{gpu_fits_cpu, gpu_fits_machine, gpu_fits_os};
use crate::exceptions::{CamoufoxError, Result};
use crate::fingerprints::{fpgen_os_name, gpu_screen_is_plausible, is_software_renderer};
use crate::paths::configure_fpgen_model_dir;
use crate::pycompat::{compare_py_str, orjson_dumps, py_str, PyRandom};

const NEVER_EXPOSED_EXTENSIONS: [&str; 4] = [
    "WEBGL_multi_draw",
    "WEBGL_clip_cull_distance",
    "EXT_texture_norm16",
    "WEBGL_compressed_texture_etc1",
];

const HOST_DEPENDENT_EXTENSIONS: [&str; 1] = ["OVR_multiview2"];
const POLYGON_OFFSET_CLAMP: &str = "EXT_polygon_offset_clamp";
const TEXTURE_NORM16: &str = "EXT_texture_norm16";
const CLIP_CONTROL: &str = "EXT_clip_control";
const TEXTURE_MIRROR_CLAMP_TO_EDGE: &str = "EXT_texture_mirror_clamp_to_edge";
const MAX_UNIFORM_BLOCK_SIZE_KEY: &str = "35376";
const MAX_UNIFORM_BLOCK_SIZE_CAP: i64 = 64 * 1024;
const RFP_RENDERER: &str = "Mozilla";

fn filtered_extensions(target_os: &str) -> HashSet<&'static str> {
    let mut set: HashSet<&'static str> = NEVER_EXPOSED_EXTENSIONS.iter().copied().collect();
    if target_os != "win" {
        set.extend(HOST_DEPENDENT_EXTENSIONS);
    }
    set
}

fn find_exact_value_ids(node_name: &str, target_value: &Value) -> Result<IndexSet<String>> {
    let model = get_model(None)?;
    let node = model.network.node(node_name)?;
    let raw_list = model.lookup_value_list(&node.possible_values)?;
    let mut matched = IndexSet::new();
    for (id, raw_json) in node.possible_values.iter().zip(raw_list.iter()) {
        let parsed = parse_py_typed(raw_json)?;
        if py_equals(&parsed, target_value) {
            matched.insert(id.clone());
        }
    }
    if matched.is_empty() {
        return Err(CamoufoxError::ValueError(format!(
            "No exact value id found in node '{node_name}'"
        )));
    }
    Ok(matched)
}

fn trace_cached(
    target: &str,
    target_os: &str,
    pins: &[(&str, &Value)],
) -> Result<Vec<TraceEntry>> {
    configure_fpgen_model_dir();
    static CACHE: OnceLock<Mutex<HashMap<String, Vec<TraceEntry>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = format!(
        "{target}|{target_os}|{}",
        pins.iter()
            .map(|(n, v)| format!("{n}:{}", orjson_dumps(v, true)))
            .collect::<Vec<_>>()
            .join(";")
    );
    if let Ok(guard) = cache.lock() {
        if let Some(hit) = guard.get(&key) {
            return Ok(hit.clone());
        }
    }

    let model = get_model(None)?;
    let os_name = fpgen_os_name(target_os)?;
    let cond = serde_json::json!({
        "browser": "Firefox",
        "os": os_name,
    });
    let mut evidence: IndexMap<String, AllowedValues> =
        fpgen_rs::utils::build_evidence(&model, Some(&cond), false)?;
    for &(node, val) in pins {
        let ids = find_exact_value_ids(node, val)?;
        evidence.insert(node.to_owned(), AllowedValues::Set(ids));
    }

    let traced = trace_with_evidence(&model, &[target], &evidence, false)?;
    let entries = traced
        .as_distribution()
        .map(|s| s.to_vec())
        .unwrap_or_default();

    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, entries.clone());
    }
    Ok(entries)
}

fn choose<'a>(rng: &mut PyRandom, results: &'a [TraceEntry]) -> &'a TraceEntry {
    let total: f64 = results.iter().map(|r| r.probability).sum();
    let mut roll = rng.random() * total;
    for item in results {
        roll -= item.probability;
        if roll <= 0.0 {
            return item;
        }
    }
    &results[results.len() - 1]
}

pub fn firefox_gpus(target_os: &str) -> Result<Vec<(String, String)>> {
    let traces = trace_cached("gpu", target_os, &[])?;
    let mut out = Vec::with_capacity(traces.len());
    for entry in traces {
        if let (Some(v), Some(r)) = (
            entry.value.get("vendor").and_then(Value::as_str),
            entry.value.get("renderer").and_then(Value::as_str),
        ) {
            out.push((v.to_owned(), r.to_owned()));
        }
    }
    Ok(out)
}

fn backend_extensions(target_os: &str, webgl: &Value, has_webgl2: bool) -> Vec<String> {
    let mut extensions = Vec::new();
    if target_os == "win" {
        extensions.push("EXT_depth_clamp".to_owned());
    }
    let has_depth_clamp = webgl
        .get("supportedExtensions")
        .and_then(Value::as_array)
        .is_some_and(|arr| arr.iter().any(|x| x.as_str() == Some("EXT_depth_clamp")));
    let desktop_gl = target_os == "lin" && has_depth_clamp;
    if (target_os == "win" && has_webgl2) || desktop_gl {
        extensions.push(POLYGON_OFFSET_CLAMP.to_owned());
        extensions.push(CLIP_CONTROL.to_owned());
    }
    if target_os == "win" || desktop_gl {
        extensions.push(TEXTURE_MIRROR_CLAMP_TO_EDGE.to_owned());
    }
    extensions
}

fn context_config(
    prefix: &str,
    webgl: &Value,
    target_os: &str,
    backend: &[String],
) -> Map<String, Value> {
    let blocked = filtered_extensions(target_os);
    let mut extensions: Vec<String> = webgl
        .get("supportedExtensions")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .filter(|ext| !blocked.contains(ext))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();

    for ext in backend {
        if extensions.contains(ext) {
            continue;
        }
        let ext_lower = ext.to_ascii_lowercase();
        let pos = extensions
            .iter()
            .position(|other| other.to_ascii_lowercase() > ext_lower)
            .unwrap_or(extensions.len());
        extensions.insert(pos, ext.clone());
    }

    let mut params = Map::new();
    if let Some(obj) = webgl.get("params").and_then(Value::as_object) {
        for (pname, param) in obj {
            let val = param.get("value").cloned().unwrap_or(Value::Null);
            params.insert(pname.clone(), val);
        }
    }

    let mut shader_precisions = Map::new();
    if let Some(arr) = webgl.get("shaderPrecisionFormats").and_then(Value::as_array) {
        for entry in arr {
            let st = entry.get("shaderType").map(py_str).unwrap_or_default();
            let pt = entry.get("precisionType").map(py_str).unwrap_or_default();
            let fmt = entry
                .get("shaderPrecisionFormat")
                .cloned()
                .unwrap_or(Value::Null);
            shader_precisions.insert(format!("{st},{pt}"), fmt);
        }
    }

    let mut out = Map::new();
    out.insert(
        format!("{prefix}:supportedExtensions"),
        Value::Array(extensions.into_iter().map(Value::String).collect()),
    );
    out.insert(format!("{prefix}:parameters"), Value::Object(params));
    out.insert(
        format!("{prefix}:shaderPrecisionFormats"),
        Value::Object(shader_precisions),
    );
    out
}

pub fn to_config(webgl: &Value, webgl2: &Value, target_os: &str) -> Map<String, Value> {
    let has_webgl2 = match webgl2 {
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => false,
    };
    let backend = backend_extensions(target_os, webgl, has_webgl2);
    let mut config = Map::new();
    config.insert(
        "webGl:vendor".into(),
        webgl.get("vendor").cloned().unwrap_or(Value::Null),
    );
    config.insert(
        "webGl:renderer".into(),
        webgl.get("renderer").cloned().unwrap_or(Value::Null),
    );
    config.extend(context_config("webGl", webgl, target_os, &backend));
    config.insert("webGl2Enabled".into(), Value::Bool(has_webgl2));

    if has_webgl2 {
        let mut webgl2_backend = backend.clone();
        if matches!(target_os, "mac" | "lin" | "win") {
            webgl2_backend.push(TEXTURE_NORM16.to_owned());
        }
        config.extend(context_config("webGl2", webgl2, target_os, &webgl2_backend));

        if let (Some(w1_params), Some(w2_params)) = (
            config.get("webGl:parameters").and_then(Value::as_object).cloned(),
            config.get("webGl2:parameters").and_then(Value::as_object).cloned(),
        ) {
            let mut merged_w1 = w1_params;
            for (k, v) in &w2_params {
                if merged_w1.contains_key(k) && !v.is_string() {
                    merged_w1.insert(k.clone(), v.clone());
                }
            }
            config.insert("webGl:parameters".into(), Value::Object(merged_w1));
            let mut updated_w2 = w2_params;
            if let Some(max_val) = updated_w2.get(MAX_UNIFORM_BLOCK_SIZE_KEY).and_then(Value::as_i64) {
                updated_w2.insert(
                    MAX_UNIFORM_BLOCK_SIZE_KEY.into(),
                    Value::from(max_val.min(MAX_UNIFORM_BLOCK_SIZE_CAP)),
                );
            }
            config.insert("webGl2:parameters".into(), Value::Object(updated_w2));
        }
    }
    config
}

fn webgl_config_for(
    target_os: &str,
    gpu_val: &Value,
    rng: &mut PyRandom,
) -> Result<Map<String, Value>> {
    let w1_list = trace_cached("webgl", target_os, &[("gpu", gpu_val)])?;
    let w1 = choose(rng, &w1_list);
    let w2_list = trace_cached("webgl2", target_os, &[("gpu", gpu_val), ("webgl", &w1.value)])?;
    let w2 = choose(rng, &w2_list);
    Ok(to_config(&w1.value, &w2.value, target_os))
}

pub fn webgl_for_gpu(
    target_os: &str,
    vendor: &str,
    renderer: &str,
    seed: Option<i64>,
) -> Result<Map<String, Value>> {
    let gpus = firefox_gpus(target_os)?;
    if !gpus.iter().any(|(v, r)| v == vendor && r == renderer) {
        let mut sorted = gpus;
        sorted.sort_by(|a, b| compare_py_str(&a.0, &b.0).then_with(|| compare_py_str(&a.1, &b.1)));
        let pairs: Vec<String> = sorted
            .into_iter()
            .map(|(v, r)| format!("('{v}', '{r}')"))
            .collect();
        return Err(CamoufoxError::ValueError(format!(
            "No recorded WebGL data for vendor '{vendor}' and renderer '{renderer}' \
             from Firefox on {}. Possible pairs: [{}]",
            fpgen_os_name(target_os)?,
            pairs.join(", ")
        )));
    }
    let gpu_val = serde_json::json!({
        "vendor": vendor,
        "renderer": renderer,
    });
    let mut rng = PyRandom::new(seed.map(|s| s as u64));
    webgl_config_for(target_os, &gpu_val, &mut rng)
}

pub fn sample_webgl_for_screen(
    target_os: &str,
    width: Option<i64>,
    height: Option<i64>,
    seed: Option<i64>,
    cores: Option<i64>,
    cpu: Option<&BTreeMap<String, Value>>,
) -> Result<Map<String, Value>> {
    let arch = cpu.and_then(|c| c.get("cpu:arch")).and_then(Value::as_str);
    let maker = cpu
        .and_then(|c| c.get("cpu:manufacturer"))
        .and_then(Value::as_str);

    let all_gpus = trace_cached("gpu", target_os, &[])?;
    let fits = |entry: &TraceEntry, software: bool| -> bool {
        let renderer = entry
            .value
            .get("renderer")
            .and_then(Value::as_str)
            .unwrap_or("");
        is_software_renderer(Some(renderer)) == software
            && renderer != RFP_RENDERER
            && gpu_fits_os(Some(renderer), target_os)
            && gpu_fits_machine(Some(renderer), target_os, cores, width, height)
            && gpu_screen_is_plausible(Some(renderer), width, height)
            && gpu_fits_cpu(Some(renderer), target_os, arch, maker)
    };

    let mut candidates: Vec<&TraceEntry> = all_gpus.iter().filter(|r| fits(r, false)).collect();
    if candidates.is_empty()
        && cpu.is_some_and(|c| !c.is_empty())
        && !all_gpus.iter().any(|r| {
            let renderer = r.value.get("renderer").and_then(Value::as_str);
            !is_software_renderer(renderer) && gpu_fits_cpu(renderer, target_os, arch, maker)
        })
    {
        candidates = all_gpus.iter().filter(|r| fits(r, true)).collect();
    }

    if candidates.is_empty() {
        if cpu.is_some_and(|c| !c.is_empty())
            && !all_gpus.iter().any(|r| {
                let renderer = r.value.get("renderer").and_then(Value::as_str);
                gpu_fits_cpu(renderer, target_os, arch, maker)
            })
        {
            let desc = [maker, arch].into_iter().flatten().collect::<Vec<_>>().join(" ");
            return Err(CamoufoxError::ValueError(format!(
                "No GPU fpgen has recorded from Firefox on {target_os} sits beside a {desc} CPU"
            )));
        }
        return Err(CamoufoxError::ValueError(format!(
            "No recorded {target_os} GPU fits a {}x{} screen",
            width.unwrap_or(0),
            height.unwrap_or(0)
        )));
    }

    let mut rng = PyRandom::new(seed.map(|s| s as u64));
    let total: f64 = candidates.iter().map(|c| c.probability).sum();
    let mut roll = rng.random() * total;
    let mut chosen = candidates[candidates.len() - 1];
    for c in &candidates {
        roll -= c.probability;
        if roll <= 0.0 {
            chosen = c;
            break;
        }
    }
    webgl_config_for(target_os, &chosen.value, &mut rng)
}
