//! Locale data structures, validation, and statistical CLDR locale selector, matching `locales.ts` / `locales.py`.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::exceptions::{CamoufoxError, Result};
use crate::paths::data_file_str;
use crate::warnings::LeakWarning;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Locale {
    pub language: String,
    pub region: Option<String>,
    pub script: Option<String>,
}

impl Locale {
    pub fn new(
        language: impl Into<String>,
        region: Option<String>,
        script: Option<String>,
    ) -> Self {
        Self {
            language: language.into(),
            region,
            script,
        }
    }

    pub fn as_string(&self) -> String {
        match &self.region {
            Some(region) => format!("{}-{region}", self.language),
            None => self.language.clone(),
        }
    }

    pub fn as_config(&self) -> Result<Map<String, Value>> {
        let Some(ref region) = self.region else {
            return Err(CamoufoxError::LocaleError(
                "Locale::as_config() needs a region".into(),
            ));
        };
        let mut map = Map::new();
        map.insert("locale:region".into(), Value::String(region.clone()));
        map.insert("locale:language".into(), Value::String(self.language.clone()));
        if let Some(ref script) = self.script {
            map.insert("locale:script".into(), Value::String(script.clone()));
        }
        Ok(map)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Geolocation {
    pub locale: Locale,
    pub longitude: f64,
    pub latitude: f64,
    pub timezone: String,
    pub accuracy: Option<f64>,
}

impl Geolocation {
    pub fn new(
        locale: Locale,
        longitude: f64,
        latitude: f64,
        timezone: impl Into<String>,
        accuracy: Option<f64>,
    ) -> Self {
        Self {
            locale,
            longitude,
            latitude,
            timezone: timezone.into(),
            accuracy,
        }
    }

    pub fn as_config(&self) -> Result<Map<String, Value>> {
        let mut map = self.locale.as_config()?;
        map.insert("geolocation:longitude".into(), serde_json::json!(self.longitude));
        map.insert("geolocation:latitude".into(), serde_json::json!(self.latitude));
        map.insert("timezone".into(), Value::String(self.timezone.clone()));
        if let Some(acc) = self.accuracy {
            map.insert("geolocation:accuracy".into(), serde_json::json!(acc));
        }
        Ok(map)
    }
}

/// Suppress-Script map for major BCP-47 languages (IANA language-subtag-registry).
/// Matches Python `language_tags` / `langcodes` `suppress_script` for common tags.
fn suppress_script_for_language(lang: &str) -> Option<&'static str> {
    match lang.to_ascii_lowercase().as_str() {
        "af" | "ca" | "cs" | "cy" | "da" | "de" | "en" | "es" | "et" | "eu" | "fi" | "fil"
        | "fr" | "ga" | "gl" | "hr" | "hu" | "id" | "is" | "it" | "lb" | "lt" | "lv" | "ms"
        | "mt" | "nb" | "nl" | "nn" | "no" | "pl" | "pt" | "ro" | "sk" | "sl" | "sq" | "sv"
        | "sw" | "tr" | "vi" => Some("Latn"),
        "be" | "bg" | "mk" | "ru" | "uk" => Some("Cyrl"),
        "ar" | "fa" | "ps" | "ur" => Some("Arab"),
        "el" => Some("Grek"),
        "he" | "iw" | "yi" => Some("Hebr"),
        "hi" | "mr" | "ne" => Some("Deva"),
        "bn" => Some("Beng"),
        "gu" => Some("Gujr"),
        "pa" => Some("Guru"),
        "kn" => Some("Knda"),
        "ml" => Some("Mlym"),
        "or" => Some("Orya"),
        "si" => Some("Sinh"),
        "ta" => Some("Taml"),
        "te" => Some("Telu"),
        "th" => Some("Thai"),
        "lo" => Some("Laoo"),
        "km" => Some("Khmr"),
        "my" => Some("Mymr"),
        "ka" => Some("Geor"),
        "hy" => Some("Armn"),
        "am" => Some("Ethi"),
        "ja" => Some("Jpan"),
        "ko" => Some("Kore"),
        _ => None,
    }
}

fn is_valid_lang_subtag(s: &str) -> bool {
    (2..=8).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphabetic())
}

fn is_valid_script_subtag(s: &str) -> bool {
    s.len() == 4 && s.bytes().all(|b| b.is_ascii_alphabetic())
}

fn is_valid_region_subtag(s: &str) -> bool {
    (s.len() == 2 && s.bytes().all(|b| b.is_ascii_alphabetic()))
        || (s.len() == 3 && s.bytes().all(|b| b.is_ascii_digit()))
}

