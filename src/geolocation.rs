//! MaxMind GeoIP MMDB download, update checks, and IP geolocation resolution, matching `geolocation.ts` / `geolocation.py`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use maxminddb::Reader;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::exceptions::{CamoufoxError, Result};
use crate::ip::validate_ip;
use crate::locales::{Geolocation, StatisticalLocaleSelector};
use crate::paths::{install_dir, load_data_value};
use crate::pkgman::{unzip, webdl};
use crate::warnings::{load_warnings, warn};

pub fn geoip_dir() -> PathBuf {
    install_dir().join("geoip")
}

pub fn mmdb_dir() -> PathBuf {
    geoip_dir().join("mmdb")
}

pub fn geoip_config_path() -> PathBuf {
    geoip_dir().join("config.yml")
}

const UPDATE_DAYS: f64 = 8.0;
const RECHECK_DAYS: f64 = 1.0;
const FROZEN_DAYS: f64 = 30.0;
const DAY_SECS: f64 = 24.0 * 60.0 * 60.0;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GeoIpRepo {
    pub name: String,
    pub urls: serde_json::Map<String, Value>,
    pub paths: serde_json::Map<String, Value>,
    #[serde(default)]
    pub extract: bool,
    #[serde(default)]
    pub deprecated: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SavedGeoIpConfig {
    name: Option<String>,
    #[serde(default)]
    explicit: bool,
}

pub fn allow_geoip() -> bool {
    true
}

pub fn geoip_allowed() -> Result<()> {
    Ok(())
}

fn find_in<'a>(mut data: &'a Value, key: &str) -> Option<&'a Value> {
    for part in key.split('.') {
        data = data.as_object()?.get(part)?;
        if data.is_null() {
            return None;
        }
    }
    Some(data)
}

pub fn load_geoip_repos() -> Result<(Vec<GeoIpRepo>, String)> {
    let val = load_data_value("repos.yml")?;
    let repos_val = val
        .get("geoip")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let repos: Vec<GeoIpRepo> =
        serde_json::from_value(repos_val).map_err(|e| CamoufoxError::ValueError(e.to_string()))?;
    let default_name = val
        .get("default")
        .and_then(|d| d.get("geoip"))
        .and_then(Value::as_str)
        .unwrap_or("GeoIP AIO by daijro")
        .to_owned();
    Ok((repos, default_name))
}

pub fn get_geoip_config_by_name(name: Option<&str>) -> Result<GeoIpRepo> {
    let (repos, default_name) = load_geoip_repos()?;
    let target = name.filter(|s| !s.is_empty()).unwrap_or(&default_name);
    for repo in &repos {
        if repo.name.eq_ignore_ascii_case(target) {
            return Ok(repo.clone());
        }
    }
    if let Some(given) = name {
        let available: Vec<String> = repos.iter().map(|r| format!("'{}'", r.name)).collect();
        return Err(CamoufoxError::ValueError(format!(
            "GeoIP database '{given}' not found. Available: [{}]",
            available.join(", ")
        )));
    }
    repos
        .into_iter()
        .next()
        .ok_or_else(|| CamoufoxError::ValueError("No GeoIP repos configured in repos.yml".into()))
}

pub fn warn_if_deprecated(config: &GeoIpRepo) {
    if config.deprecated {
        if let Ok((_, default_name)) = load_geoip_repos() {
            let tmpl = load_warnings()
                .get("geoip_deprecated")
                .map(String::as_str)
                .unwrap_or("GeoIP source {name} is deprecated; use {default}");
            warn(
                &tmpl.replace("{name}", &config.name).replace("{default}", &default_name),
                "FutureWarning",
            );
        }
    }
}

pub fn load_geoip_config() -> Result<GeoIpRepo> {
    let cfg_path = geoip_config_path();
    if cfg_path.exists() {
        let raw = fs::read_to_string(&cfg_path)?;
        let saved: SavedGeoIpConfig = serde_yaml_ng::from_str(&raw)
            .unwrap_or(SavedGeoIpConfig {
                name: None,
                explicit: false,
            });
        let (repos, _) = load_geoip_repos()?;
        let known: Vec<String> = repos.iter().map(|r| r.name.to_ascii_lowercase()).collect();
        let saved_name = saved.name.as_deref().unwrap_or("");
        let name = if saved.explicit || known.contains(&saved_name.to_ascii_lowercase()) {
            saved.name.as_deref()
        } else {
            None
        };
        let config = get_geoip_config_by_name(name)?;
        if config.deprecated && !saved.explicit {
            return get_geoip_config_by_name(None);
        }
        return Ok(config);
    }
    get_geoip_config_by_name(None)
}

