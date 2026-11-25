//! Helpers to find public IP, validate IPs, and split SOCKS5 proxy auth, matching `ip.ts` / `ip.py`.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::exceptions::{CamoufoxError, Result};

pub const PROXY_AUTH_ENV: &str = "CAMOU_PROXY_AUTH";
pub const DIRECT_IP_TIMEOUT: Duration = Duration::from_secs(5);
pub const PROXIED_IP_TIMEOUT: Duration = Duration::from_secs(15);

const PUBLIC_IP_URLS: [&str; 6] = [
    "https://api.ipify.org",
    "https://checkip.amazonaws.com",
    "https://ipinfo.io/ip",
    "https://icanhazip.com",
    "https://ifconfig.co/ip",
    "https://ipecho.net/plain",
];

const IPV6_URLS: [&str; 2] = ["https://api6.ipify.org", "https://v6.ident.me"];

const PROXY_LOOKUP_FAILED: &str = "Could not look up where the proxy exits. Pass webrtc_ip, timezoneId, locale and geolocation explicitly to skip the lookup";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub server: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bypass: Option<String>,
}

impl ProxyConfig {
    pub fn new(server: impl Into<String>) -> Self {
        Self {
            server: server.into(),
            username: None,
            password: None,
            bypass: None,
        }
    }

