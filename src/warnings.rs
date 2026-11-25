//! Leak and fallback warnings backed by `data-files/warnings.yml`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::paths::load_data_value;
use crate::version::VERSION;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedWarning {
    pub category: String,
    pub message: String,
}

static RECORDER: OnceLock<Mutex<Option<Vec<RecordedWarning>>>> = OnceLock::new();

fn recorder_lock() -> &'static Mutex<Option<Vec<RecordedWarning>>> {
    RECORDER.get_or_init(|| Mutex::new(None))
}

pub fn load_warnings() -> &'static HashMap<String, String> {
    static DATA: OnceLock<HashMap<String, String>> = OnceLock::new();
    DATA.get_or_init(|| {
        let val = load_data_value("warnings.yml").expect("warnings.yml bundled");
        serde_json::from_value(val).expect("warnings.yml must be a string map")
    })
}

pub fn warn(message: &str, category: &str) {
    if let Ok(mut guard) = recorder_lock().lock() {
        if let Some(buf) = guard.as_mut() {
            buf.push(RecordedWarning {
                category: category.to_owned(),
                message: message.to_owned(),
            });
            return;
        }
    }
    eprintln!("[{category}] {message}");
}

pub fn capture_warnings<F, T>(f: F) -> (T, Vec<RecordedWarning>)
where
    F: FnOnce() -> T,
{
    {
        let mut guard = recorder_lock().lock().expect("warning recorder poisoned");
        *guard = Some(Vec::new());
    }
    let result = f();
    let captured = {
        let mut guard = recorder_lock().lock().expect("warning recorder poisoned");
        guard.take().unwrap_or_default()
    };
    (result, captured)
}

pub struct LeakWarning;

impl LeakWarning {
    pub fn warn(warning_key: &str, i_know_what_im_doing: Option<bool>) {
        if i_know_what_im_doing == Some(true) {
            return;
        }
        let mut msg = load_warnings()
            .get(warning_key)
            .cloned()
            .unwrap_or_else(|| warning_key.to_owned());
        if i_know_what_im_doing.is_some() {
            msg.push_str("\nIf this is intentional, pass `i_know_what_im_doing: true`.");
        }
        warn(&msg, "LeakWarning");
    }
}

pub struct FallbackWarning;

impl FallbackWarning {
    pub fn warn(what: &str, instead: &str, error: &str, identity: Option<&str>) {
        let mut lines = vec![
            format!("camoufox: {VERSION} (rust)"),
            format!("os: {}-{}", std::env::consts::OS, std::env::consts::ARCH),
            format!("error: {error}"),
        ];
        if let Some(id) = identity {
            lines.push(format!("identity: {id}"));
        }
        let report = lines
            .into_iter()
            .map(|l| format!("    {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        let tmpl = load_warnings()
            .get("fallback")
            .map(String::as_str)
            .unwrap_or("{what} failed; using {instead}\n{report}");
        let msg = tmpl
            .replace("{what}", what)
            .replace("{instead}", instead)
            .replace("{report}", &report);
        warn(&msg, "FallbackWarning");
    }
}