pub fn save_geoip_config(config: &GeoIpRepo, explicit: bool) -> Result<()> {
    fs::create_dir_all(geoip_dir())?;
    let mut map = serde_yaml_ng::Mapping::new();
    map.insert(
        serde_yaml_ng::Value::String("name".into()),
        serde_yaml_ng::Value::String(config.name.clone()),
    );
    if explicit {
        map.insert(
            serde_yaml_ng::Value::String("explicit".into()),
            serde_yaml_ng::Value::Bool(true),
        );
    }
    let yaml = serde_yaml_ng::to_string(&map)
        .map_err(|e| CamoufoxError::Serialization(e.to_string()))?;
    fs::write(geoip_config_path(), yaml)?;
    Ok(())
}

pub fn get_mmdb_path(ip_version: &str, config: Option<&GeoIpRepo>) -> Result<PathBuf> {
    let cfg = match config {
        Some(c) => c.clone(),
        None => load_geoip_config()?,
    };
    let name = cfg.name.to_ascii_lowercase();
    if cfg.urls.contains_key("combined") {
        Ok(mmdb_dir().join(format!("{name}-combined.mmdb")))
    } else {
        Ok(mmdb_dir().join(format!("{name}-{ip_version}.mmdb")))
    }
}

fn chosen_explicitly(config: &GeoIpRepo) -> bool {
    let cfg_path = geoip_config_path();
    let Ok(raw) = fs::read_to_string(cfg_path) else {
        return false;
    };
    let Ok(saved) = serde_yaml_ng::from_str::<SavedGeoIpConfig>(&raw) else {
        return false;
    };
    saved.explicit && saved.name.as_deref() == Some(&config.name)
}

fn remove_deprecated_databases(keep: &GeoIpRepo) {
    let Ok((repos, _)) = load_geoip_repos() else {
        return;
    };
    let mdir = mmdb_dir();
    let Ok(entries) = fs::read_dir(&mdir) else {
        return;
    };
    let deprecated_prefixes: Vec<String> = repos
        .into_iter()
        .filter(|r| r.deprecated && r.name != keep.name)
        .map(|r| format!("{}-", r.name.to_ascii_lowercase()))
        .collect();
    for entry in entries.flatten() {
        let fname = entry.file_name().to_string_lossy().into_owned();
        if fname.ends_with(".mmdb") && deprecated_prefixes.iter().any(|p| fname.starts_with(p)) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn find_first_mmdb(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(found) = find_first_mmdb(&p) {
                return Some(found);
            }
        } else if p.extension().and_then(|e| e.to_str()) == Some("mmdb") {
            return Some(p);
        }
    }
    None
}

fn build_age_days(mmdb_path: &Path) -> Option<f64> {
    let reader = Reader::open_readfile(mmdb_path).ok()?;
    let epoch = reader.metadata.build_epoch as f64;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs_f64();
    Some((now - epoch) / DAY_SECS)
}

pub fn needs_update(config: Option<&GeoIpRepo>) -> Result<bool> {
    let cfg = match config {
        Some(c) => c.clone(),
        None => load_geoip_config()?,
    };
    let ipv4_path = get_mmdb_path("ipv4", Some(&cfg))?;
    if !ipv4_path.exists() {
        return Ok(true);
    }
    let meta = fs::metadata(&ipv4_path)?;
    let modified = meta.modified().unwrap_or(UNIX_EPOCH);
    let checked_days = SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::ZERO)
        .as_secs_f64()
        / DAY_SECS;
    if checked_days < RECHECK_DAYS {
        return Ok(false);
    }
    Ok(build_age_days(&ipv4_path).is_none_or(|d| d > UPDATE_DAYS))
}

