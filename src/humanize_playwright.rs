//! Playwright `myuri-rs` integration on pages, matching `_humanize_playwright.ts`.

use std::sync::Arc;

use myuri_rs::{
    create_playwright_cursor, generate_profile, make_rng, playwright_driver, Actions,
    ActionsOptions, Cursor, CursorOptions, Driver, Host, Os, SettingsOverride, Target,
};
use playwright_rs::protocol::{Locator, Page};

use crate::exceptions::{CamoufoxError, Result};
use crate::humanize::{normalize, EngineKind, HumanizeSetting};

pub const VIEWPORT_JS: &str = "() => [innerWidth, innerHeight]";
pub const FRAME_ORIGIN_JS: &str =
    "e => { if (window === window.top) return null; const r = e.getBoundingClientRect(); return [r.x, r.y]; }";

pub fn map_os(os_name: &str) -> Os {
    match os_name {
        "windows" | "win" => Os::Windows,
        "macos" | "mac" => Os::Macos,
        _ => Os::Linux,
    }
}

/// Builds a `myuri_rs::Cursor` configured with the identity's OS and humanize settings.
pub async fn cursor_for_page(
    page: &Page,
    humanize: Option<&HumanizeSetting>,
    os_name: &str,
) -> Result<Cursor> {
    let os = map_os(os_name);
    let mut opts = CursorOptions {
        os: Some(os),
        ..Default::default()
    };

    if let Some(setting) = humanize {
        let norm = normalize(setting)?;
        opts.seed = norm.seed;
        if !norm.settings.is_empty() {
            let val = serde_json::Value::Object(norm.settings);
            let override_cfg: SettingsOverride = serde_json::from_value(val)
                .map_err(|e| CamoufoxError::ValueError(format!("humanize.settings: {e}")))?;
            opts.settings = Some(override_cfg);
        }
    }

    Ok(create_playwright_cursor(page.clone(), opts)?)
}

/// Playwright-backed `myuri_rs::Host` adapter for running raw or humanized actions.
pub struct CamoufoxPlaywrightHost {
    driver: myuri_rs::PlaywrightDriver,
    started_at: std::time::Instant,
}

impl CamoufoxPlaywrightHost {
    pub fn new(page: Page) -> Self {
        Self {
            driver: playwright_driver(page),
            started_at: std::time::Instant::now(),
        }
    }
}

#[async_trait::async_trait]
impl Host for CamoufoxPlaywrightHost {
    async fn move_mouse(&self, x: f64, y: f64) -> myuri_rs::Result<()> {
        self.driver.move_mouse(x, y).await
    }

    async fn click(
        &self,
        button: myuri_rs::MouseButton,
        click_count: usize,
        hold_ms: f64,
    ) -> myuri_rs::Result<()> {
        self.driver.click(button, click_count, hold_ms).await
    }

    async fn down(&self, button: myuri_rs::MouseButton) -> myuri_rs::Result<()> {
        self.driver.mouse_down(button).await
    }

    async fn up(&self, button: myuri_rs::MouseButton) -> myuri_rs::Result<()> {
        self.driver.mouse_up(button).await
    }

    async fn wheel(&self, dx: f64, dy: f64) -> myuri_rs::Result<()> {
        self.driver.wheel(dx, dy).await
    }

    async fn key_down(&self, key: &str) -> myuri_rs::Result<()> {
        self.driver.down(key).await
    }

    async fn key_up(&self, key: &str) -> myuri_rs::Result<()> {
        self.driver.up(key).await
    }

    async fn insert_text(&self, text: &str) -> myuri_rs::Result<()> {
        self.driver.insert_text(text).await
    }

    async fn evaluate(
        &self,
        target: &Target,
        fn_source: &str,
        arg: serde_json::Value,
    ) -> myuri_rs::Result<serde_json::Value> {
        let script = myuri_rs::on_selector_script(fn_source);
        self.driver
            .evaluate(
                &script,
                Some(serde_json::json!({ "selector": target.selector, "arg": arg })),
            )
            .await
    }