fn parse_bcp47_parts(input: &str) -> Option<(String, Option<String>, Option<String>)> {
    let parts: Vec<&str> = input.trim().split(['-', '_']).collect();
    if parts.is_empty() || !is_valid_lang_subtag(parts[0]) {
        return None;
    }
    let language = parts[0].to_ascii_lowercase();
    let mut idx = 1usize;
    let mut script = None;
    let mut region = None;

    if idx < parts.len() && is_valid_script_subtag(parts[idx]) {
        let mut chars = parts[idx].chars();
        if let Some(first) = chars.next() {
            script = Some(format!(
                "{}{}",
                first.to_ascii_uppercase(),
                chars.as_str().to_ascii_lowercase()
            ));
        }
        idx += 1;
    }
    if idx < parts.len() && is_valid_region_subtag(parts[idx]) {
        region = Some(parts[idx].to_ascii_uppercase());
        idx += 1;
    }
    if idx < parts.len() {
        return None;
    }
    Some((language, script, region))
}

/// Validates and normalizes a BCP-47 locale with language + region.
/// Uses the language's Suppress-Script (matching `locales.py` / `locales.ts`).
pub fn normalize_locale(locale: &str) -> Result<Locale> {
    let Some((language, _explicit_script, region)) = parse_bcp47_parts(locale) else {
        return Err(CamoufoxError::invalid_locale_input(locale));
    };
    let Some(region) = region else {
        return Err(CamoufoxError::invalid_locale_input(locale));
    };
    let suppress = suppress_script_for_language(&language).map(str::to_owned);
    Ok(Locale::new(language, Some(region), suppress))
}

pub fn handle_locale(locale: &str, ignore_region: bool) -> Result<Locale> {
    if locale.len() > 3 {
        return normalize_locale(locale);
    }
    match StatisticalLocaleSelector::from_region(locale) {
        Ok(loc) => return Ok(loc),
        Err(CamoufoxError::UnknownTerritory(_)) => {}
        Err(err) => return Err(err),
    }
    if ignore_region {
        let Some((lang, _, _)) = parse_bcp47_parts(locale) else {
            return Err(CamoufoxError::invalid_locale_input(locale));
        };
        return Ok(Locale::new(lang, None, None));
    }
    match StatisticalLocaleSelector::from_language(locale) {
        Ok(loc) => {
            LeakWarning::warn("no_region", None);
            Ok(loc)
        }
        Err(CamoufoxError::UnknownLanguage(_)) => Err(CamoufoxError::invalid_locale_input(locale)),
        Err(err) => Err(err),
    }
}

pub fn handle_locales(locales: &[String], config: &mut Map<String, Value>) -> Result<()> {
    if locales.is_empty() {
        return Ok(());
    }
    let first = handle_locale(&locales[0], false)?;
    for (k, v) in first.as_config()? {
        config.insert(k, v);
    }
    if locales.len() < 2 {
        return Ok(());
    }
    let mut resolved = Vec::with_capacity(locales.len());
    let mut seen = HashSet::new();
    for item in locales {
        let s = handle_locale(item, true)?.as_string();
        if seen.insert(s.clone()) {
            resolved.push(s);
        }
    }
    config.insert("locale:all".into(), Value::String(resolved.join(", ")));
    Ok(())
}

#[derive(Debug, Clone)]
struct LanguagePopulation {
    lang_type: String,
    population_percent: f64,
}

#[derive(Debug, Clone)]
struct TerritoryRecord {
    territory_type: String,
    literacy_percent: f64,
    population: f64,
    languages: Vec<LanguagePopulation>,
}

fn territory_data() -> &'static Vec<TerritoryRecord> {
    static DATA: OnceLock<Vec<TerritoryRecord>> = OnceLock::new();
    DATA.get_or_init(|| {
        let xml = data_file_str("territoryInfo.xml").expect("territoryInfo.xml bundled");
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);

        let mut territories = Vec::new();
        let mut current: Option<TerritoryRecord> = None;
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) if e.name().as_ref() == b"territory" => {
                    let mut t_type = String::new();
                    let mut lit = 0.0;
                    let mut pop = 0.0;
                    for attr in e.attributes().flatten() {
                        let val = String::from_utf8_lossy(&attr.value);
                        match attr.key.as_ref() {
                            b"type" => t_type = val.into_owned(),
                            b"literacyPercent" => lit = val.trim().parse().unwrap_or(0.0),
                            b"population" => pop = val.trim().parse().unwrap_or(0.0),
                            _ => {}
                        }
                    }
                    current = Some(TerritoryRecord {
                        territory_type: t_type,
                        literacy_percent: lit,
                        population: pop,
                        languages: Vec::new(),
                    });
                }
                Ok(Event::Empty(ref e)) if e.name().as_ref() == b"languagePopulation" => {
                    if let Some(ref mut rec) = current {
                        let mut l_type = String::new();
                        let mut pct = 0.0;
                        for attr in e.attributes().flatten() {
                            let val = String::from_utf8_lossy(&attr.value);
                            match attr.key.as_ref() {
                                b"type" => l_type = val.into_owned(),
                                b"populationPercent" => pct = val.trim().parse().unwrap_or(0.0),
                                _ => {}
                            }
                        }
                        rec.languages.push(LanguagePopulation {
                            lang_type: l_type,
                            population_percent: pct,
                        });
                    }
                }
                Ok(Event::End(ref e)) if e.name().as_ref() == b"territory" => {
                    if let Some(rec) = current.take() {
                        territories.push(rec);
                    }
                }
                Ok(Event::Eof) => break,
                Err(err) => panic!("Invalid territoryInfo.xml: {err}"),
                _ => {}
            }
            buf.clear();
        }
        territories
    })
}

