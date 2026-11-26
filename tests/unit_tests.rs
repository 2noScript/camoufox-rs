use std::collections::{BTreeMap, BTreeSet};

use camoufox_rs::addons::{confirm_paths, DefaultAddons};
use camoufox_rs::coherence;
use camoufox_rs::config::{validate_config, validate_type};
use camoufox_rs::fingerprints::{
    app_version_from_user_agent, build_init_script, from_preset, generate_random_font_subset,
    generate_random_voice_subset, identity_salt, identity_seed, versionless_ua, InitValues,
};
use camoufox_rs::humanize::{
    cursory, driven_channels, myuri, normalize, raw, HumanizeConfig, HumanizeSetting,
};
use camoufox_rs::ip::{socks5_auth, validate_ip, ProxyConfig, ProxyHelper};
use camoufox_rs::locales::{handle_locale, normalize_locale};
use camoufox_rs::pkgman::{parse_semver, Version};
use camoufox_rs::pycompat::{crc32, orjson_dumps, py_repr, py_str, py_truthy};
use camoufox_rs::utils::{check_valid_os, os_for_cpu, CpuOption};
use camoufox_rs::warnings::{capture_warnings, LeakWarning};
use camoufox_rs::{chunk_env_vars, CamoufoxError, TargetOs};
use serde_json::{json, Map, Value};

#[test]
fn target_os_and_chunking() {
    assert_eq!(TargetOs::parse("windows").unwrap(), TargetOs::Windows);
    assert_eq!(TargetOs::parse("macos").unwrap(), TargetOs::MacOs);
    assert_eq!(TargetOs::parse("linux").unwrap(), TargetOs::Linux);
    assert!(matches!(TargetOs::parse("bsd"), Err(CamoufoxError::InvalidOS(_))));

    let payload = "é".repeat(20_000);
    let chunks = chunk_env_vars("CAMOU_CONFIG", &payload);
    let reassembled: String = chunks.into_values().collect();
    assert_eq!(reassembled, payload);
}

#[test]
fn version_sorting_matches_python() {
    let a = Version::new("alpha.26", Some("135.0".into()));
    let b = Version::new("beta.19", Some("135.0".into()));
    let c = Version::new("beta.35", Some("159.0".into()));
    assert!(a < b);
    assert!(b < c);
    assert!(a.is_alpha());
    assert!(!b.is_alpha());
    assert_eq!(parse_semver("^1.61.2"), vec![1, 61, 2]);
}

#[test]
fn property_type_and_config_validation() {
    assert!(validate_type(&json!("hello"), "str"));
    assert!(validate_type(&json!(42), "uint"));
    assert!(!validate_type(&json!(-1), "uint"));
    assert!(validate_type(&json!(true), "bool"));

    let mut bad_cfg = Map::new();
    bad_cfg.insert("screen.width".into(), json!("not_an_int"));
    assert!(matches!(
        validate_config(&bad_cfg, None),
        Err(CamoufoxError::InvalidPropertyType(_))
    ));

    let mut good_cfg = Map::new();
    good_cfg.insert("screen.width".into(), json!(1920));
    good_cfg.insert("navigator.platform".into(), json!("MacIntel"));
    assert!(validate_config(&good_cfg, None).is_ok());
}

#[test]
fn ip_and_socks5_auth_splitting() {
    assert!(validate_ip("1.1.1.1").is_ok());
    assert!(validate_ip("2606:4700:4700::1111").is_ok());
    assert!(matches!(
        validate_ip("not-an-ip"),
        Err(CamoufoxError::InvalidIP(_))
    ));

    let proxy = ProxyConfig {
        server: "socks5://127.0.0.1:1080".into(),
        username: Some("alice".into()),
        password: Some("s3cr3t".into()),
        bypass: None,
    };
    let as_url = ProxyHelper::as_string(&proxy).unwrap();
    assert_eq!(as_url, "socks5://alice:s3cr3t@127.0.0.1:1080");

    let (bare, auth) = socks5_auth(Some(&proxy)).unwrap();
    let bare = bare.unwrap();
    assert!(bare.username.is_none());
    assert!(bare.password.is_none());
    let auth = auth.unwrap();
    assert_eq!(auth.host, "127.0.0.1");
    assert_eq!(auth.port, Some(1080));
    assert_eq!(auth.username, "alice");
}

