# Camoufox Rust SDK

Rust launcher SDK for the Camoufox anti-detect Firefox browser, structured alongside `python/` and `typescript/`.

## Usage

Add the crate from the local workspace path in `Cargo.toml`:

```toml
[dependencies]
camoufox-rs = { git = "https://github.com/2noScript/camoufox-rs" }
```

Build `CAMOU_CONFIG_*` and `CAMOU_PREFS_*` environment variables for launching a Camoufox binary:

```rust
use camoufox_rs::{launch_env, LaunchOptions, TargetOs};

let mut opts = LaunchOptions {
    os: Some(TargetOs::MacOs),
    headless: true,
    ..Default::default()
};
opts.config.insert("navigator.platform".into(), "MacIntel".into());

let env = launch_env(&opts).expect("valid launch options");
assert!(env.contains_key("CAMOU_CONFIG_1"));
```

## Development

```bash
cd camoufox-rs
cargo test
cargo clippy --all-targets
```
