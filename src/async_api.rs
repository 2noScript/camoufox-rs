//! Async and primary Rust launcher entry points (`Camoufox`, `NewBrowser`, `NewContext`),
//! matching `async_api.ts` / `async_api.py` and `sync_api.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use playwright_rs::protocol::{
    Browser, BrowserContext, BrowserContextOptions, Geolocation as PwGeolocation, Page, Viewport,
};
use playwright_rs::Playwright;
use serde_json::{Map, Value};
use tokio::sync::Mutex;

use crate::coherence::Config;
use crate::cpu_affinity;
use crate::exceptions::{CamoufoxError, Result};
use crate::fingerprints::{generate_context_fingerprint, ContextFingerprint};
use crate::fpgen_model::ensure_fpgen_model;
use crate::host_rendering::Gpu;
use crate::humanize::{normalize as normalize_humanize, HumanizeSetting};
use crate::ip::{proxy_auth_init_script, proxy_exit_geo, socks5_auth, ProxyConfig, ProxyHelper};
use crate::locales::StatisticalLocaleSelector;
use crate::utils::{
    camou_config_blob, context_identity, host_identity, launch_options, launch_os,
    pinned_core_count, spoofs_window_dimensions, start_virtual_display, ActiveDisplay,
    CamoufoxOptions, PreparedLaunchOptions,
};
use crate::warnings::LeakWarning;

pub type Headless = crate::utils::HeadlessMode;

#[derive(Debug, Clone, Default)]
pub struct NewContextOptions {
    pub preset: Option<Value>,
    pub os: Option<String>,
    pub ff_version: Option<String>,
    pub webrtc_ip: Option<String>,
    pub timezone_id: Option<String>,
    pub locale: Option<String>,
    pub proxy: Option<ProxyConfig>,
    pub geolocation: Option<(f64, f64)>,
    pub user_agent: Option<String>,
    pub viewport: Option<(u32, u32)>,
    pub no_viewport: Option<bool>,
    pub device_scale_factor: Option<f64>,
    pub is_mobile: Option<bool>,
    pub config_overrides: Option<BTreeMap<String, Value>>,
    pub extra_options: Option<BrowserContextOptions>,
}

/// Managed launched Camoufox instance holding the Playwright runtime,
/// browser (or persistent context), virtual display handle, host identity,
/// and humanization settings.
pub struct CamoufoxBrowser {
    playwright: Option<Playwright>,
    pub browser: Option<Browser>,
    pub persistent_context: Option<BrowserContext>,
    display: Arc<Mutex<Option<ActiveDisplay>>>,
    host_identity: Option<(String, Option<Gpu>)>,
    no_viewport_default: bool,
    pub target_os: String,
    pub humanize: Option<HumanizeSetting>,
    pub i_know_what_im_doing: bool,
    pub prepared_options: PreparedLaunchOptions,
}

impl CamoufoxBrowser {
    /// Launches a managed Camoufox browser (or persistent context).
    pub async fn launch(options: CamoufoxOptions) -> Result<Self> {
        new_browser(options).await
    }

    pub fn browser(&self) -> Result<&Browser> {
        self.browser.as_ref().ok_or_else(|| {
            CamoufoxError::ValueError(
                "Launched in persistent_context mode; use `.persistent_context` instead".into(),
            )
        })
    }

    pub fn is_persistent(&self) -> bool {
        self.persistent_context.is_some()
    }

    /// Creates a new isolated `BrowserContext` with its own drawn or preset identity
    /// and SOCKS5 / init script injection, matching `NewContext`.
    pub async fn new_context(&self, opts: NewContextOptions) -> Result<BrowserContext> {
        let browser = self.browser()?;
        new_context_with_identity(
            browser,
            self.host_identity.as_ref(),
            self.no_viewport_default,
            opts,
        )
        .await
    }

