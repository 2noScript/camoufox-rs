use std::path::PathBuf;

use camoufox_rs::addons::DefaultAddons;
use camoufox_rs::humanize::HumanizeSetting;
use camoufox_rs::humanize_playwright::HumanizedPage;
use camoufox_rs::{
    camoufox, launch_server, CamoufoxOptions, HeadlessMode, HeadlessStreamOptions, Modifiers,
    MouseButton, NewContextOptions, StreamFormat,
};

/// End-to-end launch verification using a local Camoufox binary.
/// Run with:
/// `CAMOUFOX_TEST_BIN=/path/to/camoufox cargo test --test e2e_browser -- --ignored --nocapture`
#[tokio::test]
#[ignore = "Requires local Camoufox binary and fpgen model"]
async fn e2e_launch_and_new_context_isolation() {
    let bin = std::env::var("CAMOUFOX_TEST_BIN")
        .map(PathBuf::from)
        .expect("CAMOUFOX_TEST_BIN must be set");

    let mut browser = camoufox(CamoufoxOptions {
        executable_path: Some(bin.clone()),
        headless: HeadlessMode::Bool(true),
        exclude_addons: vec![DefaultAddons::Ubo],
        humanize: Some(HumanizeSetting::Bool(true)),
        canvas_noise: Some(true),
        i_know_what_im_doing: true,
        ..Default::default()
    })
    .await
    .expect("launch Camoufox");

    let ctx1 = browser
        .new_context(NewContextOptions {
            os: Some("windows".into()),
            ..Default::default()
        })
        .await
        .expect("ctx1");
    let page1 = ctx1.new_page().await.expect("page1");
    let ua1: String = page1
        .evaluate::<(), String>("() => navigator.userAgent", None)
        .await
        .expect("eval ua1");
    assert!(ua1.contains("Firefox/"));
    assert!(ua1.contains("Windows NT"));

    // Exercise myuri-rs humanize cursor clicking a real button inside Camoufox.
    // DOM attributes are visible from both the page's main world and Juggler's isolated world.
    page1
        .set_content(
            "<button id='btn' style='position:fixed;left:40px;top:40px;width:120px;height:40px' \
             onclick=\"this.dataset.clicked=(Number(this.dataset.clicked||0)+1)\">Click</button>",
            None,
        )
        .await
        .expect("set_content");
    let mut hp = HumanizedPage::attach(page1.clone(), Some(&HumanizeSetting::Bool(true)), "win")
        .expect("attach humanized");
    hp.click_selector("#btn").await.expect("humanized click");
    let clicked: String = page1
        .evaluate::<(), String>("() => document.querySelector('#btn').dataset.clicked || '0'", None)
        .await
        .expect("eval clicked");
    assert_eq!(clicked, "1");

    browser.close().await.expect("close browser");

    // Verify persistent_context propagates CAMOU_CONFIG_* env vars and clean server bind/unbind.
    let tmp_dir = std::env::temp_dir().join(format!("camoufox-e2e-persist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).expect("mkdir tmp");
    let mut persist = camoufox(CamoufoxOptions {
        executable_path: Some(bin.clone()),
        os: vec!["windows".into()],
        headless: HeadlessMode::Bool(true),
        persistent_context: true,
        user_data_dir: Some(tmp_dir.clone()),
        exclude_addons: vec![DefaultAddons::Ubo],
        i_know_what_im_doing: true,
        ..Default::default()
    })
    .await
    .expect("launch persistent Camoufox");
    let p_page = persist.new_page().await.expect("persistent new_page");
    let p_plat: String = p_page
        .evaluate::<(), String>("() => navigator.platform", None)
        .await
        .expect("eval persistent platform");
    assert_eq!(p_plat, "Win32");
    persist.close().await.expect("close persistent");
    let _ = std::fs::remove_dir_all(&tmp_dir);

    let mut srv = launch_server(CamoufoxOptions {
        executable_path: Some(bin),
        headless: HeadlessMode::Bool(true),
        exclude_addons: vec![DefaultAddons::Ubo],
        i_know_what_im_doing: true,
        ..Default::default()
    })
    .await
    .expect("launch_server");
    assert!(srv.ws_endpoint.starts_with("ws://"));
    srv.close().await.expect("close server");
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "Requires local Camoufox binary with headless-stream patch"]
async fn e2e_headless_stream_capture_and_input() {
    let bin = std::env::var("CAMOUFOX_TEST_BIN")
        .map(PathBuf::from)
        .expect("CAMOUFOX_TEST_BIN must be set");

    let mut browser = camoufox(CamoufoxOptions {
        executable_path: Some(bin),
        os: vec!["macos".into()],
        headless: HeadlessMode::Bool(true),
        headless_stream: Some(
            HeadlessStreamOptions::new()
                .with_fps(30)
                .with_format(StreamFormat::Nv12),
        ),
        exclude_addons: vec![DefaultAddons::Ubo],
        i_know_what_im_doing: true,
        ..Default::default()
    })
    .await
    .expect("launch Camoufox with headless_stream");

    let sock = browser
        .headless_stream_socket()
        .expect("stream socket injected");
    let mut stream = browser
        .connect_stream()
        .await
        .expect("connect HeadlessStreamClient");

    let page = browser.new_page().await.expect("new_page");
    page.set_content(
        "<input id='inp' style='position:fixed;inset:0;width:100vw;height:100vh' />",
        None,
    )
    .await
    .expect("set_content");

    let frame = stream.next_frame().await.expect("receive NV12 frame");
    assert!(frame.width >= 200 && frame.height >= 200);
    assert_eq!(frame.format, StreamFormat::Nv12);
    assert_eq!(frame.to_rgba().unwrap().len(), (frame.width * frame.height * 4) as usize);

    stream
        .click(200, 200, MouseButton::Left, Modifiers::NONE)
        .await
        .expect("send click");
    stream
        .type_text("rs-ok", Modifiers::NONE)
        .await
        .expect("send text");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let val: String = page
            .evaluate::<(), String>("() => document.getElementById('inp').value", None)
            .await
            .unwrap_or_default();
        if val == "rs-ok" {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "typed value did not reach page, got {val:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    browser.close().await.expect("close browser");
    assert!(!sock.exists(), "auto stream socket should be cleaned up on close");
}