    pub fn to_playwright(&self) -> playwright_rs::protocol::ProxySettings {
        let mut p = playwright_rs::protocol::ProxySettings::new(self.server.clone());
        if let Some(ref b) = self.bypass {
            p = p.bypass(b.clone());
        }
        if let Some(ref u) = self.username {
            p = p.username(u.clone());
        }
        if let Some(ref pw) = self.password {
            p = p.password(pw.clone());
        }
        p
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyAuth {
    pub host: String,
    pub port: Option<u16>,
    pub username: String,
    pub password: String,
}

pub struct ProxyHelper;

impl ProxyHelper {
    pub fn parse_server(server: &str) -> Result<(String, String, Option<String>)> {
        let trimmed = server.trim();
        let (schema, rest) = if let Some(idx) = trimmed.find("://") {
            (&trimmed[..idx], &trimmed[idx + 3..])
        } else {
            ("http", trimmed)
        };
        if rest.is_empty() {
            return Err(CamoufoxError::InvalidProxy(format!(
                "Invalid proxy server: {server}"
            )));
        }
        if rest.starts_with('[') {
            if let Some(close) = rest.find(']') {
                let host = &rest[..=close];
                let port = rest[close + 1..]
                    .strip_prefix(':')
                    .filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                    .map(str::to_owned);
                return Ok((schema.to_owned(), host.to_owned(), port));
            }
        }
        if let Some(colon) = rest.rfind(':') {
            let potential_port = &rest[colon + 1..];
            if !potential_port.is_empty() && potential_port.bytes().all(|b| b.is_ascii_digit()) {
                return Ok((
                    schema.to_owned(),
                    rest[..colon].to_owned(),
                    Some(potential_port.to_owned()),
                ));
            }
        }
        Ok((schema.to_owned(), rest.to_owned(), None))
    }

    pub fn as_string(proxy: &ProxyConfig) -> Result<String> {
        let (schema, host, port) = Self::parse_server(&proxy.server)?;
        let mut out = format!("{schema}://");
        if let Some(ref user) = proxy.username {
            if !user.is_empty() {
                out.push_str(&percent_encode_component(user));
                if let Some(ref pass) = proxy.password {
                    if !pass.is_empty() {
                        out.push(':');
                        out.push_str(&percent_encode_component(pass));
                    }
                }
                out.push('@');
            }
        }
        out.push_str(&host);
        if let Some(p) = port {
            out.push(':');
            out.push_str(&p);
        }
        Ok(out)
    }
}

fn percent_encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(*b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Splits a SOCKS5 proxy's credentials from what Playwright receives.
pub fn socks5_auth(proxy: Option<&ProxyConfig>) -> Result<(Option<ProxyConfig>, Option<ProxyAuth>)> {
    let Some(p) = proxy else {
        return Ok((None, None));
    };
    let has_creds = p.username.as_deref().is_some_and(|s| !s.is_empty())
        || p.password.as_deref().is_some_and(|s| !s.is_empty());
    if !has_creds {
        return Ok((Some(p.clone()), None));
    }
    let candidate = if p.server.contains("://") {
        p.server.clone()
    } else {
        format!("http://{}", p.server)
    };
    let url = Url::parse(&candidate)
        .map_err(|e| CamoufoxError::InvalidProxy(format!("Invalid proxy server: {} ({e})", p.server)))?;
    let Some(host) = url.host_str() else {
        return Ok((Some(p.clone()), None));
    };
    if !url.scheme().eq_ignore_ascii_case("socks5") {
        return Ok((Some(p.clone()), None));
    }
    let stripped_host = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host)
        .to_owned();
    let bare = ProxyConfig {
        server: p.server.clone(),
        username: None,
        password: None,
        bypass: p.bypass.clone(),
    };
    let auth = ProxyAuth {
        host: stripped_host,
        port: url.port(),
        username: p.username.clone().unwrap_or_default(),
        password: p.password.clone().unwrap_or_default(),
    };
    Ok((Some(bare), Some(auth)))
}

pub fn proxy_auth_env(auth: &ProxyAuth) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert(
        PROXY_AUTH_ENV.to_owned(),
        serde_json::to_string(auth).unwrap_or_default(),
    );
    env
}

pub fn proxy_auth_init_script(auth: &ProxyAuth) -> String {
    format!(
        "camoufoxProxyAuth: ({});",
        serde_json::to_string(auth).unwrap_or_default()
    )
}

pub fn valid_ipv4(ip: &str) -> bool {
    ip.parse::<Ipv4Addr>().is_ok()
}

pub fn valid_ipv6(ip: &str) -> bool {
    ip.parse::<Ipv6Addr>().is_ok()
}

pub fn validate_ip(ip: &str) -> Result<()> {
    if valid_ipv4(ip) || valid_ipv6(ip) {
        Ok(())
    } else {
        Err(CamoufoxError::InvalidIP(format!("Invalid IP address: {ip}")))
    }
}

fn build_client(proxy: Option<&str>) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(if proxy.is_some() {
            PROXIED_IP_TIMEOUT
        } else {
            DIRECT_IP_TIMEOUT
        })
        .user_agent(format!("camoufox-rs/{}", crate::version::VERSION));
    if let Some(p) = proxy {
        let reqwest_proxy = reqwest::Proxy::all(p)
            .map_err(|e| CamoufoxError::InvalidProxy(format!("Invalid proxy '{p}': {e}")))?;
        builder = builder.proxy(reqwest_proxy);
    }
    builder
        .build()
        .map_err(|e| CamoufoxError::InvalidIP(e.to_string()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitGeo {
    pub ip: String,
    pub timezone: String,
    pub country: String,
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IpApiResponse {
    status: Option<String>,
    message: Option<String>,
    query: Option<String>,
    timezone: Option<String>,
    country_code: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
}

pub async fn proxy_exit_geo(proxy: &str) -> Result<ExitGeo> {
    let client = build_client(Some(proxy))?;
    let resp = client
        .get("http://ip-api.com/json?fields=status,message,query,timezone,countryCode,lat,lon")
        .send()
        .await
        .map_err(|e| CamoufoxError::InvalidIP(format!("{PROXY_LOOKUP_FAILED}: {e}")))?;
    if !resp.status().is_success() {
        return Err(CamoufoxError::InvalidIP(format!(
            "{PROXY_LOOKUP_FAILED}: {} {}",
            resp.status(),
            resp.status().canonical_reason().unwrap_or("")
        )));
    }
    let data: IpApiResponse = resp
        .json()
        .await
        .map_err(|e| CamoufoxError::InvalidIP(format!("{PROXY_LOOKUP_FAILED}: {e}")))?;
    if data.status.as_deref() != Some("success") {
        return Err(CamoufoxError::InvalidIP(format!(
            "{PROXY_LOOKUP_FAILED}: {}",
            data.message.unwrap_or_else(|| "unknown error".into())
        )));
    }
    let (Some(ip), Some(timezone), Some(country), Some(latitude), Some(longitude)) = (
        data.query,
        data.timezone,
        data.country_code,
        data.lat,
        data.lon,
    ) else {
        return Err(CamoufoxError::InvalidIP(format!(
            "{PROXY_LOOKUP_FAILED}: missing fields in ip-api response"
        )));
    };
    validate_ip(&ip)?;
    Ok(ExitGeo {
        ip,
        timezone,
        country,
        latitude,
        longitude,
    })
}

static PUBLIC_IP_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static PUBLIC_IPV6_CACHE: OnceLock<Mutex<Option<Option<String>>>> = OnceLock::new();

pub async fn public_ip(proxy: Option<&str>) -> Result<String> {
    let key = proxy.unwrap_or("").to_owned();
    let cache = PUBLIC_IP_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(ip) = guard.get(&key) {
            return Ok(ip.clone());
        }
    }

    let client = build_client(proxy)?;
    let mut last_err = String::new();
    for url in PUBLIC_IP_URLS {
        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(text) = resp.text().await {
                    let ip = text.trim().to_owned();
                    if validate_ip(&ip).is_ok() {
                        if let Ok(mut guard) = cache.lock() {
                            guard.insert(key, ip.clone());
                        }
                        return Ok(ip);
                    }
                }
            }
            Ok(resp) => last_err = format!("HTTP {} for url: {url}", resp.status()),
            Err(err) => last_err = err.to_string(),
        }
    }
    Err(CamoufoxError::InvalidIP(format!(
        "Failed to get IP address: {last_err}"
    )))
}

pub async fn public_ipv6() -> Option<String> {
    let cache = PUBLIC_IPV6_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock() {
        if let Some(cached) = guard.as_ref() {
            return cached.clone();
        }
    }
    let Ok(client) = build_client(None) else {
        return None;
    };
    let mut found = None;
    for url in IPV6_URLS {
        if let Ok(resp) = client.get(url).send().await {
            if resp.status().is_success() {
                if let Ok(text) = resp.text().await {
                    let ip = text.trim().to_owned();
                    if valid_ipv6(&ip) {
                        found = Some(ip);
                        break;
                    }
                }
            }
        }
    }
    if let Ok(mut guard) = cache.lock() {
        *guard = Some(found.clone());
    }
    found
}