    /// Creates a new page using the browser's launch identity (`CAMOU_CONFIG`),
    /// applying `no_viewport` when window dimensions are spoofed.
    /// For a fresh per-context identity, use [`Self::new_context`].
    pub async fn new_page(&self) -> Result<Page> {
        if let Some(ref ctx) = self.persistent_context {
            return Ok(ctx.new_page().await?);
        }
        let browser = self.browser()?;
        let mut ctx_opts = BrowserContextOptions::default();
        if self.no_viewport_default {
            ctx_opts.no_viewport = Some(true);
        }
        let ctx = browser.new_context_with_options(ctx_opts).await?;
        Ok(ctx.new_page().await?)
    }

    /// Creates a humanized cursor driven by `myuri-rs` on `page`.
    pub async fn cursor(&self, page: &Page) -> Result<myuri_rs::Cursor> {
        crate::humanize_playwright::cursor_for_page(
            page,
            self.humanize.as_ref(),
            &self.target_os,
        )
        .await
    }

    /// Closes the browser / context and tears down any virtual display.
    pub async fn close(&mut self) -> Result<()> {
        if let Some(ctx) = self.persistent_context.take() {
            let _ = ctx.close().await;
        }
        if let Some(browser) = self.browser.take() {
            let _ = browser.close().await;
        }
        if let Some(mut disp) = self.display.lock().await.take() {
            disp.kill();
        }
        if let Some(pw) = self.playwright.take() {
            let _ = pw.shutdown().await;
        }
        Ok(())
    }
}

impl Drop for CamoufoxBrowser {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.display.try_lock() {
            if let Some(mut disp) = guard.take() {
                disp.kill();
            }
        }
    }
}

/// Primary async entry point (Rust twin of `Camoufox` / `NewBrowser`).
pub async fn camoufox(options: CamoufoxOptions) -> Result<CamoufoxBrowser> {
    new_browser(options).await
}