pub fn pairwise_sum(values: &[f64]) -> f64 {
    fn sum_slice(s: &[f64]) -> f64 {
        let n = s.len();
        if n < 8 {
            return s.iter().copied().sum();
        }
        if n <= 128 {
            let mut r = [s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]];
            let mut i = 8usize;
            while i + 8 <= n {
                for j in 0..8 {
                    r[j] += s[i + j];
                }
                i += 8;
            }
            let mut res = r[0] + r[1] + (r[2] + r[3]) + (r[4] + r[5] + (r[6] + r[7]));
            while i < n {
                res += s[i];
                i += 1;
            }
            return res;
        }
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        sum_slice(&s[..n2]) + sum_slice(&s[n2..])
    }
    sum_slice(values)
}

fn normalize_probabilities(items: Vec<String>, freq: Vec<f64>) -> (Vec<String>, Vec<f64>) {
    let total = pairwise_sum(&freq);
    let probs = freq.into_iter().map(|f| f / total).collect();
    (items, probs)
}

pub fn weighted_choice<'a, T>(items: &'a [T], probs: &[f64], u: f64) -> &'a T {
    let mut cdf = Vec::with_capacity(probs.len());
    let mut acc = 0.0;
    for &p in probs {
        acc += p;
        cdf.push(acc);
    }
    let last = *cdf.last().unwrap_or(&1.0);
    for x in &mut cdf {
        *x /= last;
    }
    let mut idx = 0usize;
    while idx < cdf.len() && cdf[idx] <= u {
        idx += 1;
    }
    &items[idx.min(items.len() - 1)]
}

fn os_random_unit() -> f64 {
    let mut rng = crate::pycompat::PyRandom::new(None);
    rng.random()
}

pub struct StatisticalLocaleSelector;

impl StatisticalLocaleSelector {
    pub fn from_region(region: &str) -> Result<Locale> {
        let territory = territory_data()
            .iter()
            .find(|t| t.territory_type == region)
            .ok_or_else(|| {
                CamoufoxError::UnknownTerritory(format!("Unknown territory: {region}"))
            })?;
        if territory.languages.is_empty() {
            return Err(CamoufoxError::LocaleError(format!(
                "No language data found for region: {region}"
            )));
        }
        let langs: Vec<String> = territory
            .languages
            .iter()
            .map(|l| l.lang_type.clone())
            .collect();
        let pcts: Vec<f64> = territory
            .languages
            .iter()
            .map(|l| l.population_percent)
            .collect();
        let (items, probs) = normalize_probabilities(langs, pcts);
        let picked = weighted_choice(&items, &probs, os_random_unit()).replace('_', "-");
        normalize_locale(&format!("{picked}-{region}"))
    }

    pub fn from_language(language: &str) -> Result<Locale> {
        let mut regions = Vec::new();
        let mut weights = Vec::new();
        for terr in territory_data() {
            if let Some(lp) = terr.languages.iter().find(|l| l.lang_type == language) {
                regions.push(terr.territory_type.clone());
                weights.push(
                    ((lp.population_percent * terr.literacy_percent) / 10_000.0) * terr.population,
                );
            }
        }
        if regions.is_empty() {
            return Err(CamoufoxError::UnknownLanguage(format!(
                "No region data found for language: {language}"
            )));
        }
        let (items, probs) = normalize_probabilities(regions, weights);
        let picked = weighted_choice(&items, &probs, os_random_unit());
        normalize_locale(&format!("{language}-{picked}"))
    }
}

pub fn lookup_territory_languages(iso: &str) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    if let Some(t) = territory_data().iter().find(|t| t.territory_type == iso) {
        for lp in &t.languages {
            out.insert(lp.lang_type.clone(), lp.population_percent);
        }
    }
    out
}
