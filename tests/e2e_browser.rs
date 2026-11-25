use std::path::PathBuf;

use camoufox_rs::addons::DefaultAddons;
use camoufox_rs::{camoufox, CamoufoxOptions, HeadlessMode, NewContextOptions};

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
        executable_path: Some(bin),
        headless: HeadlessMode::Bool(true),
        exclude_addons: vec![DefaultAddons::Ubo],
        i_know_what_im_doing: true,
        ..Default::default()
    })
    .await
    .expect("launch Camoufox");

    let ctx1 = browser
        .new_context(NewContextOptions::default())
        .await
        .expect("ctx1");
    let page1 = ctx1.new_page().await.expect("page1");
    let ua1: String = page1
        .evaluate::<(), String>("() => navigator.userAgent", None)
        .await
        .expect("eval ua1");
    assert!(ua1.contains("Firefox/"));

    browser.close().await.expect("close browser");
}
