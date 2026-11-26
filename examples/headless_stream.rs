//! Launch headless Camoufox with live NV12 frame capture (`CAMF`) and native input (`CAMI`).
//!
//! Run against an installed build (`camoufox fetch`) or a local binary:
//! ```bash
//! CAMOUFOX_EXECUTABLE_PATH=/path/to/camoufox cargo run --example headless_stream
//! ```

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
    page.set_content(
        r#"<!doctype html><body style="margin:0;background:#0f172a;color:#fff">
        <input id="box" style="margin:40px;width:260px;height:44px;font-size:20px" />
        </body>"#,
        None,
    )
    .await?;

    let frame = stream.next_frame().await?;
    println!(
        "First frame: {}x{} {:?} ({} bytes, pts={}us)",
        frame.width,
        frame.height,
        frame.format,
        frame.data.len(),
        frame.pts_us
    );
    let rgba = frame.to_rgba()?;
    println!("Converted RGBA buffer: {} bytes", rgba.len());

    stream.click(120, 140, MouseButton::Left, Modifiers::NONE).await?;
    stream.type_text("hello from camoufox-rs", Modifiers::NONE).await?;

    let next = stream.next_frame().await?;
    println!("Next frame pts={}us", next.pts_us);

    browser.close().await?;
    Ok(())
}
