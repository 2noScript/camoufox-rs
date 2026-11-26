# camoufox-rs

Rust launcher SDK and CLI for the [Camoufox](https://camoufox.com) anti-detect Firefox browser, parity-aligned with `python/src/camoufox/` and `typescript/src/` and powered by:

- [`fpgen-rs`](https://github.com/2noScript/fpgen-rs) — Bayesian network fingerprint generator
- [`myuri-rs`](https://github.com/2noScript/myuri-rs) — biomechanical mouse, wheel, and typing humanizer
- [`playwright-rs`](https://crates.io/crates/playwright-rs) (`~0.16`, Playwright `1.62.1`) + [`tokio`](https://tokio.rs)

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
camoufox-rs = { git = "https://github.com/2noScript/camoufox-rs" }
tokio = { version = "1", features = ["full"] }
```

Install the `camoufox` CLI and download the pinned browser release + GeoIP database:

```bash
cargo install --path .
camoufox fetch
```

If you want to run against a custom/local build instead of the downloaded release, set `executable_path: Some(...)` or export `CAMOUFOX_EXECUTABLE_PATH=/path/to/camoufox`.

## Quick Start

### Async (`camoufox` / `new_browser`)

```rust
use camoufox_rs::{camoufox, CamoufoxOptions, HeadlessMode};

#[tokio::main]
async fn main() -> camoufox_rs::Result<()> {
    let mut browser = camoufox(CamoufoxOptions {
        os: vec!["macos".into()],
        headless: HeadlessMode::Bool(true),
        ..Default::default()
    })
    .await?;

    let page = browser.new_page().await?;
    page.goto("https://example.com", None).await?;
    let ua: String = page.evaluate::<(), String>("() => navigator.userAgent", None).await?;
    println!("User-Agent: {ua}");

    browser.close().await?;
    Ok(())
}
```

### Sync (`camoufox_sync`)

```rust
use camoufox_rs::{camoufox_sync, CamoufoxOptions, HeadlessMode};

fn main() -> camoufox_rs::Result<()> {
    let mut browser = camoufox_sync(CamoufoxOptions {
        os: vec!["windows".into()],
        headless: HeadlessMode::Bool(true),
        ..Default::default()
    })?;

    let page = browser.new_page()?;
    let ua: String = browser.block_on(page.evaluate::<(), String>("() => navigator.userAgent", None))?;
    println!("Sync User-Agent: {ua}");

    browser.close()?;
    Ok(())
}
```

## Controlling Window Size & Screen Bounds

By default, `fpgen-rs` samples a real Firefox device profile, including its `screen` and `window` dimensions, so each launch can open at a different size. To fix or constrain the window size without breaking identity coherence:

```rust
use camoufox_rs::{camoufox, CamoufoxOptions, Screen};

#[tokio::main]
async fn main() -> camoufox_rs::Result<()> {
    // Option A: Fix outer window dimensions while keeping randomized fingerprint traits
    let mut fixed = camoufox(CamoufoxOptions {
        window: Some((1280, 800)),
        ..Default::default()
    })
    .await?;
    fixed.close().await?;

    // Option B: Constrain the drawn screen resolution range
    let mut bounded = camoufox(CamoufoxOptions {
        screen: Some(Screen {
            min_width: Some(1280),
            max_width: Some(1920),
            min_height: Some(720),
            max_height: Some(1080),
        }),
        ..Default::default()
    })
    .await?;
    bounded.close().await?;
    Ok(())
}
```

> **Note:** Do not manually override `viewport` or `window.*` keys in `config` when window spoofing is enabled — Camoufox defaults to `no_viewport: true` so the page observes a real desktop window matching the spoofed screen.

## Humanized Mouse, Keyboard & Scroll (`myuri-rs`)

Camoufox keeps the browser C++ layer free of synthetic delays; all human motion paths, overshoot, log-normal click hold/dwell timing, Markov typing, and mouse-wheel strokes are planned on the driver side via `myuri-rs` (with embedded `cursory-js` recorded human trajectories).

Enable humanization on `CamoufoxOptions`, then drive actions using `browser.cursor(&page)` (`myuri_rs::Cursor`) and/or `HumanizedPage`:

```rust
use camoufox_rs::humanize::{myuri, raw, HumanizeConfig, HumanizeSetting};
use camoufox_rs::humanize_playwright::HumanizedPage;
use camoufox_rs::myuri_rs::{ClickOptions, MouseButton, MoveToOptions, ScrollOptions, Vector};
use camoufox_rs::{camoufox, CamoufoxOptions, HeadlessMode};
use serde_json::json;

#[tokio::main]
async fn main() -> camoufox_rs::Result<()> {
    // Enable all channels (mouse + keyboard + scroll):
    let humanize_all = HumanizeSetting::Bool(true);

    // Or configure channels individually with a deterministic seed & speed tuning:
    let _custom_humanize = HumanizeSetting::Config(HumanizeConfig {
        mouse: Some(myuri()),
        keyboard: Some(myuri()),
        scroll: Some(raw()),
        seed: Some(1234),
        settings: Some(json!({
            "typing": { "wpm": 70 },
            "mouse": { "speedFactor": 1.0 }
        })),
    });

    let mut browser = camoufox(CamoufoxOptions {
        os: vec!["macos".into()],
        headless: HeadlessMode::Bool(false),
        humanize: Some(humanize_all.clone()),
        window: Some((1280, 820)),
        ..Default::default()
    })
    .await?;

    let page = browser.new_page().await?;
    page.goto("https://example.com", None).await?;

    // 1) Low-level & selector-based cursor control (`myuri_rs::Cursor`)
    let mut cursor = browser.cursor(&page).await?;
    cursor.set_cursor_location(Vector::new(40.0, 40.0)).await?;

    // Move by coordinates or hover a selector (with natural curve + overshoot)
    cursor.move_to(Vector::new(320.0, 220.0), Some(MoveToOptions::default())).await?;
    cursor.move_to_selector("a", None).await?;

    // Scroll an element into view cleanly, click, double-click, right-click, or drag
    cursor.scroll("a", Some(ScrollOptions::default())).await?;
    cursor.click(Some("a"), None).await?;

    // Right click or double click example:
    let _right_click = ClickOptions {
        button: Some(MouseButton::Right),
        click_count: Some(1),
        ..Default::default()
    };
    let _double_click = ClickOptions {
        click_count: Some(2),
        ..Default::default()
    };
    // cursor.click(Some("#target"), Some(_double_click)).await?;
    // cursor.drag("#source", "#dropzone", None).await?;

    // 2) Channel-routed page wrapper (`HumanizedPage`, respects `myuri()` vs `raw()`)
    let mut hp = HumanizedPage::attach(page.clone(), Some(&humanize_all), "mac")?;
    hp.move_to(400.0, 300.0).await?;
    hp.wheel(0.0, 240.0).await?;
    // hp.click_selector("#input").await?;
    // hp.type_text("Hello Camoufox").await?;
    // hp.press("Enter").await?;

    browser.close().await?;
    Ok(())
}
```

### Runnable Mouse & Web Examples

Two complete runnable examples are included in `examples/`:

```bash
# Clean multi-page store login, hover, search, scroll, and cart workflow (no overlays)
cargo run --example web_automation_demo

# Visual mouse trajectory playground drawing real-time paths on canvas
cargo run --example visual_mouse
```

## Multi-Context Identity Isolation

Pass `canvas_noise: Some(true)` when creating contexts whose OS or GPU differs from the host machine:

```rust
use camoufox_rs::{camoufox, CamoufoxOptions, NewContextOptions};

#[tokio::main]
async fn main() -> camoufox_rs::Result<()> {
    let mut browser = camoufox(CamoufoxOptions {
        canvas_noise: Some(true),
        i_know_what_im_doing: true,
        ..Default::default()
    })
    .await?;

    let ctx = browser
        .new_context(NewContextOptions {
            os: Some("windows".into()),
            locale: Some("en-US".into()),
            timezone_id: Some("America/New_York".into()),
            ..Default::default()
        })
        .await?;

    let page = ctx.new_page().await?;
    page.goto("https://example.com", None).await?;
    browser.close().await?;
    Ok(())
}
```

## Main World vs Isolated World Evaluation

Playwright `page.evaluate(...)` runs in Juggler's isolated world so page scripts cannot detect it. To read or mutate globals defined by the page itself, enable `main_world_eval: true` and prefix the expression with `mw:`:

```rust
use camoufox_rs::{camoufox, CamoufoxOptions};
use serde_json::Value;

#[tokio::main]
async fn main() -> camoufox_rs::Result<()> {
    let mut browser = camoufox(CamoufoxOptions {
        main_world_eval: true,
        ..Default::default()
    })
    .await?;
    let page = browser.new_page().await?;
    page.set_content("<script>window.__pageToken = 'abc';</script>", None).await?;

    let iso: Value = page.evaluate::<(), Value>("() => window.__pageToken ?? null", None).await?;
    let main: Value = page.evaluate::<(), Value>("mw:() => window.__pageToken ?? null", None).await?;
    assert_eq!(iso, Value::Null);
    assert_eq!(main, Value::String("abc".into()));

    browser.close().await?;
    Ok(())
}
```

## Headless Live Stream (`CAMF` Video + `CAMI` Input)

When running `un-camoufox` builds from `2noScript/camoufox` (the default `un-camoufox` repo in [`data-files/repos.yml`](data-files/repos.yml)) on Linux or macOS, enable `headless_stream` on `CamoufoxOptions` to receive composited `NV12`/`BGRA` frames over `AF_UNIX` and send native mouse, wheel, and keyboard events back without a virtual display:

```rust
use camoufox_rs::{
    camoufox, CamoufoxOptions, HeadlessMode, HeadlessStreamOptions, Modifiers, MouseButton,
    StreamFormat,
};

#[tokio::main]
async fn main() -> camoufox_rs::Result<()> {
    let mut browser = camoufox(CamoufoxOptions {
        os: vec!["macos".into()],
        headless: HeadlessMode::Bool(true),
        headless_stream: Some(
            HeadlessStreamOptions::new()
                .with_fps(60)
                .with_format(StreamFormat::Nv12),
        ),
        ..Default::default()
    })
    .await?;

    let mut stream = browser.connect_stream().await?;
    let page = browser.new_page().await?;
    page.goto("https://example.com", None).await?;

    let frame = stream.next_frame().await?;
    println!("Got {}x{} frame ({} bytes)", frame.width, frame.height, frame.data.len());
    stream.click(100, 100, MouseButton::Left, Modifiers::NONE).await?;

    browser.close().await?;
    Ok(())
}
```

See [`examples/headless_stream.rs`](examples/headless_stream.rs) for a runnable demo.

## Persistent Context & Remote Server

- **Persistent profile**: set `persistent_context: true` and `user_data_dir: Some(path)` on `CamoufoxOptions`.
- **WebSocket server**: call `launch_server(options).await?` (note: cannot be combined with `persistent_context`, `user_data_dir`, or driver-side `humanize`).

## CLI Commands

```bash
camoufox sync                  # Sync remote release metadata
camoufox fetch                 # Download active browser build & GeoIP database
camoufox list                  # List installed versions & GeoIP files
camoufox set official/stable   # Switch active channel/version
camoufox set --release         # Reset to paired release
camoufox server --port 3000    # Launch Playwright WebSocket server
camoufox test                  # Open Playwright Inspector with Camoufox
camoufox version               # Print crate, browser SHA-256, and cache paths
camoufox active                # Print active version specifier
camoufox path                  # Print install cache directory
camoufox gpu-xorg-config       # Print Xorg config for Linux virtual-gpu
camoufox gpu-setup             # Print root setup script for Linux virtual-gpu
```

## Testing

```bash
cargo test
cargo clippy --all-targets -- -D warnings

# Real browser launch, multi-context, humanize click, persistent context & server E2E:
CAMOUFOX_TEST_BIN=/path/to/camoufox cargo test --test e2e_browser -- --ignored --nocapture
```
