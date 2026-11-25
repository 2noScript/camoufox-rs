//! Per-channel driver-side humanization configuration, matching `humanize.ts` / `humanize.py`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::exceptions::{CamoufoxError, Result};

pub const CHANNELS: [&str; 3] = ["mouse", "keyboard", "scroll"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Mouse,
    Keyboard,
    Scroll,
}

impl Channel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mouse => "mouse",
            Self::Keyboard => "keyboard",
            Self::Scroll => "scroll",
        }
    }
}

pub fn channel_parts(channel: &str) -> &'static [&'static str] {
    match channel {
        "mouse" => &["move", "press", "point", "reaches"],
        "keyboard" => &["type", "key"],
        "scroll" => &["intoView", "wheel"],
        _ => &[],
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineKind {
    Raw,
    Myuri,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Engine {
    pub engine: EngineKind,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
}

pub fn raw() -> Engine {
    Engine {
        engine: EngineKind::Raw,
        options: BTreeMap::new(),
    }
}

pub fn myuri() -> Engine {
    Engine {
        engine: EngineKind::Myuri,
        options: BTreeMap::new(),
    }
}

pub fn cursory() -> Engine {
    myuri()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HumanizeSetting {
    Bool(bool),
    Config(HumanizeConfig),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HumanizeConfig {
    pub mouse: Option<Engine>,
    pub keyboard: Option<Engine>,
    pub scroll: Option<Engine>,
    pub seed: Option<u32>,
    pub settings: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedHumanize {
    pub engines: BTreeMap<String, Engine>,
    pub seed: Option<u32>,
    pub settings: Map<String, Value>,
}

fn validate_engine(channel: &str, engine: &Engine) -> Result<Engine> {
    if engine.engine == EngineKind::Raw {
        if !engine.options.is_empty() {
            return Err(CamoufoxError::ValueError(format!(
                "humanize[\"{channel}\"]: raw() takes no options"
            )));
        }
        return Ok(raw());
    }
    let allowed = channel_parts(channel);
    let mut misplaced: Vec<&String> = engine
        .options
        .keys()
        .filter(|k| !allowed.contains(&k.as_str()))
        .collect();
    misplaced.sort();
    if let Some(first) = misplaced.first() {
        return Err(CamoufoxError::ValueError(format!(
            "humanize[\"{channel}\"]: myuri({{ {first} }}) is not a {channel} part; \
             the {channel} parts are {}",
            allowed.join(", ")
        )));
    }
    Ok(engine.clone())
}

pub fn normalize(humanize: &HumanizeSetting) -> Result<NormalizedHumanize> {
    let every = |eng: fn() -> Engine| -> BTreeMap<String, Engine> {
        CHANNELS
            .iter()
            .map(|c| ((*c).to_owned(), eng()))
            .collect()
    };
    match humanize {
        HumanizeSetting::Bool(false) => Ok(NormalizedHumanize {
            engines: every(raw),
            seed: None,
            settings: Map::new(),
        }),
        HumanizeSetting::Bool(true) => Ok(NormalizedHumanize {
            engines: every(myuri),
            seed: None,
            settings: Map::new(),
        }),
        HumanizeSetting::Config(cfg) => {
            let mut settings_map = Map::new();
            if let Some(ref s) = cfg.settings {
                let Some(obj) = s.as_object() else {
                    return Err(CamoufoxError::ValueError(
                        "humanize.settings must be an object of myuri settings".into(),
                    ));
                };
                // Validate settings via myuri-rs.
                if let Err(err) = serde_json::from_value::<myuri_rs::SettingsOverride>(s.clone()) {
                    return Err(CamoufoxError::ValueError(format!(
                        "humanize.settings: {err}"
                    )));
                }
                settings_map = obj.clone();
            }
            let mut engines = BTreeMap::new();
            let pairs = [
                ("mouse", cfg.mouse.as_ref()),
                ("keyboard", cfg.keyboard.as_ref()),
                ("scroll", cfg.scroll.as_ref()),
            ];
            for (ch, opt) in pairs {
                let eng = opt.cloned().unwrap_or_else(myuri);
                engines.insert(ch.to_owned(), validate_engine(ch, &eng)?);
            }
            Ok(NormalizedHumanize {
                engines,
                seed: cfg.seed,
                settings: settings_map,
            })
        }
    }
}

pub fn driven_channels(humanize: Option<&HumanizeSetting>) -> Result<Vec<String>> {
    let Some(h) = humanize else {
        return Ok(Vec::new());
    };
    let norm = normalize(h)?;
    Ok(CHANNELS
        .iter()
        .filter(|c| norm.engines[**c].engine != EngineKind::Raw)
        .map(|c| (*c).to_owned())
        .collect())
}