    async fn offset(&self, target: &Target) -> myuri_rs::Result<Option<[f64; 2]>> {
        let script = myuri_rs::on_selector_script("() => true");
        let found = self
            .driver
            .evaluate(
                &script,
                Some(serde_json::json!({ "selector": target.selector, "arg": serde_json::Value::Null })),
            )
            .await?;
        Ok(found.as_bool().filter(|&b| b).map(|_| [0.0, 0.0]))
    }

    async fn viewport(&self) -> myuri_rs::Result<[f64; 2]> {
        let res = self.driver.evaluate(VIEWPORT_JS, None).await?;
        let arr = res.as_array().ok_or_else(|| {
            myuri_rs::MyuriError::InvalidArgument("viewport did not return array".into())
        })?;
        let w = arr.first().and_then(|v| v.as_f64()).unwrap_or(1280.0);
        let h = arr.get(1).and_then(|v| v.as_f64()).unwrap_or(720.0);
        Ok([w, h])
    }

    fn now(&self) -> f64 {
        self.started_at.elapsed().as_secs_f64() * 1000.0
    }
}

/// High-level humanized page helper routing channels per `HumanizeSetting` (`myuri` vs `raw`).
pub struct HumanizedPage {
    pub page: Page,
    pub actions: Actions,
    pub mouse_humanized: bool,
    pub keyboard_humanized: bool,
    pub scroll_humanized: bool,
}

impl HumanizedPage {
    pub fn attach(
        page: Page,
        humanize: Option<&HumanizeSetting>,
        os_name: &str,
    ) -> Result<Self> {
        let norm = match humanize {
            Some(h) => normalize(h)?,
            None => normalize(&HumanizeSetting::Bool(false))?,
        };
        let os = map_os(os_name);
        let seed = norm
            .seed
            .unwrap_or_else(myuri_rs::generate_random_seed);
        let override_cfg: Option<SettingsOverride> = if norm.settings.is_empty() {
            None
        } else {
            Some(
                serde_json::from_value(serde_json::Value::Object(norm.settings))
                    .map_err(|e| CamoufoxError::ValueError(format!("humanize.settings: {e}")))?,
            )
        };
        let mut profiled = override_cfg.unwrap_or_default();
        let _ = generate_profile(seed, Some(&profiled), Some(os));
        profiled.scroll.get_or_insert_with(Default::default).device =
            Some(myuri_rs::ScrollDevice::Mouse);

        let host = Arc::new(CamoufoxPlaywrightHost::new(page.clone()));
        let rng = make_rng(seed);
        let actions = Actions::new(
            host,
            rng,
            ActionsOptions {
                os: Some(os),
                device: Some(myuri_rs::ScrollDevice::Mouse),
                settings: Some(profiled),
            },
        );
        Ok(Self {
            page,
            actions,
            mouse_humanized: norm.engines["mouse"].engine != EngineKind::Raw,
            keyboard_humanized: norm.engines["keyboard"].engine != EngineKind::Raw,
            scroll_humanized: norm.engines["scroll"].engine != EngineKind::Raw,
        })
    }

    pub async fn click_selector(&mut self, selector: &str) -> Result<()> {
        if self.mouse_humanized {
            self.actions
                .click(&Target::new(selector), Default::default())
                .await?;
        } else {
            let loc: Locator = self.page.locator(selector);
            loc.click(None).await?;
        }
        Ok(())
    }

    pub async fn move_to(&mut self, x: f64, y: f64) -> Result<()> {
        if self.mouse_humanized {
            self.actions.move_to(x, y).await?;
        } else {
            self.page.mouse().move_to(x, y, None).await?;
        }
        Ok(())
    }

    pub async fn wheel(&mut self, dx: f64, dy: f64) -> Result<()> {
        if self.scroll_humanized {
            self.actions.wheel(dx, dy).await?;
        } else {
            self.page.mouse().wheel(dx, dy).await?;
        }
        Ok(())
    }

    pub async fn press(&mut self, key: &str) -> Result<()> {
        if self.keyboard_humanized {
            self.actions.key(key).await?;
        } else {
            self.page.keyboard().press(key, None).await?;
        }
        Ok(())
    }

    pub async fn type_text(&mut self, text: &str) -> Result<()> {
        if self.keyboard_humanized {
            self.actions.type_text(text).await?;
        } else {
            self.page.keyboard().type_text(text, None).await?;
        }
        Ok(())
    }
}