/// Launches a Camoufox browser instance given `CamoufoxOptions`.
pub async fn new_browser(mut options: CamoufoxOptions) -> Result<CamoufoxBrowser> {
    if let Some(ref h) = options.humanize {
        let _ = normalize_humanize(h)?;
    }
    let humanize_opt = options.humanize.clone();
    let i_know = options.i_know_what_im_doing;

    let (virtual_display, headless_bool) = start_virtual_display(&mut options).await?;
    options.headless = Headless::Bool(headless_bool);

    let canvas_noise = options.canvas_noise;
    let is_persistent = options.persistent_context || options.user_data_dir.is_some();
    let user_data_dir = options.user_data_dir.clone();
    let no_viewport_opt = options.no_viewport;

    let prepared = match launch_options(options).await {
        Ok(p) => p,
        Err(err) => {
            if let Some(mut d) = virtual_display {
                d.kill();
            }
            return Err(err);
        }
    };

    let no_viewport_default = spoofs_window_dimensions(&prepared.env);
    let pin_to = pinned_core_count(&prepared.env);
    let target_os = launch_os(&prepared.env).to_owned();

    let display_arc = Arc::new(Mutex::new(virtual_display));

    let do_launch = || async {
        let host_ident = if is_persistent {
            None
        } else {
            host_identity(&prepared, canvas_noise).await?
        };
        if is_persistent {
            static PERSISTENT_ENV_LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
            let env_guard = PERSISTENT_ENV_LOCK.get_or_init(|| Mutex::new(())).lock().await;

            struct EnvRestore(Vec<(String, Option<std::ffi::OsString>)>);
            impl Drop for EnvRestore {
                fn drop(&mut self) {
                    for (k, prev) in &self.0 {
                        match prev {
                            Some(v) => std::env::set_var(k, v),
                            None => std::env::remove_var(k),
                        }
                    }
                }
            }

            let mut saved = Vec::with_capacity(prepared.env.len());
            for (k, v) in &prepared.env {
                saved.push((k.clone(), std::env::var_os(k)));
                std::env::set_var(k, v);
            }
            let _restore = EnvRestore(saved);

            let pw = Playwright::launch().await?;
            let firefox = pw.firefox();
            let dir_str = user_data_dir
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let mut ctx_opts = BrowserContextOptions::builder()
                .executable_path(prepared.executable_path.display().to_string())
                .headless(prepared.headless)
                .build();
            ctx_opts.args = if prepared.args.is_empty() {
                None
            } else {
                Some(prepared.args.clone())
            };
            let prefs_map: std::collections::HashMap<String, Value> = prepared
                .firefox_user_prefs
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            ctx_opts.firefox_user_prefs = Some(prefs_map);
            if no_viewport_opt == Some(true) || (no_viewport_default && no_viewport_opt.is_none()) {
                ctx_opts.no_viewport = Some(true);
            }
            if let Some(ref p) = prepared.proxy {
                ctx_opts.proxy = Some(p.to_playwright());
            }
            let ctx = firefox
                .launch_persistent_context_with_options(dir_str, ctx_opts)
                .await?;
            drop(_restore);
            drop(env_guard);
            return Ok::<_, CamoufoxError>(CamoufoxBrowser {
                playwright: Some(pw),
                browser: None,
                persistent_context: Some(ctx),
                display: display_arc.clone(),
                host_identity: None,
                no_viewport_default,
                target_os: target_os.clone(),
                humanize: humanize_opt.clone(),
                i_know_what_im_doing: i_know,
                prepared_options: prepared.clone(),
            });
        }

        let pw = Playwright::launch().await?;
        let firefox = pw.firefox();

        let pw_opts = prepared.to_playwright_launch_options();
        let browser = firefox.launch_with_options(pw_opts).await?;
        Ok(CamoufoxBrowser {
            playwright: Some(pw),
            browser: Some(browser),
            persistent_context: None,
            display: display_arc.clone(),
            host_identity: host_ident,
            no_viewport_default,
            target_os: target_os.clone(),
            humanize: humanize_opt.clone(),
            i_know_what_im_doing: i_know,
            prepared_options: prepared.clone(),
        })
    };

    let launched = if let Some(count) = pin_to {
        let pid = std::process::id();
        cpu_affinity::with_pin_lock(|| async {
            let previous = cpu_affinity::pin(pid, count);
            let res = do_launch().await;
            cpu_affinity::restore(pid, previous.as_deref());
            res
        })
        .await
    } else {
        cpu_affinity::with_unpinned_launch(do_launch).await
    };

    match launched {
        Ok(b) => {
            if let Some(ActiveDisplay::Gpu(ref gpu_disp)) = b.display.lock().await.as_ref() {
                if let Some(ref browser) = b.browser {
                    crate::gpudisplay::prove_gpu(browser, gpu_disp).await?;
                }
            }
            Ok(b)
        }
        Err(err) => {
            if let Some(mut d) = display_arc.lock().await.take() {
                d.kill();
            }
            Err(err)
        }
    }
}

/// Public standalone `NewContext(browser, opts)` matching `async_api.NewContext`.
pub async fn new_context(browser: &Browser, opts: NewContextOptions) -> Result<BrowserContext> {
    new_context_with_identity(browser, None, true, opts).await
}

