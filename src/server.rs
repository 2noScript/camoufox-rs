//! Playwright remote server mode (`launch_server`), matching `server.ts` / `server.py`.

use std::collections::BTreeMap;

use playwright_rs::protocol::BindOptions;
use serde_json::Value;

use crate::async_api::{new_browser, CamoufoxBrowser};
use crate::exceptions::{CamoufoxError, Result};
use crate::humanize::driven_channels;
use crate::sync_api::camel_case;
use crate::utils::CamoufoxOptions;

pub fn to_camel_case_map(data: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    for (k, v) in data {
        let new_k = if k.contains('_') { camel_case(k) } else { k.clone() };
        out.insert(new_k, v.clone());
    }
    out
}

pub struct CamoufoxServer {
    pub browser_handle: CamoufoxBrowser,
    pub ws_endpoint: String,
}

impl CamoufoxServer {
    pub async fn close(&mut self) -> Result<()> {
        if let Some(ref b) = self.browser_handle.browser {
            let _ = b.unbind().await;
        }
        self.browser_handle.close().await
    }
}

/// Launches a Camoufox browser and exposes it over a Playwright WebSocket endpoint
/// via `Browser::bind`, rejecting `persistent_context` / `user_data_dir` and driver-side
/// `humanize` engines upfront.
pub async fn launch_server(mut options: CamoufoxOptions) -> Result<CamoufoxServer> {
    if options.persistent_context {
        return Err(CamoufoxError::ValueError(
            "launch_server() does not support 'persistent_context': Playwright cannot serve a \
             persistent context over a websocket endpoint. Use camoufox() with persistent_context instead."
                .into(),
        ));
    }
    if options.user_data_dir.is_some() {
        return Err(CamoufoxError::ValueError(
            "launch_server() does not support 'user_data_dir': Playwright cannot serve a \
             persistent context over a websocket endpoint. Use camoufox() with user_data_dir instead."
                .into(),
        ));
    }
    if !driven_channels(options.humanize.as_ref())?.is_empty() {
        return Err(CamoufoxError::ValueError(
            "launch_server() cannot run humanize engines: they run in the process that drives the page. \
             Serve without humanize, and attach on the client."
                .into(),
        ));
    }

    let port = options.port;
    options.humanize = None;
    let browser_handle = new_browser(options).await?;

    let mut bind_opts = BindOptions::default();
    if let Some(p) = port {
        bind_opts = bind_opts.port(p);
    }
    bind_opts = bind_opts.host("127.0.0.1");

    let bind_res = browser_handle
        .browser()?
        .bind("Camoufox", bind_opts)
        .await?;
    println!("Websocket endpoint: {}", bind_res.endpoint);

    Ok(CamoufoxServer {
        browser_handle,
        ws_endpoint: bind_res.endpoint,
    })
}