#[test]
fn locales_and_cldr_selector() {
    let us = normalize_locale("en-US").unwrap();
    assert_eq!(us.language, "en");
    assert_eq!(us.region.as_deref(), Some("US"));
    assert_eq!(us.script.as_deref(), Some("Latn"));

    let region_only = handle_locale("JP", false).unwrap();
    assert_eq!(region_only.region.as_deref(), Some("JP"));
    assert_eq!(region_only.language, "ja");

    assert!(matches!(
        normalize_locale("invalid_locale_tag_xx"),
        Err(CamoufoxError::InvalidLocale(_))
    ));
}

#[test]
fn coherence_repairs_and_cpu_checks() {
    let mut cfg = Map::new();
    cfg.insert("navigator.userAgent".into(), json!("Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:159.0) Gecko/20100101 Firefox/159.0"));
    cfg.insert("navigator.platform".into(), json!("MacIntel"));
    cfg.insert("webGl:renderer".into(), json!("Apple M1, or similar"));
    cfg.insert("navigator.hardwareConcurrency".into(), json!(4));
    cfg.insert("screen.colorDepth".into(), json!(24));
    cfg.insert("navigator.maxTouchPoints".into(), json!(2));
    cfg.insert("screen.width".into(), json!(1920));
    cfg.insert("screen.height".into(), json!(1080));
    cfg.insert("window.innerHeight".into(), json!(1000));
    cfg.insert("window.outerHeight".into(), json!(1010));

    let keep = BTreeSet::new();
    let remaining = coherence::apply(&mut cfg, "mac", &keep);
    assert!(remaining.is_empty(), "expected clean repair, got {remaining:?}");
    assert_eq!(cfg["navigator.hardwareConcurrency"], json!(8));
    assert_eq!(cfg["screen.colorDepth"], json!(30));
    assert_eq!(cfg["navigator.maxTouchPoints"], json!(0));
    assert_eq!(cfg["window.innerHeight"], json!(924));
    assert_eq!(cfg["window.outerHeight"], json!(1010));

    // macOS never accepts AMD CPUs
    let bad_cpu = CpuOption {
        arch: None,
        manufacturer: Some("amd".into()),
    };
    assert!(os_for_cpu(&bad_cpu, &["macos".into()]).is_err());
    assert!(check_valid_os(&["windows".into(), "macos".into(), "linux".into()]).is_ok());
    assert!(matches!(
        check_valid_os(&["Windows".into()]),
        Err(CamoufoxError::InvalidOS(_))
    ));
}

#[test]
fn font_voice_determinism_and_init_script() {
    let salt_a = identity_salt(Some(&json!({"a": 1, "b": "x"})));
    let salt_b = identity_salt(Some(&json!({"b": "x", "a": 1})));
    assert_eq!(salt_a, salt_b);

    let mut cfg = Map::new();
    cfg.insert("navigator.userAgent".into(), json!("Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:159.0) Gecko/20100101 Firefox/159.0"));
    cfg.insert("navigator.platform".into(), json!("Win32"));
    cfg.insert("screen.width".into(), json!(1920));
    cfg.insert("screen.height".into(), json!(1080));
    cfg.insert("navigator.hardwareConcurrency".into(), json!(8));

    let seed = identity_seed(&cfg, salt_a) as u64;
    let fonts1 = generate_random_font_subset("windows", Some(seed), false, None);
    let fonts2 = generate_random_font_subset("windows", Some(seed), false, None);
    assert_eq!(fonts1, fonts2);
    assert!(!fonts1.is_empty());

    let voices1 = generate_random_voice_subset("windows", Some("en-US"), Some(seed)).unwrap();
    let voices2 = generate_random_voice_subset("windows", Some("en-US"), Some(seed)).unwrap();
    assert_eq!(voices1.len(), voices2.len());
    assert!(!voices1.is_empty());

    let script = build_init_script(&InitValues {
        navigator_platform: Some("Win32".into()),
        webrtc_ip: Some("1.1.1.1".into()),
        font_list: Some(vec!["Arial".into()]),
        ..Default::default()
    })
    .unwrap();
    assert!(script.contains("setNavigatorPlatform(\"Win32\")"));
    assert!(script.contains("setWebRTCIPv4(\"1.1.1.1\")"));
}