async fn new_context_with_identity(
    browser: &Browser,
    host_ident: Option<&(String, Option<Gpu>)>,
    no_viewport_default: bool,
    opts: NewContextOptions,
) -> Result<BrowserContext> {
    let _ = ensure_fpgen_model(false).await?;
    if opts.is_mobile == Some(true) {
        LeakWarning::warn("is_mobile", None);
    }

    let ff_version = opts.ff_version.clone().unwrap_or_else(|| {
        browser
            .version()
            .split('.')
            .next()
            .unwrap_or("159")
            .to_owned()
    });

    let mut webrtc_ip = opts.webrtc_ip.clone();
    let mut position = opts.geolocation;
    let mut timezone_id = opts.timezone_id.clone();
    let mut locale = opts.locale.clone();

    if let Some(ref proxy) = opts.proxy {
        if webrtc_ip.is_none()
            || position.is_none()
            || timezone_id.is_none()
            || locale.is_none()
        {
            let proxy_str = ProxyHelper::as_string(proxy)?;
            let exit = proxy_exit_geo(&proxy_str).await?;
            if webrtc_ip.is_none() {
                webrtc_ip = Some(exit.ip);
            }
            if position.is_none() {
                position = Some((exit.latitude, exit.longitude));
            }
            if timezone_id.is_none() {
                timezone_id = Some(exit.timezone);
            }
            if locale.is_none() {
                if let Ok(loc) = StatisticalLocaleSelector::from_region(&exit.country) {
                    locale = Some(loc.as_string());
                }
            }
        }
    }

    let (ctx_os, webgl_cfg) = context_identity(
        host_ident,
        opts.os.as_deref(),
        opts.preset.as_ref(),
    )?;

    let fp: ContextFingerprint = generate_context_fingerprint(
        opts.preset.as_ref(),
        ctx_os.as_deref(),
        Some(&ff_version),
        webrtc_ip.as_deref(),
        timezone_id.as_deref(),
        locale.as_deref(),
        opts.config_overrides.as_ref(),
        webgl_cfg
            .as_ref()
            .map(|(v, r)| (v.as_str(), r.as_str())),
    )?;

    let mut pw_ctx = opts.extra_options.unwrap_or_default();
    if let Some(ua) = opts
        .user_agent
        .or_else(|| fp.context_options.get("userAgent").and_then(Value::as_str).map(str::to_owned))
    {
        pw_ctx.user_agent = Some(ua);
    }
    if let Some((w, h)) = opts.viewport {
        pw_ctx.viewport = Some(Viewport {
            width: w,
            height: h,
        });
    } else if opts.no_viewport == Some(true)
        || (no_viewport_default && opts.no_viewport.is_none() && pw_ctx.viewport.is_none())
    {
        pw_ctx.no_viewport = Some(true);
        pw_ctx.viewport = None;
    } else if pw_ctx.viewport.is_none() {
        if let Some(vp) = fp.context_options.get("viewport").and_then(Value::as_object) {
            if let (Some(w), Some(h)) = (
                vp.get("width").and_then(Value::as_u64),
                vp.get("height").and_then(Value::as_u64),
            ) {
                pw_ctx.viewport = Some(Viewport {
                    width: w as u32,
                    height: h as u32,
                });
            }
        }
    }
    if let Some(dpr) = opts.device_scale_factor.or_else(|| {
        fp.context_options
            .get("deviceScaleFactor")
            .and_then(Value::as_f64)
    }) {
        pw_ctx.device_scale_factor = Some(dpr);
    }
    if let Some(tz) = timezone_id.or_else(|| {
        fp.context_options
            .get("timezoneId")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }) {
        pw_ctx.timezone_id = Some(tz);
    }
    if let Some(loc) = locale.or_else(|| {
        fp.context_options
            .get("locale")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }) {
        pw_ctx.locale = Some(loc);
    }

    let (bare_proxy, proxy_auth) = socks5_auth(opts.proxy.as_ref())?;
    if let Some(p) = bare_proxy {
        pw_ctx.proxy = Some(p.to_playwright());
    }
    if let Some((lat, lon)) = position {
        pw_ctx.geolocation = Some(PwGeolocation {
            latitude: lat,
            longitude: lon,
            accuracy: None,
        });
        if pw_ctx.permissions.is_none() {
            pw_ctx.permissions = Some(vec!["geolocation".to_owned()]);
        }
    }

    let context = browser.new_context_with_options(pw_ctx).await?;
    context.add_init_script(&fp.init_script).await?;
    if let Some(auth) = proxy_auth {
        context.add_init_script(&proxy_auth_init_script(&auth)).await?;
    }
    Ok(context)
}

/// Inspects `CAMOU_CONFIG_*` inside a `PreparedLaunchOptions`.
pub fn config_from_prepared(opts: &PreparedLaunchOptions) -> Config {
    let blob = camou_config_blob(&opts.env);
    serde_json::from_str::<Map<String, Value>>(&blob).unwrap_or_default()
}
