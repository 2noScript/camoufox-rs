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

Install the `camoufox` CLI from source:

```bash
cargo install --path .
camoufox fetch
```

## Quick Start

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
    browser.close().await?;
    Ok(())
}
```

### Multi-Context Identity Isolation

```rust
use camoufox_rs::{camoufox, CamoufoxOptions, NewContextOptions};

# async fn run() -> camoufox_rs::Result<()> {
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
page.goto("https://browserleaks.com", None).await?;
browser.close().await?;
# Ok(())
# }
```

### CLI Commands

```bash
camoufox sync
camoufox fetch
camoufox list
camoufox set official/stable
camoufox set --release
camoufox server --port 3000
camoufox version
camoufox active
camoufox path
```
