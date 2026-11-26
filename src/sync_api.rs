//! Synchronous blocking wrappers over `async_api`, matching `sync_api.ts` / `sync_api.py`.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::runtime::{Builder, Runtime};

use crate::async_api::{
    camoufox as async_camoufox, new_browser as async_new_browser, CamoufoxBrowser,
    NewContextOptions,
};
use crate::exceptions::{CamoufoxError, Result};
use crate::headless_stream::HeadlessStreamClient;
use crate::utils::CamoufoxOptions;

/// Blocking handle to a launched Camoufox browser backed by a dedicated Tokio runtime.
pub struct SyncCamoufox {
    runtime: Arc<Runtime>,
    inner: Option<CamoufoxBrowser>,
}

impl SyncCamoufox {
    pub fn launch(options: CamoufoxOptions) -> Result<Self> {
        let rt = Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| CamoufoxError::ValueError(format!("Failed to build Tokio runtime: {e}")))?;
        let inner = rt.block_on(async_new_browser(options))?;
        Ok(Self {
            runtime: Arc::new(rt),
            inner: Some(inner),
        })
    }

    pub fn block_on<F: std::future::Future>(&self, fut: F) -> F::Output {
        self.runtime.block_on(fut)
    }

    pub fn new_context(&self, opts: NewContextOptions) -> Result<playwright_rs::protocol::BrowserContext> {
        let browser = self
            .inner
            .as_ref()
            .ok_or_else(|| CamoufoxError::ValueError("Camoufox already closed".into()))?;
        self.runtime.block_on(browser.new_context(opts))
    }

    pub fn new_page(&self) -> Result<playwright_rs::protocol::Page> {
        let browser = self
            .inner
            .as_ref()
            .ok_or_else(|| CamoufoxError::ValueError("Camoufox already closed".into()))?;
        self.runtime.block_on(browser.new_page())
    }

    pub fn headless_stream_socket(&self) -> Option<PathBuf> {
        self.inner
            .as_ref()
            .and_then(CamoufoxBrowser::headless_stream_socket)
    }

    pub fn connect_stream(&self) -> Result<HeadlessStreamClient> {
        let browser = self
            .inner
            .as_ref()
            .ok_or_else(|| CamoufoxError::ValueError("Camoufox already closed".into()))?;
        self.runtime.block_on(browser.connect_stream())
    }

    pub fn close(&mut self) -> Result<()> {
        if let Some(mut inner) = self.inner.take() {
            self.runtime.block_on(inner.close())?;
        }
        Ok(())
    }
}

impl Drop for SyncCamoufox {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

pub fn camoufox(options: CamoufoxOptions) -> Result<SyncCamoufox> {
    SyncCamoufox::launch(options)
}

pub async fn camoufox_async(options: CamoufoxOptions) -> Result<CamoufoxBrowser> {
    async_camoufox(options).await
}

/// Converts `snake_case` to `camelCase`, matching `camoufox.server.camel_case`.
pub fn camel_case(snake: &str) -> String {
    if snake.len() < 2 {
        return snake.to_owned();
    }
    let prefix = if snake.starts_with('_') { "_" } else { "" };
    let joined: String = snake
        .to_ascii_lowercase()
        .split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut chars = p.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect();
    let mut chars = joined.chars();
    match chars.next() {
        Some(first) => format!("{prefix}{}{}", first.to_ascii_lowercase(), chars.as_str()),
        None => prefix.to_owned(),
    }
}