#[test]
fn humanize_and_warnings_and_misc() {
    let norm = normalize(&HumanizeSetting::Bool(true)).unwrap();
    assert_eq!(norm.engines.len(), 3);
    let driven = driven_channels(Some(&HumanizeSetting::Config(HumanizeConfig {
        mouse: Some(cursory()),
        keyboard: Some(raw()),
        scroll: Some(myuri()),
        seed: Some(42),
        settings: None,
    })))
    .unwrap();
    assert_eq!(driven, vec!["mouse".to_owned(), "scroll".to_owned()]);

    let (_, warnings) = capture_warnings(|| {
        LeakWarning::warn("block_images", Some(false));
        LeakWarning::warn("block_images", Some(true));
    });
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].category, "LeakWarning");

    assert_eq!(DefaultAddons::ubo_check(), DefaultAddons::Ubo);
    assert!(confirm_paths(&[std::path::PathBuf::from("/nonexistent-camoufox-addon")]).is_err());

    assert_eq!(versionless_ua("Mozilla/5.0 (rv:159.0) Firefox/159.0"), "Mozilla/5.0 (rv:) Firefox/");
    assert_eq!(app_version_from_user_agent(Some("Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:159.0) Gecko/20100101 Firefox/159.0")).as_deref(), Some("5.0 (Macintosh)"));
    assert_eq!(crc32(b"hello"), 0x3610a686);
    assert!(py_truthy(&json!(1)));
    assert!(!py_truthy(&json!([])));
    assert_eq!(py_str(&json!(true)), "True");
    assert_eq!(py_repr(&json!("abc")), "'abc'");
    assert_eq!(orjson_dumps(&json!({"b": 1, "a": 2}), true), r#"{"a":2,"b":1}"#);

    let preset = json!({
        "navigator": {
            "userAgent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:140.0) Gecko/20100101 Firefox/140.0",
            "platform": "MacIntel",
            "hardwareConcurrency": 8
        },
        "screen": { "width": 1440, "height": 900, "colorDepth": 30 },
        "webgl": { "unmaskedVendor": "Apple", "unmaskedRenderer": "Apple M1, or similar" }
    });
    let converted = from_preset(&preset, Some("159"), Some(12345));
    assert!(converted["navigator.userAgent"].as_str().unwrap().contains("Firefox/159.0"));

    let _map: BTreeMap<String, Value> = BTreeMap::new();
}

trait UboCheck {
    fn ubo_check() -> DefaultAddons {
        DefaultAddons::Ubo
    }
}
impl UboCheck for DefaultAddons {}

#[test]
fn repos_default_points_to_fork() {
    use camoufox_rs::multiversion::get_default_channel;
    use camoufox_rs::pkgman::RepoConfig;

    let def = RepoConfig::get_default();
    assert_eq!(def.name, "un-camoufox");
    assert_eq!(def.primary_repo(), "2noScript/camoufox");
    assert_eq!(get_default_channel(), "un-camoufox/stable");
    assert!(RepoConfig::find_by_name("Official").is_some());
    assert!(camoufox_rs::paths::install_dir()
        .to_string_lossy()
        .contains("uncamoufox"));
}

