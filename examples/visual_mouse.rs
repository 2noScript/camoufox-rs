//! Visual humanized mouse, keyboard, drag, and wheel scroll demo powered strictly by `camoufox_rs`.
//!
//! Run against a local build or installed Camoufox binary:
//! ```bash
//! CAMOUFOX_EXECUTABLE_PATH=/path/to/camoufox cargo run --example visual_mouse
//! ```

use std::time::Duration;

use camoufox_rs::addons::DefaultAddons;
use camoufox_rs::humanize::HumanizeSetting;
use camoufox_rs::humanize_playwright::HumanizedPage;
use camoufox_rs::myuri_rs::Vector;
use camoufox_rs::{camoufox, CamoufoxOptions, HeadlessMode};
use serde_json::Value;

#[tokio::main]
async fn main() -> camoufox_rs::Result<()> {
    let mut browser = camoufox(CamoufoxOptions {
        os: vec!["macos".into()],
        headless: HeadlessMode::Bool(false),
        main_world_eval: true,
        humanize: Some(HumanizeSetting::Bool(true)),
        exclude_addons: vec![DefaultAddons::Ubo],
        i_know_what_im_doing: true,
        ..Default::default()
    })
    .await?;

    let page = browser.new_page().await?;
    page.set_content(
        r#"
        <!doctype html>
        <html>
        <head>
        <style>
          body { margin: 0; height: 2000px; background: #0f172a; color: #e2e8f0; font-family: system-ui, sans-serif; }
          #hud { position: fixed; top: 12px; left: 12px; z-index: 9999; background: rgba(15,23,42,.88); border: 1px solid #334155; border-radius: 8px; padding: 10px 14px; font-size: 13px; }
          #trail { position: fixed; inset: 0; pointer-events: none; z-index: 9998; }
          .target { position: absolute; border-radius: 8px; padding: 10px 16px; border: 1px solid #475569; background: #1e293b; color: #fff; font-size: 15px; cursor: pointer; }
          .hit { background: #16a34a !important; border-color: #4ade80 !important; }
          #near { left: 740px; top: 180px; width: 140px; height: 48px; }
          #name { position: absolute; left: 110px; top: 150px; width: 280px; height: 38px; padding: 6px 12px; font-size: 16px; border-radius: 6px; border: 1px solid #475569; background: #1e293b; color: #fff; }
          #btn2 { left: 680px; top: 470px; width: 170px; height: 50px; }
          #drag { left: 140px; top: 380px; width: 100px; height: 46px; background: #2563eb; display: flex; align-items: center; justify-content: center; }
          #drop { left: 520px; top: 360px; width: 160px; height: 80px; background: transparent; border: 2px dashed #22c55e; display: flex; align-items: center; justify-content: center; }
          #far { left: 380px; top: 1450px; width: 220px; height: 54px; }
        </style>
        </head>
        <body>
          <div id="hud">Moves: <b id="mv">0</b> | Clicks: <b id="ck">0</b> | ScrollY: <b id="sy">0</b></div>
          <canvas id="trail"></canvas>
          <input id="name" placeholder="Input target (#name)" />
          <button id="near" class="target">#near button</button>
          <div id="drag" class="target">#drag</div>
          <div id="drop" class="target">#drop zone</div>
          <button id="btn2" class="target">#btn2 target</button>
          <button id="far" class="target">#far scrolled button</button>
          <script>
            const canvas = document.getElementById('trail');
            const ctx = canvas.getContext('2d');
            const resize = () => { canvas.width = window.innerWidth; canvas.height = window.innerHeight; };
            resize();
            window.addEventListener('resize', resize);

            let last = null, moves = 0, clicks = 0;
            window.addEventListener('mousemove', (e) => {
              moves++;
              document.getElementById('mv').textContent = String(moves);
              const x = e.clientX, y = e.clientY;
              if (last) {
                ctx.strokeStyle = 'rgba(56, 189, 248, 0.85)';
                ctx.lineWidth = 2.2;
                ctx.beginPath();
                ctx.moveTo(last.x, last.y);
                ctx.lineTo(x, y);
                ctx.stroke();
              }
              ctx.fillStyle = 'rgba(250, 204, 21, 0.85)';
              ctx.beginPath();
              ctx.arc(x, y, 2.5, 0, Math.PI * 2);
              ctx.fill();
              last = { x, y };
            }, true);

            window.addEventListener('click', (e) => {
              clicks++;
              document.getElementById('ck').textContent = String(clicks);
              ctx.strokeStyle = '#4ade80';
              ctx.lineWidth = 3;
              ctx.beginPath();
              ctx.arc(e.clientX, e.clientY, 14, 0, Math.PI * 2);
              ctx.stroke();
              if (e.target && e.target.classList) e.target.classList.add('hit');
            }, true);

            window.addEventListener('scroll', () => {
              document.getElementById('sy').textContent = String(Math.round(window.scrollY));
            }, true);
          </script>
        </body>
        </html>
        "#,
        None,
    )
    .await?;

    let mut cursor = browser.cursor(&page).await?;
    let mut hp = HumanizedPage::attach(page.clone(), Some(&HumanizeSetting::Bool(true)), "mac")?;

    cursor
        .set_cursor_location(Vector::new(50.0, 50.0))
        .await
        .map_err(camoufox_rs::CamoufoxError::from)?;
    tokio::time::sleep(Duration::from_millis(400)).await;

    cursor
        .click(Some("#near"), None)
        .await
        .map_err(camoufox_rs::CamoufoxError::from)?;
    cursor
        .click(Some("#name"), None)
        .await
        .map_err(camoufox_rs::CamoufoxError::from)?;
    hp.type_text("Camoufox Rust SDK").await?;
    cursor
        .drag("#drag", "#drop", None)
        .await
        .map_err(camoufox_rs::CamoufoxError::from)?;
    cursor
        .click(Some("#btn2"), None)
        .await
        .map_err(camoufox_rs::CamoufoxError::from)?;
    cursor
        .click(Some("#far"), None)
        .await
        .map_err(camoufox_rs::CamoufoxError::from)?;

    let hud: Value = page
        .evaluate::<(), Value>(
            "() => ({ moves: document.getElementById('mv').textContent, clicks: document.getElementById('ck').textContent, scrollY: document.getElementById('sy').textContent, input: document.getElementById('name').value })",
            None,
        )
        .await?;
    println!("camoufox-rs visual mouse demo finished: {hud}");

    let auto_close = std::env::args().any(|a| a == "--auto-close");
    if auto_close {
        tokio::time::sleep(Duration::from_millis(1000)).await;
    } else {
        println!("Browser stays open for manual interaction. Close the Camoufox window or press Ctrl+C to exit.");
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => break,
                _ = tokio::time::sleep(Duration::from_millis(250)) => {
                    let connected = browser.browser().map(|b| b.is_connected()).unwrap_or(false);
                    if !connected {
                        break;
                    }
                }
            }
        }
    }
    browser.close().await?;
    Ok(())
}