pub async fn download_mmdb(source: Option<&str>, activate: bool) -> Result<()> {
    let config = match source {
        Some(name) => get_geoip_config_by_name(Some(name))?,
        None => load_geoip_config()?,
    };
    fs::create_dir_all(mmdb_dir())?;
    let name_lower = config.name.to_ascii_lowercase();

    for (ip_ver, raw_list) in &config.urls {
        let urls: Vec<String> = match raw_list {
            Value::String(s) => vec![s.clone()],
            Value::Array(arr) => arr
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            _ => continue,
        };
        let mmdb_path = mmdb_dir().join(format!("{name_lower}-{ip_ver}.mmdb"));
        let mut last_err = None;
        let mut done = false;

        for url in &urls {
            let tmp_dir = geoip_dir().join(format!(".download-{}", std::process::id()));
            let _ = fs::create_dir_all(&tmp_dir);
            let res: Result<()> = async {
                let bytes = webdl(url, Some(&format!("Downloading {}", config.name)), true).await?;
                if config.extract {
                    let tmp_zip = tmp_dir.join("archive.zip");
                    fs::write(&tmp_zip, &bytes)?;
                    unzip(&tmp_zip, &tmp_dir)?;
                    let found = find_first_mmdb(&tmp_dir).ok_or_else(|| {
                        CamoufoxError::FileNotFound("No .mmdb file found in archive".into())
                    })?;
                    fs::rename(found, &mmdb_path)?;
                } else {
                    fs::write(&mmdb_path, &bytes)?;
                }
                Ok(())
            }
            .await;
            let _ = fs::remove_dir_all(&tmp_dir);
            match res {
                Ok(()) => {
                    done = true;
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        if !done {
            return Err(last_err.unwrap_or_else(|| {
                CamoufoxError::ValueError(format!("Failed to download {ip_ver}"))
            }));
        }
        if let Some(age) = build_age_days(&mmdb_path) {
            if age > FROZEN_DAYS {
                let tmpl = load_warnings()
                    .get("geoip_frozen")
                    .map(String::as_str)
                    .unwrap_or("GeoIP {name} is {days} days old");
                warn(
                    &tmpl
                        .replace("{name}", &config.name)
                        .replace("{days}", &(age.trunc() as i64).to_string()),
                    "RuntimeWarning",
                );
            }
        }
    }

    remove_deprecated_databases(&config);
    if activate {
        let exp = source.is_some() || chosen_explicitly(&config);
        save_geoip_config(&config, exp)?;
    }
    Ok(())
}

pub async fn get_geolocation(ip: &str, geoip_db: Option<&str>) -> Result<Geolocation> {
    validate_ip(ip)?;
    let ip_version = if ip.contains(':') { "ipv6" } else { "ipv4" };
    let config = match geoip_db {
        Some(name) => get_geoip_config_by_name(Some(name))?,
        None => load_geoip_config()?,
    };
    warn_if_deprecated(&config);
    let mmdb_path = get_mmdb_path(ip_version, Some(&config))?;
    if !mmdb_path.exists() || needs_update(Some(&config))? {
        download_mmdb(geoip_db, geoip_db.is_none()).await?;
    }

    let reader = Reader::open_readfile(&mmdb_path)
        .map_err(|e| CamoufoxError::ValueError(format!("Failed to open mmdb: {e}")))?;
    let addr: std::net::IpAddr = ip
        .parse()
        .map_err(|e| CamoufoxError::InvalidIP(format!("Invalid IP {ip}: {e}")))?;
    let record: Value = reader
        .lookup(addr)
        .map_err(|_| CamoufoxError::UnknownIPLocation(format!("IP not found in database: {ip}")))?;

    let path_str = |key: &str| -> String {
        config
            .paths
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(key)
            .to_owned()
    };
    let iso_code = find_in(&record, &path_str("iso_code"))
        .and_then(Value::as_str)
        .unwrap_or("US")
        .to_ascii_uppercase();
    let longitude = find_in(&record, &path_str("longitude"))
        .and_then(Value::as_f64)
        .ok_or_else(|| {
            CamoufoxError::UnknownIPLocation(format!("Missing longitude for IP {ip}"))
        })?;
    let latitude = find_in(&record, &path_str("latitude"))
        .and_then(Value::as_f64)
        .ok_or_else(|| {
            CamoufoxError::UnknownIPLocation(format!("Missing latitude for IP {ip}"))
        })?;
    let timezone = find_in(&record, &path_str("timezone"))
        .and_then(Value::as_str)
        .unwrap_or("UTC")
        .to_owned();

    let locale = StatisticalLocaleSelector::from_region(&iso_code)?;
    Ok(Geolocation::new(locale, longitude, latitude, timezone, None))
}