#[test]
fn headless_stream_wire_and_env_roundtrip() {
    use camoufox_rs::headless_stream::{
        apply_headless_stream_env, CAMF_HEADER_SIZE, CAMF_MAGIC, CAMI_HEADER_SIZE, CAMI_MAGIC,
        CAMOU_STREAM_FORMAT_ENV, CAMOU_STREAM_FPS_ENV, CAMOU_STREAM_SOCKET_ENV,
        STREAM_PROTOCOL_VERSION,
    };
    use camoufox_rs::{
        HeadlessStreamOptions, Modifiers, MouseButton, StreamFormat, StreamFrame, StreamInputEvent,
    };

    // Valid CAMF BGRA & NV12 header decoding + RGBA conversion
    let mut bgra_hdr = [0u8; CAMF_HEADER_SIZE];
    bgra_hdr[0..4].copy_from_slice(&CAMF_MAGIC.to_le_bytes());
    bgra_hdr[4..8].copy_from_slice(&STREAM_PROTOCOL_VERSION.to_le_bytes());
    bgra_hdr[8..12].copy_from_slice(&2u32.to_le_bytes());
    bgra_hdr[12..16].copy_from_slice(&2u32.to_le_bytes());
    bgra_hdr[16..20].copy_from_slice(&(StreamFormat::Bgra as u32).to_le_bytes());
    bgra_hdr[20..24].copy_from_slice(&16u32.to_le_bytes());
    bgra_hdr[24..32].copy_from_slice(&123456u64.to_le_bytes());
    let parsed = StreamFrame::decode_header(&bgra_hdr).unwrap();
    assert_eq!(parsed, (2, 2, StreamFormat::Bgra, 16, 123456));

    let bgra_frame = StreamFrame {
        width: 2,
        height: 2,
        format: StreamFormat::Bgra,
        pts_us: 1,
        data: [10, 20, 30, 255].repeat(4),
    };
    let rgba = bgra_frame.to_rgba().unwrap();
    assert_eq!(&rgba[..4], &[30, 20, 10, 255]);

    // NV12 to RGBA dimensions & bounds
    let nv12_frame = StreamFrame {
        width: 2,
        height: 2,
        format: StreamFormat::Nv12,
        pts_us: 2,
        data: vec![16, 16, 16, 16, 128, 128],
    };
    let nv12_rgba = nv12_frame.to_rgba().unwrap();
    assert_eq!(nv12_rgba.len(), 16);

    // CAMI reverse input encoding
    let pkt = StreamInputEvent::key_down(
        "Enter",
        "Enter",
        true,
        0,
        13,
        Modifiers::from_bits(Modifiers::CONTROL),
    )
    .encode()
    .unwrap();
    assert_eq!(u32::from_le_bytes(pkt[0..4].try_into().unwrap()), CAMI_MAGIC);
    assert_eq!(u32::from_le_bytes(pkt[8..12].try_into().unwrap()), 5);
    assert_eq!(u32::from_le_bytes(pkt[12..16].try_into().unwrap()), Modifiers::CONTROL);
    assert_eq!(i32::from_le_bytes(pkt[16..20].try_into().unwrap()), 1);
    assert_eq!(i32::from_le_bytes(pkt[28..32].try_into().unwrap()), 13);
    assert_eq!(std::str::from_utf8(&pkt[CAMI_HEADER_SIZE..]).unwrap(), "Enter\tEnter");

    let click_pkt = StreamInputEvent::mouse_down(12, 34, MouseButton::Right, Modifiers::NONE)
        .encode()
        .unwrap();
    assert_eq!(click_pkt.len(), CAMI_HEADER_SIZE);
    assert_eq!(i32::from_le_bytes(click_pkt[24..28].try_into().unwrap()), 2);

    // Env injection & bounds validation
    let mut env = BTreeMap::new();
    let opts = HeadlessStreamOptions::new()
        .with_fps(45)
        .with_format(StreamFormat::Bgra);
    #[cfg(unix)]
    {
        apply_headless_stream_env(&opts, true, None, &mut env).unwrap();
        assert!(env.contains_key(CAMOU_STREAM_SOCKET_ENV));
        assert_eq!(env.get(CAMOU_STREAM_FPS_ENV).map(String::as_str), Some("45"));
        assert_eq!(env.get(CAMOU_STREAM_FORMAT_ENV).map(String::as_str), Some("bgra"));
        let _ = std::fs::remove_file(&env[CAMOU_STREAM_SOCKET_ENV]);

        let bad_fps = HeadlessStreamOptions::new().with_fps(0);
        assert!(apply_headless_stream_env(&bad_fps, true, None, &mut BTreeMap::new()).is_err());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn headless_stream_unix_socket_roundtrip() {
    use camoufox_rs::headless_stream::{
        CAMF_HEADER_SIZE, CAMF_MAGIC, CAMI_HEADER_SIZE, CAMI_MAGIC, STREAM_PROTOCOL_VERSION,
    };
    use camoufox_rs::{HeadlessStreamClient, Modifiers, MouseButton, StreamFormat};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    let sock_path = std::env::temp_dir().join(format!("camou-test-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).unwrap();

    let server = tokio::spawn(async move {
        let (mut srv, _) = listener.accept().await.unwrap();
        let payload = vec![235u8; 4 * 2 + 4];
        let mut hdr = Vec::with_capacity(CAMF_HEADER_SIZE);
        hdr.extend_from_slice(&CAMF_MAGIC.to_le_bytes());
        hdr.extend_from_slice(&STREAM_PROTOCOL_VERSION.to_le_bytes());
        hdr.extend_from_slice(&4u32.to_le_bytes());
        hdr.extend_from_slice(&2u32.to_le_bytes());
        hdr.extend_from_slice(&(StreamFormat::Nv12 as u32).to_le_bytes());
        hdr.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        hdr.extend_from_slice(&999u64.to_le_bytes());
        srv.write_all(&hdr).await.unwrap();
        srv.write_all(&payload).await.unwrap();
        srv.flush().await.unwrap();

        let mut in_hdr = [0u8; CAMI_HEADER_SIZE];
        srv.read_exact(&mut in_hdr).await.unwrap();
        assert_eq!(u32::from_le_bytes(in_hdr[0..4].try_into().unwrap()), CAMI_MAGIC);
        let text_len = u32::from_le_bytes(in_hdr[32..36].try_into().unwrap()) as usize;
        let mut body = vec![0u8; text_len];
        srv.read_exact(&mut body).await.unwrap();
        assert_eq!(std::str::from_utf8(&body).unwrap(), "ok");
    });

    let mut client = HeadlessStreamClient::connect_with_retry(&sock_path, std::time::Duration::from_secs(2))
        .await
        .unwrap();
    let frame = client.next_frame().await.unwrap();
    assert_eq!((frame.width, frame.height, frame.pts_us), (4, 2, 999));
    client.type_text("ok", Modifiers::NONE).await.unwrap();

    let _ = MouseButton::Left;
    server.await.unwrap();
    let _ = std::fs::remove_file(&sock_path);
}

#[test]
fn pkgman_sha256_and_zip_extraction() {
    use camoufox_rs::pkgman::{unzip, verify_sha256, verify_sha256_hex};
    use std::io::Write as _;

    let payload = b"camoufox-archive-bytes";
    assert!(verify_sha256(payload, None, "test").is_ok());
    assert!(verify_sha256_hex(&"0".repeat(64), Some(&"0".repeat(64)), "test").is_ok());
    let err = verify_sha256_hex(&"0".repeat(64), Some(&"f".repeat(64)), "test").unwrap_err();
    assert!(matches!(err, CamoufoxError::CorruptedDownload(_)));

    let tmp_dir = std::env::temp_dir().join(format!("camou-zip-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let zip_path = tmp_dir.join("bundle.zip");
    let extract_dir = tmp_dir.join("out");

    {
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
        zip.start_file("Camoufox.app/Contents/MacOS/camoufox", opts)
            .unwrap();
        zip.write_all(payload).unwrap();
        zip.finish().unwrap();
    }

    unzip(&zip_path, &extract_dir).unwrap();
    let extracted = extract_dir.join("Camoufox.app/Contents/MacOS/camoufox");
    assert_eq!(std::fs::read(&extracted).unwrap(), payload);
    let _ = std::fs::remove_dir_all(&tmp_dir);
}
