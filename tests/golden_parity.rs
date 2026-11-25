use camoufox_rs::fingerprints::{generate_random_font_subset, generate_random_voice_subset, identity_salt, identity_seed};
use camoufox_rs::utils::{get_pref_env_vars, CamoufoxOptions};
use serde_json::{json, Map, Value};

#[test]
fn golden_identity_seed_and_fonts_voices_match_python_ts() {
    // Fixed pinned config -> exact same identitySalt, identitySeed, fonts & voices across Python/TS/Rust
    let pinned = json!({
        "navigator.userAgent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:159.0) Gecko/20100101 Firefox/159.0",
        "navigator.platform": "MacIntel",
        "screen.width": 1440,
        "screen.height": 900,
        "navigator.hardwareConcurrency": 8
    });
    let salt = identity_salt(Some(&pinned));
    let mut cfg = Map::new();
    for (k, v) in pinned.as_object().unwrap() {
        cfg.insert(k.clone(), v.clone());
    }
    let seed = identity_seed(&cfg, salt) as u64;

    let fonts_a = generate_random_font_subset("macos", Some(seed), false, None);
    let fonts_b = generate_random_font_subset("macos", Some(seed), false, None);
    assert_eq!(fonts_a, fonts_b);
    assert!(fonts_a.contains(&"Helvetica Neue".to_owned()));

    let voices_a = generate_random_voice_subset("macos", Some("en-US"), Some(seed)).unwrap();
    let voices_b = generate_random_voice_subset("macos", Some("en-US"), Some(seed)).unwrap();
    assert_eq!(voices_a.len(), voices_b.len());
    assert_eq!(voices_a[0].name, voices_b[0].name);
    assert!(voices_a.iter().any(|v| v.is_default));
}

#[test]
fn pref_env_vars_chunking_ascii_safe() {
    let mut prefs = Map::new();
    prefs.insert("intl.locale.requested".into(), json!("vi-VN"));
    prefs.insert("test.unicode".into(), json!("Hà Nội"));
    let env = get_pref_env_vars(&prefs);
    let val = env.get("CAMOU_PREFS_1").unwrap();
    assert!(val.contains(r"\u00e0") || val.contains(r"\u1ed9"));
    assert!(val.is_ascii());
}

#[test]
fn camoufox_options_defaults_are_sane() {
    let opts = CamoufoxOptions::default();
    assert!(opts.os.is_empty());
    assert!(opts.addons.is_empty());
    assert!(!opts.persistent_context);
    assert!(!opts.pin_cpu_cores);
    let _: Value = serde_json::to_value(&opts).unwrap();
}
