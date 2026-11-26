//! Interactive end-to-end web automation demo powered by `camoufox_rs` + `myuri_rs`.
//!
//! Spins up a tiny local multi-page web app (`GET /`, `GET /dashboard`), launches
//! headed Camoufox with humanized mouse/keyboard/scroll input, logs in, hovers
//! tooltips, filters rows, scrolls down, approves an item, and keeps the window open
//! for manual inspection (pass `--auto-close` to exit automatically).
//!
//! ```bash
//! CAMOUFOX_EXECUTABLE_PATH=/path/to/camoufox cargo run --example web_automation_demo
//! ```

use std::time::Duration;

use camoufox_rs::addons::DefaultAddons;
use camoufox_rs::humanize::HumanizeSetting;
use camoufox_rs::humanize_playwright::HumanizedPage;
use camoufox_rs::myuri_rs::Vector;
use camoufox_rs::{camoufox, CamoufoxError, CamoufoxOptions, HeadlessMode};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const LOGIN_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>Camoufox Rust Web Demo — Sign In</title>
<style>
  * { box-sizing: border-box; }
  body { margin: 0; min-height: 100vh; background: #0b1120; color: #e2e8f0; font-family: system-ui, -apple-system, sans-serif; display: grid; place-items: center; }
  #trail { position: fixed; inset: 0; pointer-events: none; z-index: 9998; }
  .card { width: 420px; background: #111827; border: 1px solid #1f2937; border-radius: 14px; padding: 28px; box-shadow: 0 20px 60px rgba(0,0,0,.45); }
  h1 { margin: 0 0 6px; font-size: 20px; }
  p.sub { margin: 0 0 20px; color: #94a3b8; font-size: 13px; }
  label { display: block; font-size: 13px; margin: 12px 0 6px; color: #cbd5e1; }
  input[type=text], input[type=password], select {
    width: 100%; padding: 10px 12px; border-radius: 8px; border: 1px solid #334155;
    background: #0f172a; color: #f8fafc; font-size: 14px; outline: none;
  }
  input:focus, select:focus { border-color: #38bdf8; }
  .row { display: flex; align-items: center; justify-content: space-between; margin-top: 14px; gap: 10px; }
  .check { display: flex; align-items: center; gap: 8px; font-size: 13px; cursor: pointer; }
  .badge { position: relative; font-size: 12px; color: #38bdf8; border: 1px solid #1e3a8a; background: #0f172a; padding: 4px 10px; border-radius: 999px; cursor: help; }
  #tooltip { display: none; position: absolute; right: 0; bottom: 30px; width: 220px; padding: 8px 10px; border-radius: 8px; background: #1e293b; border: 1px solid #38bdf8; color: #e2e8f0; font-size: 12px; }
  .badge.open #tooltip { display: block; }
  button { margin-top: 20px; width: 100%; padding: 11px; border: 0; border-radius: 8px; background: #2563eb; color: #fff; font-weight: 600; font-size: 15px; cursor: pointer; }
  button:hover { background: #1d4ed8; }
</style>
</head>
<body>
  <canvas id="trail"></canvas>
  <form id="login-form" class="card" method="GET" action="/dashboard">
    <h1>Camoufox Portal</h1>
    <p class="sub">Sign in with Rust SDK humanized mouse &amp; keyboard</p>
    <label for="email">Email</label>
    <input id="email" name="user" type="text" placeholder="you@example.com" required />
    <label for="password">Password</label>
    <input id="password" type="password" placeholder="Enter password" required />
    <label for="plan">Workspace Plan</label>
    <select id="plan" name="plan">
      <option value="starter">Starter</option>
      <option value="pro">Pro Anti-Detect</option>
      <option value="enterprise">Enterprise Cluster</option>
    </select>
    <div class="row">
      <label class="check"><input id="agree" type="checkbox" required /> Accept terms</label>
      <div id="help-badge" class="badge">Hover info
        <div id="tooltip">Camoufox C++ spoof + Myuri driver cursor active.</div>
      </div>
    </div>
    <button id="login-btn" type="submit">Sign In to Dashboard</button>
  </form>
  <script>
    const canvas = document.getElementById('trail');
    const ctx = canvas.getContext('2d');
    const resize = () => { canvas.width = window.innerWidth; canvas.height = window.innerHeight; };
    resize();
    window.addEventListener('resize', resize);
    let last = null;
    window.addEventListener('mousemove', (e) => {
      if (last) {
        ctx.strokeStyle = 'rgba(56, 189, 248, 0.75)';
        ctx.lineWidth = 2;
        ctx.beginPath();
        ctx.moveTo(last.x, last.y);
        ctx.lineTo(e.clientX, e.clientY);
        ctx.stroke();
      }
      last = { x: e.clientX, y: e.clientY };
    }, true);
    document.getElementById('help-badge').addEventListener('mouseenter', (e) => e.currentTarget.classList.add('open'));
    document.getElementById('help-badge').addEventListener('mouseleave', (e) => e.currentTarget.classList.remove('open'));
    document.getElementById('login-form').addEventListener('submit', () => {
      document.cookie = 'camou_session=verified_rust_sdk; path=/';
    });
  </script>
</body>
</html>"##;

fn dashboard_html(query: &str) -> String {
    let mut rows = String::new();
    for i in 1..=25 {
        let label = format!("Order #{i:03}");
        let owner = if i % 2 == 0 { "Alice" } else { "Bob" };
        rows.push_str(&format!(
            r#"<tr data-row="{i}" data-label="{label} {owner}">
              <td>#{i:03}</td>
              <td>{label}</td>
              <td>{owner}</td>
              <td><span id="status-{i}" class="pill pending">Pending</span></td>
              <td><button id="approve-{i}" class="approve" onclick="approveRow({i})">Approve</button></td>
            </tr>"#
        ));
    }
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>Camoufox Dashboard</title>
<style>
  * {{ box-sizing: border-box; }}
  body {{ margin: 0; background: #0b1120; color: #e2e8f0; font-family: system-ui, -apple-system, sans-serif; padding: 28px; }}
  #trail {{ position: fixed; inset: 0; pointer-events: none; z-index: 9998; }}
  .wrap {{ max-width: 960px; margin: 0 auto; }}
  .grid {{ display: grid; grid-template-columns: repeat(3, 1fr); gap: 12px; margin: 16px 0 24px; }}
  .stat {{ background: #111827; border: 1px solid #1f2937; border-radius: 10px; padding: 14px; font-size: 13px; }}
  .stat b {{ display: block; font-size: 15px; color: #38bdf8; margin-top: 4px; word-break: break-all; }}
  .toolbar {{ display: flex; gap: 12px; margin-bottom: 14px; }}
  input#search {{ flex: 1; padding: 10px 12px; border-radius: 8px; border: 1px solid #334155; background: #111827; color: #fff; font-size: 14px; }}
  button.clear {{ padding: 10px 16px; border-radius: 8px; border: 1px solid #334155; background: #1e293b; color: #fff; cursor: pointer; }}
  table {{ width: 100%; border-collapse: collapse; background: #111827; border: 1px solid #1f2937; border-radius: 10px; overflow: hidden; }}
  th, td {{ padding: 14px 16px; border-bottom: 1px solid #1f2937; text-align: left; font-size: 14px; }}
  th {{ background: #1e293b; color: #94a3b8; font-size: 12px; text-transform: uppercase; }}
  .pill {{ padding: 4px 10px; border-radius: 999px; font-size: 12px; }}
  .pending {{ background: #451a03; color: #fbbf24; }}
  .done {{ background: #052e16; color: #4ade80; }}
  button.approve {{ padding: 6px 12px; border-radius: 6px; border: 0; background: #2563eb; color: #fff; cursor: pointer; }}
  button.approve.done-btn {{ background: #16a34a; }}
</style>
</head>
<body>
  <canvas id="trail"></canvas>
  <div class="wrap">
    <h1>Dashboard <small style="color:#64748b;font-size:13px">({query})</small></h1>
    <div class="grid">
      <div class="stat">Session Cookie<b id="fp-cookie"></b></div>
      <div class="stat">Platform / WebDriver<b id="fp-plat"></b></div>
      <div class="stat">WebGL Renderer<b id="fp-webgl"></b></div>
    </div>
    <div class="toolbar">
      <input id="search" type="text" placeholder="Filter orders (e.g. Order #018)..." />
      <button id="reset-search" class="clear" onclick="clearSearch()">Show All</button>
    </div>
    <table>
      <thead><tr><th>ID</th><th>Order</th><th>Owner</th><th>Status</th><th>Action</th></tr></thead>
      <tbody id="rows">{rows}</tbody>
    </table>
  </div>
  <script>
    const canvas = document.getElementById('trail');
    const ctx = canvas.getContext('2d');
    const resize = () => {{ canvas.width = window.innerWidth; canvas.height = window.innerHeight; }};
    resize();
    window.addEventListener('resize', resize);
    let last = null;
    window.addEventListener('mousemove', (e) => {{
      if (last) {{
        ctx.strokeStyle = 'rgba(56, 189, 248, 0.75)';
        ctx.lineWidth = 2;
        ctx.beginPath();
        ctx.moveTo(last.x, last.y);
        ctx.lineTo(e.clientX, e.clientY);
        ctx.stroke();
      }}
      last = {{ x: e.clientX, y: e.clientY }};
    }}, true);

    const gl = document.createElement('canvas').getContext('webgl');
    const dbg = gl && gl.getExtension('WEBGL_debug_renderer_info');
    document.getElementById('fp-cookie').textContent = document.cookie || 'none';
    document.getElementById('fp-plat').textContent = navigator.platform + ' / webdriver=' + navigator.webdriver;
    document.getElementById('fp-webgl').textContent = dbg ? gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) : 'disabled';

    function filterRows(q) {{
      const needle = q.trim().toLowerCase();
      document.querySelectorAll('#rows tr').forEach((tr) => {{
        tr.style.display = !needle || tr.dataset.label.toLowerCase().includes(needle) ? '' : 'none';
      }});
    }}
    document.getElementById('search').addEventListener('input', (e) => filterRows(e.target.value));
    window.clearSearch = () => {{
      document.getElementById('search').value = '';
      filterRows('');
    }};
    window.approveRow = (id) => {{
      const s = document.getElementById('status-' + id);
      const b = document.getElementById('approve-' + id);
      s.textContent = 'Approved';
      s.className = 'pill done';
      b.textContent = 'Approved';
      b.classList.add('done-btn');
    }};
  </script>
</body>
</html>"##
    )
}

async fn start_demo_server() -> std::io::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let first_line = req.lines().next().unwrap_or("");
                let target = first_line.split_whitespace().nth(1).unwrap_or("/");
                let body = if target.starts_with("/dashboard") {
                    dashboard_html(target)
                } else {
                    LOGIN_HTML.to_owned()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            });
        }
    });
    Ok(format!("http://127.0.0.1:{}", addr.port()))
}

#[tokio::main]
async fn main() -> camoufox_rs::Result<()> {
    let base_url = start_demo_server().await?;
    println!("Demo web app listening at {base_url}");

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
    let mut cursor = browser.cursor(&page).await?;
    let mut hp = HumanizedPage::attach(page.clone(), Some(&HumanizeSetting::Bool(true)), "mac")?;

    println!("1) Navigating to {base_url}/ ...");
    page.goto(&format!("{base_url}/"), None).await?;
    cursor
        .set_cursor_location(Vector::new(40.0, 40.0))
        .await
        .map_err(CamoufoxError::from)?;

    println!("2) Filling login form (#email, #password, #plan)...");
    cursor
        .click(Some("#email"), None)
        .await
        .map_err(CamoufoxError::from)?;
    hp.type_text("rust.user@camoufox.dev").await?;

    cursor
        .click(Some("#password"), None)
        .await
        .map_err(CamoufoxError::from)?;
    hp.type_text("Camoufox#2026").await?;

    cursor
        .move_to_selector("#plan", None)
        .await
        .map_err(CamoufoxError::from)?;
    page.locator("#plan").select_option("pro", None).await?;

    println!("3) Hovering #help-badge tooltip, checking #agree, and submitting...");
    cursor
        .move_to_selector("#help-badge", None)
        .await
        .map_err(CamoufoxError::from)?;
    tokio::time::sleep(Duration::from_millis(450)).await;
    cursor
        .click(Some("#agree"), None)
        .await
        .map_err(CamoufoxError::from)?;
    cursor
        .click(Some("#login-btn"), None)
        .await
        .map_err(CamoufoxError::from)?;

    page.wait_for_url("**/dashboard*", None).await?;
    page.locator("#rows tr").first().wait_for(None).await?;
    println!("4) Logged in! On /dashboard, filtering for Order #018...");

    cursor
        .click(Some("#search"), None)
        .await
        .map_err(CamoufoxError::from)?;
    hp.type_text("Order #018").await?;
    tokio::time::sleep(Duration::from_millis(600)).await;

    cursor
        .click(Some("#approve-18"), None)
        .await
        .map_err(CamoufoxError::from)?;
    cursor
        .click(Some("#reset-search"), None)
        .await
        .map_err(CamoufoxError::from)?;

    println!("5) Scrolling smoothly to #approve-24 and clicking it...");
    cursor
        .click(Some("#approve-24"), None)
        .await
        .map_err(CamoufoxError::from)?;

    let summary: Value = page
        .evaluate::<(), Value>(
            r#"() => ({
                url: location.href,
                cookie: document.cookie,
                row18: document.getElementById('status-18').textContent,
                row24: document.getElementById('status-24').textContent,
                scrollY: Math.round(window.scrollY)
            })"#,
            None,
        )
        .await?;
    println!("Web automation finished: {summary}");

    let auto_close = std::env::args().any(|a| a == "--auto-close");
    if auto_close {
        tokio::time::sleep(Duration::from_millis(1000)).await;
    } else {
        println!("Dashboard stays open for you to inspect/interact. Close the browser window or press Ctrl+C to exit.");
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
