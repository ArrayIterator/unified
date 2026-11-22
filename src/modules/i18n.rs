// Translations / i18n Manager module.
//
// Built-in language catalogue (ISO 639-1 codes), a key/value translation
// store held in memory and persisted to `data/i18n/<lang>.json` files,
// dynamic translation overrides, and a translate lookup that falls back
// to a caller-supplied string when no translation exists.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::json as api_json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Language catalogue
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Language {
    pub code: String,
    pub name: String,
    pub native_name: String,
    pub rtl: bool,
}

/// Built-in languages. The `name` is the English name; `native_name` is
/// the language's own name for itself.
pub fn builtin_languages() -> Vec<Language> {
    vec![
        Language {
            code: "en".to_string(),
            name: "English".to_string(),
            native_name: "English".to_string(),
            rtl: false,
        },
        Language {
            code: "id".to_string(),
            name: "Indonesian".to_string(),
            native_name: "Bahasa Indonesia".to_string(),
            rtl: false,
        },
        Language {
            code: "ar".to_string(),
            name: "Arabic".to_string(),
            native_name: "العربية".to_string(),
            rtl: true,
        },
        Language {
            code: "zh".to_string(),
            name: "Chinese".to_string(),
            native_name: "中文".to_string(),
            rtl: false,
        },
        Language {
            code: "es".to_string(),
            name: "Spanish".to_string(),
            native_name: "Español".to_string(),
            rtl: false,
        },
        Language {
            code: "fr".to_string(),
            name: "French".to_string(),
            native_name: "Français".to_string(),
            rtl: false,
        },
        Language {
            code: "de".to_string(),
            name: "German".to_string(),
            native_name: "Deutsch".to_string(),
            rtl: false,
        },
        Language {
            code: "pt".to_string(),
            name: "Portuguese".to_string(),
            native_name: "Português".to_string(),
            rtl: false,
        },
        Language {
            code: "ja".to_string(),
            name: "Japanese".to_string(),
            native_name: "日本語".to_string(),
            rtl: false,
        },
        Language {
            code: "ko".to_string(),
            name: "Korean".to_string(),
            native_name: "한국어".to_string(),
            rtl: false,
        },
        Language {
            code: "nl".to_string(),
            name: "Dutch".to_string(),
            native_name: "Nederlands".to_string(),
            rtl: false,
        },
        Language {
            code: "ru".to_string(),
            name: "Russian".to_string(),
            native_name: "Русский".to_string(),
            rtl: false,
        },
    ]
}

pub fn is_builtin_language(code: &str) -> bool {
    builtin_languages().iter().any(|language| language.code == code)
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Translation {
    pub lang: String,
    pub key: String,
    pub value: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct UpsertTranslationInput {
    pub lang: String,
    pub key: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct TranslateInput {
    pub lang: String,
    pub key: String,
    pub fallback: Option<String>,
    /// Optional gettext-style message context (`msgctxt`).
    pub context: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TranslatePluralInput {
    pub lang: String,
    pub singular: String,
    pub plural: String,
    pub count: f64,
    pub context: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LoadPoCatalogInput {
    pub lang: String,
    pub content: String,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct I18nState {
    /// Keyed by (language code, translation key).
    pub translations: Arc<DashMap<(String, String), Translation>>,
    /// Plural forms, keyed by (language code, singular key). Forms are
    /// stored in CLDR plural-form order; a contextual entry uses the same
    /// `\x04`-joined key as `contextual_key`. The first form is the
    /// singular form (`msgstr[0]` in gettext terms).
    pub plurals: Arc<DashMap<(String, String), Vec<String>>>,
    pub dir: PathBuf,
}

impl I18nState {
    pub fn new(dir: PathBuf) -> Self {
        let state = Self {
            translations: Arc::new(DashMap::new()),
            plurals: Arc::new(DashMap::new()),
            dir,
        };
        state.seed_defaults();
        state
    }

    /// A small set of built-in English strings so the default language
    /// is usable before any override is stored. Other languages start
    /// empty until overrides or files provide them.
    fn seed_defaults(&self) {
        let defaults: &[(&str, &str)] = &[
            ("app.name", "Unified"),
            ("nav.dashboard", "Dashboard"),
            ("nav.settings", "Settings"),
            ("common.save", "Save"),
            ("common.cancel", "Cancel"),
            ("common.search", "Search"),
            ("common.delete", "Delete"),
            ("auth.login", "Sign in"),
            ("auth.logout", "Sign out"),
        ];
        let now = Utc::now();
        for (key, value) in defaults {
            self.translations.insert(
                ("en".to_string(), (*key).to_string()),
                Translation {
                    lang: "en".to_string(),
                    key: (*key).to_string(),
                    value: (*value).to_string(),
                    updated_at: now,
                },
            );
        }
    }

    fn file_path(&self, lang: &str) -> PathBuf {
        self.dir.join(format!("{lang}.json"))
    }

    /// Load every `<lang>.json` file from the translations directory.
    /// Files win over the built-in seed values.
    pub fn load_from_disk(&self) -> UnifiedResult<()> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(UnifiedError::from(error)),
        };
        for entry in entries {
            let entry = entry.map_err(UnifiedError::from)?;
            let path = entry.path();
            let is_json = path
                .extension()
                .is_some_and(|extension| extension == "json");
            if !is_json {
                continue;
            }
            let Some(lang) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            if !is_builtin_language(&lang) {
                continue;
            }
            let text = std::fs::read_to_string(&path).map_err(UnifiedError::from)?;
            let values: Map<String, Value> =
                serde_json::from_str(&text).map_err(UnifiedError::from)?;
            let now = Utc::now();
            for (key, value) in values {
                if let Some(text) = value.as_str() {
                    self.translations.insert(
                        (lang.clone(), key.clone()),
                        Translation {
                            lang: lang.clone(),
                            key,
                            value: text.to_string(),
                            updated_at: now,
                        },
                    );
                }
            }
            // Plural forms live in a sidecar file so a NUL-joined value
            // never leaks into a singular lookup.
            let plurals_path = self.dir.join(format!("{lang}.plurals.json"));
            if let Ok(text) = std::fs::read_to_string(&plurals_path) {
                if let Ok(plurals) = serde_json::from_str::<Map<String, Value>>(&text) {
                    for (key, value) in plurals {
                        if let Some(forms) = value.as_array() {
                            let forms: Vec<String> = forms
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect();
                            if !forms.is_empty() {
                                self.plurals.insert((lang.clone(), key), forms);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Persist one language to its JSON file, atomically enough for this
    /// store: write a temporary file, then rename it over the target.
    pub async fn persist_lang(&self, lang: &str) -> UnifiedResult<()> {
        let mut values: BTreeMap<String, String> = BTreeMap::new();
        for entry in self.translations.iter() {
            let translation = entry.value();
            if translation.lang == lang {
                values.insert(translation.key.clone(), translation.value.clone());
            }
        }
        let text = serde_json::to_string_pretty(&values).map_err(UnifiedError::from)?;
        tokio::fs::create_dir_all(&self.dir)
            .await
            .map_err(UnifiedError::from)?;
        let target = self.file_path(lang);
        let temp = target.with_extension("json.tmp");
        tokio::fs::write(&temp, text.as_bytes())
            .await
            .map_err(UnifiedError::from)?;
        tokio::fs::rename(&temp, &target)
            .await
            .map_err(UnifiedError::from)?;
        // Persist plural forms to the `<lang>.plurals.json` sidecar.
        let mut plural_values: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for entry in self.plurals.iter() {
            let (entry_lang, key) = entry.key();
            if entry_lang == lang {
                plural_values.insert(key.clone(), entry.value().clone());
            }
        }
        let plurals_target = self.dir.join(format!("{lang}.plurals.json"));
        if plural_values.is_empty() {
            let _ = tokio::fs::remove_file(&plurals_target).await;
        } else {
            let plurals_text =
                serde_json::to_string_pretty(&plural_values).map_err(UnifiedError::from)?;
            let plurals_temp = plurals_target.with_extension("json.tmp");
            tokio::fs::write(&plurals_temp, plurals_text.as_bytes())
                .await
                .map_err(UnifiedError::from)?;
            tokio::fs::rename(&plurals_temp, &plurals_target)
                .await
                .map_err(UnifiedError::from)?;
        }
        Ok(())
    }
}

impl Default for I18nState {
    fn default() -> Self {
        Self::new(PathBuf::from("data/i18n"))
    }
}

// ---------------------------------------------------------------------------
// Locale handling, plural rules, currencies, and gettext-style catalogues.
// ---------------------------------------------------------------------------

/// Normalise a locale to BCP 47 shape: language lowercase, region
/// uppercase, hyphen separated (`en_US` / `EN-us` -> `en-US`).
pub fn normalize_locale(input: &str) -> String {
    let mut parts = input.trim().split(['-', '_']);
    let Some(language) = parts.next() else {
        return String::new();
    };
    let language = language.to_lowercase();
    let mut out = language;
    for part in parts {
        if part.is_empty() {
            continue;
        }
        out.push('-');
        out.push_str(&part.to_uppercase());
    }
    out
}

/// Language subtag of a locale (`en-US` -> `en`, `pt_BR` -> `pt`).
pub fn language_from_locale(locale: &str) -> String {
    locale
        .trim()
        .split(['-', '_'])
        .next()
        .unwrap_or("")
        .to_lowercase()
}

/// Parse an `Accept-Language` header into `(language, quality)` pairs,
/// highest quality first. Entries with `q=0` (explicitly refused) are
/// dropped; a missing quality defaults to 1.0.
pub fn parse_accept_language(header: &str) -> Vec<(String, f32)> {
    let mut parsed: Vec<(String, f32)> = Vec::new();
    for part in header.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let mut segments = part.split(';');
        let lang = segments.next().unwrap_or("").trim();
        if lang.is_empty() || lang == "*" {
            continue;
        }
        let mut quality: f32 = 1.0;
        for segment in segments {
            let segment = segment.trim();
            if let Some(value) = segment.strip_prefix("q=") {
                if let Ok(parsed_quality) = value.trim().parse::<f32>() {
                    quality = parsed_quality.clamp(0.0, 1.0);
                }
            }
        }
        if quality > 0.0 {
            parsed.push((normalize_locale(lang), quality));
        }
    }
    // Stable sort: equal qualities keep header order.
    parsed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    parsed
}

/// Best builtin language for an `Accept-Language` header: exact locale
/// match first, then language-only match, then English.
pub fn negotiate_language(header: &str) -> String {
    let available: Vec<String> = builtin_languages()
        .iter()
        .map(|language| language.code.clone())
        .collect();
    for (accepted, _) in parse_accept_language(header) {
        let language = language_from_locale(&accepted);
        if available.iter().any(|code| code == &accepted) {
            return accepted;
        }
        if available.iter().any(|code| code == &language) {
            return language;
        }
    }
    "en".to_string()
}

/// Number of plural forms a locale uses (CLDR families).
pub fn plural_form_count(locale: &str) -> usize {
    match language_from_locale(locale).as_str() {
        "id" | "ms" | "vi" | "zh" | "ja" | "ko" | "th" | "tr" | "lo" | "my" => 1,
        "ar" => 6,
        "ru" | "uk" | "be" | "sr" | "hr" | "bs" | "pl" | "cs" | "sk" | "ro" | "lt" => 3,
        _ => 2,
    }
}

/// Index of the plural form for `count` in `locale`, following CLDR
/// plural rules. Counts are treated by magnitude (negative counts use
/// their absolute value); non-finite counts select the last form.
///
/// Families covered, with the corrections the common one/other shortcut
/// gets wrong:
/// - No plural at all (Indonesian, Malay, Vietnamese, Chinese, Japanese,
///   Korean, Thai, Turkish, Lao, Burmese): always form 0.
/// - French and Portuguese: 0 and 1 both take the singular form.
/// - Slavic (Russian, Ukrainian, Belarusian, Serbian, Croatian,
///   Bosnian), Polish, Czech/Slovak, Romanian, Lithuanian: three forms
///   with distinct remainders; fractional counts take the last form.
/// - Arabic: six forms (zero, one, two, few, many, other).
/// - Anything else: 1 takes form 0, everything else form 1.
pub fn plural_form_index(locale: &str, count: f64) -> usize {
    if !count.is_finite() {
        return plural_form_count(locale).saturating_sub(1);
    }
    let n = count.abs();
    let lang = language_from_locale(locale);
    let lang = lang.as_str();
    if matches!(
        lang,
        "id" | "ms" | "vi" | "zh" | "ja" | "ko" | "th" | "tr" | "lo" | "my"
    ) {
        return 0;
    }
    // Integer part only; the fractional part is handled per family below.
    // `as` saturates on overflow, so very large counts stay well-defined.
    let i = n.trunc() as i64;
    let fractional = n.fract() != 0.0;
    match lang {
        "fr" | "pt" => {
            if !fractional && (i == 0 || i == 1) {
                0
            } else {
                1
            }
        }
        "ru" | "uk" | "be" | "sr" | "hr" | "bs" => {
            if fractional {
                2
            } else if i % 10 == 1 && i % 100 != 11 {
                0
            } else if (2..=4).contains(&(i % 10)) && (i % 100 < 10 || i % 100 >= 20) {
                1
            } else {
                2
            }
        }
        "pl" => {
            if fractional {
                2
            } else if i == 1 {
                0
            } else if (2..=4).contains(&(i % 10)) && (i % 100 < 10 || i % 100 >= 20) {
                1
            } else {
                2
            }
        }
        "cs" | "sk" => {
            if fractional {
                2
            } else if i == 1 {
                0
            } else if (2..=4).contains(&i) {
                1
            } else {
                2
            }
        }
        "ro" => {
            if fractional {
                2
            } else if i == 1 {
                0
            } else if i == 0 || (1..=19).contains(&(i % 100)) {
                1
            } else {
                2
            }
        }
        "lt" => {
            if fractional {
                2
            } else if i % 10 == 1 && i % 100 != 11 {
                0
            } else if (2..=9).contains(&(i % 10)) && (i % 100 < 10 || i % 100 >= 20) {
                1
            } else {
                2
            }
        }
        "ar" => {
            if fractional {
                5
            } else if i == 0 {
                0
            } else if i == 1 {
                1
            } else if i == 2 {
                2
            } else if (3..=10).contains(&(i % 100)) {
                3
            } else if (11..=99).contains(&(i % 100)) {
                4
            } else {
                5
            }
        }
        _ => {
            if n == 1.0 {
                0
            } else {
                1
            }
        }
    }
}

/// Split a NUL-separated plural-forms string (`"one\0other\0many"`).
pub fn split_plural_forms(forms: &str) -> Vec<String> {
    if forms.is_empty() {
        return Vec::new();
    }
    forms.split('\0').map(str::to_string).collect()
}

/// Gettext-style composite key: context and key joined with the standard
/// msgctxt separator (U+0004), so the same source string can have
/// different translations in different contexts.
pub fn contextual_key(key: &str, context: &str) -> String {
    format!("{context}\u{4}{key}")
}

// ---------------------------------------------------------------------------
// Currencies: compact ISO 4217 table (code, English name, symbol, and the
// number of minor-unit digits). This is reference data for display and
// formatting only; exchange rates live elsewhere.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct CurrencyInfo {
    pub code: String,
    pub name: String,
    pub symbol: String,
    pub minor_digits: u8,
}

pub fn builtin_currencies() -> Vec<CurrencyInfo> {
    // (code, name, symbol, minor digits)
    let rows: &[(&str, &str, &str, u8)] = &[
        ("USD", "US Dollar", "$", 2),
        ("EUR", "Euro", "€", 2),
        ("GBP", "Pound Sterling", "£", 2),
        ("JPY", "Japanese Yen", "¥", 0),
        ("IDR", "Indonesian Rupiah", "Rp", 0),
        ("SGD", "Singapore Dollar", "S$", 2),
        ("MYR", "Malaysian Ringgit", "RM", 2),
        ("THB", "Thai Baht", "฿", 2),
        ("PHP", "Philippine Peso", "₱", 2),
        ("VND", "Vietnamese Dong", "₫", 0),
        ("INR", "Indian Rupee", "₹", 2),
        ("CNY", "Chinese Yuan", "¥", 2),
        ("HKD", "Hong Kong Dollar", "HK$", 2),
        ("AUD", "Australian Dollar", "A$", 2),
        ("CAD", "Canadian Dollar", "C$", 2),
        ("CHF", "Swiss Franc", "CHF", 2),
        ("NZD", "New Zealand Dollar", "NZ$", 2),
        ("KRW", "South Korean Won", "₩", 0),
        ("TWD", "New Taiwan Dollar", "NT$", 2),
        ("AED", "UAE Dirham", "د.إ", 2),
        ("SAR", "Saudi Riyal", "﷼", 2),
        ("TRY", "Turkish Lira", "₺", 2),
        ("BRL", "Brazilian Real", "R$", 2),
        ("MXN", "Mexican Peso", "MX$", 2),
        ("ZAR", "South African Rand", "R", 2),
        ("PLN", "Polish Zloty", "zł", 2),
        ("CZK", "Czech Koruna", "Kč", 2),
        ("SEK", "Swedish Krona", "kr", 2),
        ("NOK", "Norwegian Krone", "kr", 2),
        ("DKK", "Danish Krone", "kr", 2),
        ("BHD", "Bahraini Dinar", ".د.ب", 3),
        ("KWD", "Kuwaiti Dinar", "د.ك", 3),
        ("OMR", "Omani Rial", "﷼", 3),
        ("ILS", "Israeli New Shekel", "₪", 2),
        ("UAH", "Ukrainian Hryvnia", "₴", 2),
    ];
    rows.iter()
        .map(|(code, name, symbol, minor_digits)| CurrencyInfo {
            code: (*code).to_string(),
            name: (*name).to_string(),
            symbol: (*symbol).to_string(),
            minor_digits: *minor_digits,
        })
        .collect()
}

pub fn currency_info(code: &str) -> Option<CurrencyInfo> {
    let code = code.trim().to_uppercase();
    builtin_currencies()
        .into_iter()
        .find(|currency| currency.code == code)
}

/// Format an amount with the currency symbol and the currency's minor
/// digits (`Rp15000`, `$12.50`). Returns `None` for an unknown code.
pub fn format_currency(code: &str, amount: f64) -> Option<String> {
    let info = currency_info(code)?;
    Some(format!(
        "{}{:.*}",
        info.symbol, info.minor_digits as usize, amount
    ))
}

// ---------------------------------------------------------------------------
// Minimal gettext (.po) catalogue parsing (no extra dependency).
//
// Supported: `msgid`, `msgid_plural`, `msgctxt`, `msgstr`, `msgstr[n]`,
// continuation lines, `\n`/`\t`/`\"`/`\\` escapes, and `#, fuzzy`
// markers (fuzzy entries are skipped). The header entry (`msgid ""`)
// is used only for its `Plural-Forms: nplurals=N` value. This is a
// pragmatic subset for importing translator-produced catalogues, not a
// full .po implementation: obsolete (`#~`) entries and previous-msgid
// (`#|`) comments are ignored.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoEntry {
    pub msgid: String,
    pub msgid_plural: Option<String>,
    pub msgctxt: Option<String>,
    pub msgstr: String,
    pub msgstr_plural: Vec<String>,
    pub fuzzy: bool,
}

fn unescape_po_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Extract the quoted string from a line such as `msgid "Hello"` or a
/// bare continuation line `"more"`. Returns `None` when the line holds
/// no quoted string.
fn po_quoted(line: &str) -> Option<String> {
    let start = line.find('"')?;
    let end = line.rfind('"')?;
    if end <= start {
        return None;
    }
    Some(unescape_po_string(&line[start + 1..end]))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoField {
    Msgid,
    MsgidPlural,
    Msgctxt,
    Msgstr,
    MsgstrPlural(usize),
    None,
}

pub fn parse_po_catalog(content: &str) -> Vec<PoEntry> {
    let mut entries: Vec<PoEntry> = Vec::new();
    let mut current = PoEntry {
        msgid: String::new(),
        msgid_plural: None,
        msgctxt: None,
        msgstr: String::new(),
        msgstr_plural: Vec::new(),
        fuzzy: false,
    };
    let mut has_entry = false;
    let mut field = PoField::None;
    let mut flush = |current: &mut PoEntry, has_entry: &mut bool, entries: &mut Vec<PoEntry>| {
        if *has_entry {
            entries.push(std::mem::replace(
                current,
                PoEntry {
                    msgid: String::new(),
                    msgid_plural: None,
                    msgctxt: None,
                    msgstr: String::new(),
                    msgstr_plural: Vec::new(),
                    fuzzy: false,
                },
            ));
            *has_entry = false;
        }
    };
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            flush(&mut current, &mut has_entry, &mut entries);
            field = PoField::None;
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            if comment.starts_with(',') && comment.contains("fuzzy") {
                current.fuzzy = true;
            }
            continue;
        }
        if line.starts_with('"') {
            if let Some(text) = po_quoted(line) {
                match field {
                    PoField::Msgid => current.msgid.push_str(&text),
                    PoField::MsgidPlural => {
                        current.msgid_plural.get_or_insert_with(String::new).push_str(&text);
                    }
                    PoField::Msgctxt => {
                        current.msgctxt.get_or_insert_with(String::new).push_str(&text);
                    }
                    PoField::Msgstr => current.msgstr.push_str(&text),
                    PoField::MsgstrPlural(index) => {
                        while current.msgstr_plural.len() <= index {
                            current.msgstr_plural.push(String::new());
                        }
                        current.msgstr_plural[index].push_str(&text);
                    }
                    PoField::None => {}
                }
            }
            continue;
        }
        if line.starts_with("msgid_plural") {
            field = PoField::MsgidPlural;
            current.msgid_plural = po_quoted(line);
            has_entry = true;
        } else if line.starts_with("msgid") {
            // Flush only when a previous entry actually started: a
            // `msgctxt` line precedes its `msgid` in standard files, so
            // flushing on every `msgid` would split the context off.
            if !current.msgid.is_empty() {
                flush(&mut current, &mut has_entry, &mut entries);
            }
            field = PoField::Msgid;
            current.msgid = po_quoted(line).unwrap_or_default();
            has_entry = true;
        } else if line.starts_with("msgctxt") {
            field = PoField::Msgctxt;
            current.msgctxt = po_quoted(line);
            has_entry = true;
        } else if line.starts_with("msgstr[") {
            let index = line
                .strip_prefix("msgstr[")
                .and_then(|rest| rest.split(']').next())
                .and_then(|digits| digits.parse::<usize>().ok())
                .unwrap_or(0);
            field = PoField::MsgstrPlural(index);
            while current.msgstr_plural.len() <= index {
                current.msgstr_plural.push(String::new());
            }
            current.msgstr_plural[index] = po_quoted(line).unwrap_or_default();
            has_entry = true;
        } else if line.starts_with("msgstr") {
            field = PoField::Msgstr;
            current.msgstr = po_quoted(line).unwrap_or_default();
            has_entry = true;
        }
    }
    flush(&mut current, &mut has_entry, &mut entries);
    entries
}

/// `nplurals` from a header entry's `Plural-Forms` line, if present.
fn po_header_nplurals(header_msgstr: &str) -> Option<usize> {
    for line in header_msgstr.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Plural-Forms:") {
            for part in rest.split(';') {
                let part = part.trim();
                if let Some(value) = part.strip_prefix("nplurals=") {
                    return value.trim().parse::<usize>().ok();
                }
            }
        }
    }
    None
}

#[derive(Debug, Clone, Serialize)]
pub struct PoLoadResult {
    pub loaded: usize,
    pub plural_entries: usize,
    pub skipped_fuzzy: usize,
    pub header_nplurals: Option<usize>,
}

impl I18nState {
    /// Load a gettext `.po` catalogue into the store. Fuzzy and empty
    /// entries are skipped; a plural entry stores its forms (singular
    /// first) in the plurals map and its singular form as the plain
    /// translation. Returns per-catalogue counts for the caller.
    pub fn load_po_catalog(&self, lang: &str, content: &str) -> PoLoadResult {
        let entries = parse_po_catalog(content);
        let mut result = PoLoadResult {
            loaded: 0,
            plural_entries: 0,
            skipped_fuzzy: 0,
            header_nplurals: None,
        };
        let now = Utc::now();
        for entry in entries {
            if entry.msgid.is_empty() {
                result.header_nplurals = po_header_nplurals(&entry.msgstr);
                continue;
            }
            if entry.fuzzy {
                result.skipped_fuzzy += 1;
                continue;
            }
            let key = match &entry.msgctxt {
                Some(context) => contextual_key(&entry.msgid, context),
                None => entry.msgid.clone(),
            };
            let has_plural = entry.msgid_plural.is_some() && !entry.msgstr_plural.is_empty();
            if has_plural {
                let mut forms = vec![entry.msgstr.clone()];
                forms.extend(entry.msgstr_plural.iter().cloned());
                self.plurals
                    .insert((lang.to_string(), key.clone()), forms);
                result.plural_entries += 1;
            }
            if !entry.msgstr.is_empty() {
                self.translations.insert(
                    (lang.to_string(), key.clone()),
                    Translation {
                        lang: lang.to_string(),
                        key,
                        value: entry.msgstr,
                        updated_at: now,
                    },
                );
                result.loaded += 1;
            }
        }
        result
    }

    /// Context-aware lookup: tries `context + key` first, then the bare
    /// key. Returns `None` when neither exists.
    pub fn translate_with_context(
        &self,
        lang: &str,
        key: &str,
        context: Option<&str>,
    ) -> Option<String> {
        if let Some(context) = context.filter(|value| !value.is_empty()) {
            let composite = contextual_key(key, context);
            if let Some(entry) = self.translations.get(&(lang.to_string(), composite)) {
                return Some(entry.value().value.clone());
            }
        }
        self.translations
            .get(&(lang.to_string(), key.to_string()))
            .map(|entry| entry.value().value.clone())
    }

    /// Plural-aware translation. Returns `(text, form_index, translated)`:
    /// the stored plural form when one exists for the count, the stored
    /// singular translation when the count selects form 0, and otherwise
    /// the caller-supplied singular/plural fallback.
    pub fn translate_plural(
        &self,
        lang: &str,
        singular: &str,
        plural: &str,
        count: f64,
        context: Option<&str>,
    ) -> (String, usize, bool) {
        let index = plural_form_index(lang, count);
        let key = match context.filter(|value| !value.is_empty()) {
            Some(context) => contextual_key(singular, context),
            None => singular.to_string(),
        };
        if let Some(forms) = self.plurals.get(&(lang.to_string(), key.clone())) {
            let forms = forms.value();
            if index < forms.len() {
                return (forms[index].clone(), index, true);
            }
            if index == 0 {
                return (forms[0].clone(), index, true);
            }
            return (plural.to_string(), index, true);
        }
        if index == 0 {
            if let Some(value) = self.translate_with_context(lang, singular, context) {
                return (value, index, true);
            }
            return (singular.to_string(), index, false);
        }
        (plural.to_string(), index, false)
    }
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

fn validate_lang(lang: &str) -> Result<String, UnifiedError> {
    let lang = lang.trim().to_lowercase();
    if !is_builtin_language(&lang) {
        return Err(UnifiedError::bad_request(format!(
            "unknown language code \"{lang}\""
        )));
    }
    Ok(lang)
}

fn validate_key(key: &str) -> Result<String, UnifiedError> {
    let key = key.trim();
    let pattern =
        Regex::new(r"^[a-z0-9_]+(\.[a-z0-9_]+)*$").expect("static regex is valid");
    if !pattern.is_match(key) {
        return Err(UnifiedError::bad_request(
            "key must be dot-separated lowercase segments, for example \"nav.dashboard\"",
        ));
    }
    if key.chars().count() > 200 {
        return Err(UnifiedError::bad_request(
            "key must be at most 200 characters",
        ));
    }
    Ok(key.to_string())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_languages() -> HttpResponse {
    api_json::success_response(json!({ "languages": builtin_languages() }), 200)
}

async fn list_translations(
    state: web::Data<I18nState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let Some(lang_raw) = params.filters.get("lang").cloned() else {
        return api_json::error_response(
            &UnifiedError::bad_request("query parameter \"lang\" is required"),
            false,
        );
    };
    let lang = match validate_lang(&lang_raw) {
        Ok(lang) => lang,
        Err(error) => return api_json::error_response(&error, false),
    };
    let search = params.search.clone();
    let mut items: Vec<Translation> = state
        .translations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|translation| translation.lang == lang)
        .filter(|translation| {
            if let Some(needle) = &search {
                let needle = needle.to_lowercase();
                return translation.key.to_lowercase().contains(&needle)
                    || translation.value.to_lowercase().contains(&needle);
            }
            true
        })
        .collect();
    let sort = params.allowed_sort(&["key", "updated_at"], "key");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "updated_at" => a.updated_at.cmp(&b.updated_at),
            _ => a.key.cmp(&b.key),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<Translation> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

async fn upsert_translation(
    state: web::Data<I18nState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<UpsertTranslationInput>,
) -> HttpResponse {
    let input = input.into_inner();
    let outcome = (|| -> Result<Translation, UnifiedError> {
        let lang = validate_lang(&input.lang)?;
        let key = validate_key(&input.key)?;
        let value = input.value.trim();
        if value.is_empty() {
            return Err(UnifiedError::bad_request("value must not be empty"));
        }
        if value.chars().count() > 10_000 {
            return Err(UnifiedError::bad_request(
                "value must be at most 10000 characters",
            ));
        }
        let translation = Translation {
            lang: lang.clone(),
            key: key.clone(),
            value: value.to_string(),
            updated_at: Utc::now(),
        };
        state
            .translations
            .insert((lang, key), translation.clone());
        Ok(translation)
    })();
    let translation = match outcome {
        Ok(translation) => translation,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = state.persist_lang(&translation.lang).await {
        return api_json::error_response(&error, ctx.is_debug());
    }
    ctx.hooks.emit(
        "i18n.translation.updated",
        json!({
            "lang": translation.lang,
            "key": translation.key,
            "value": translation.value,
        }),
    );
    api_json::success_response(translation, 200)
}

async fn translate(
    state: web::Data<I18nState>,
    input: web::Json<TranslateInput>,
) -> HttpResponse {
    let input = input.into_inner();
    let lang = match validate_lang(&input.lang) {
        Ok(lang) => lang,
        Err(error) => return api_json::error_response(&error, false),
    };
    let key = match validate_key(&input.key) {
        Ok(key) => key,
        Err(error) => return api_json::error_response(&error, false),
    };
    if let Some(value) = state.translate_with_context(
        &lang,
        &key,
        input.context.as_deref(),
    ) {
        return api_json::success_response(
            json!({
                "lang": lang,
                "key": key,
                "value": value,
                "translated": true,
            }),
            200,
        );
    }
    match input.fallback {
        Some(fallback) => api_json::success_response(
            json!({
                "lang": lang,
                "key": key,
                "value": fallback,
                "translated": false,
            }),
            200,
        ),
        None => api_json::error_response(
            &UnifiedError::not_found(format!(
                "no translation for key \"{key}\" in language \"{lang}\""
            )),
            false,
        ),
    }
}

async fn translate_plural_handler(
    state: web::Data<I18nState>,
    input: web::Json<TranslatePluralInput>,
) -> HttpResponse {
    let input = input.into_inner();
    let lang = match validate_lang(&input.lang) {
        Ok(lang) => lang,
        Err(error) => return api_json::error_response(&error, false),
    };
    let singular = input.singular.trim();
    let plural = input.plural.trim();
    if singular.is_empty() || plural.is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("singular and plural must not be empty"),
            false,
        );
    }
    if singular.chars().count() > 10_000 || plural.chars().count() > 10_000 {
        return api_json::error_response(
            &UnifiedError::bad_request("singular and plural must be at most 10000 characters"),
            false,
        );
    }
    let context = input.context.as_deref().map(str::trim).filter(|value| !value.is_empty());
    let (value, index, translated) =
        state.translate_plural(&lang, singular, plural, input.count, context);
    api_json::success_response(
        json!({
            "lang": lang,
            "singular": singular,
            "plural": plural,
            "count": input.count,
            "index": index,
            "form_count": plural_form_count(&lang),
            "value": value,
            "translated": translated,
        }),
        200,
    )
}

async fn load_po_catalog_handler(
    state: web::Data<I18nState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<LoadPoCatalogInput>,
) -> HttpResponse {
    let input = input.into_inner();
    let lang = match validate_lang(&input.lang) {
        Ok(lang) => lang,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    if input.content.is_empty() || input.content.chars().count() > 2_000_000 {
        return api_json::error_response(
            &UnifiedError::bad_request("content must be a non-empty .po catalogue of at most 2000000 characters"),
            ctx.is_debug(),
        );
    }
    let result = state.load_po_catalog(&lang, &input.content);
    if let Err(error) = state.persist_lang(&lang).await {
        return api_json::error_response(&error, ctx.is_debug());
    }
    ctx.hooks.emit(
        "i18n.catalog.loaded",
        json!({
            "lang": lang,
            "loaded": result.loaded,
            "plural_entries": result.plural_entries,
            "skipped_fuzzy": result.skipped_fuzzy,
        }),
    );
    api_json::success_response(json!({ "lang": lang, "result": result }), 200)
}

async fn negotiate_language_handler(req: actix_web::HttpRequest) -> HttpResponse {
    let header = req
        .headers()
        .get("accept-language")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let matched = negotiate_language(header);
    api_json::success_response(
        json!({
            "requested": header,
            "matched": matched,
            "languages": builtin_languages(),
        }),
        200,
    )
}

async fn list_currencies() -> HttpResponse {
    api_json::success_response(json!({ "currencies": builtin_currencies() }), 200)
}

async fn get_currency(path: web::Path<String>) -> HttpResponse {
    let code = path.into_inner();
    match currency_info(&code) {
        Some(info) => api_json::success_response(json!({ "currency": info }), 200),
        None => api_json::error_response(
            &UnifiedError::not_found(format!("unknown currency code \"{code}\"")),
            false,
        ),
    }
}

async fn delete_translation(
    state: web::Data<I18nState>,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (lang_raw, key_raw) = path.into_inner();
    let lang = match validate_lang(&lang_raw) {
        Ok(lang) => lang,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    let key = match validate_key(&key_raw) {
        Ok(key) => key,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    match state.translations.remove(&(lang.clone(), key.clone())) {
        Some((_, translation)) => {
            if let Err(error) = state.persist_lang(&lang).await {
                return api_json::error_response(&error, ctx.is_debug());
            }
            ctx.hooks.emit(
                "i18n.translation.deleted",
                json!({ "lang": translation.lang, "key": translation.key }),
            );
            api_json::success_response(
                json!({ "lang": translation.lang, "key": translation.key, "deleted": true }),
                200,
            )
        }
        None => api_json::error_response(
            &UnifiedError::not_found(format!(
                "no translation for key \"{key}\" in language \"{lang}\""
            )),
            ctx.is_debug(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct I18nModule {
    state: I18nState,
}

impl I18nModule {
    pub fn new() -> Self {
        Self {
            state: I18nState::default(),
        }
    }
}

impl Default for I18nModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for I18nModule {
    fn id(&self) -> &'static str {
        "i18n"
    }

    fn name(&self) -> &'static str {
        "Translations"
    }

    fn description(&self) -> &'static str {
        "Language catalogue and key/value translations persisted to JSON files."
    }

    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        // Load persisted overrides; a failure here must not stop startup,
        // but a corrupt file is reported instead of silently ignored.
        self.state.load_from_disk()
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/i18n/languages", owner: "i18n", description: "List languages" },
            RouteInfo { method: "GET", path: "/api/v1/i18n/languages/negotiate", owner: "i18n", description: "Negotiate language handler" },
            RouteInfo { method: "GET", path: "/api/v1/i18n/currencies", owner: "i18n", description: "List currencies" },
            RouteInfo { method: "GET", path: "/api/v1/i18n/currencies/{code}", owner: "i18n", description: "Get currency" },
            RouteInfo { method: "GET", path: "/api/v1/i18n/translations", owner: "i18n", description: "List translations" },
            RouteInfo { method: "POST", path: "/api/v1/i18n/translations", owner: "i18n", description: "Upsert translation" },
            RouteInfo { method: "POST", path: "/api/v1/i18n/translate", owner: "i18n", description: "Translate" },
            RouteInfo { method: "POST", path: "/api/v1/i18n/translate-plural", owner: "i18n", description: "Translate plural handler" },
            RouteInfo { method: "POST", path: "/api/v1/i18n/catalog/po", owner: "i18n", description: "Load po catalog handler" },
            RouteInfo { method: "DELETE", path: "/api/v1/i18n/translations/{lang}/{key}", owner: "i18n", description: "Delete translation" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/i18n")
                .app_data(state)
                .route("/languages", web::get().to(list_languages))
                .route("/languages/negotiate", web::get().to(negotiate_language_handler))
                .route("/currencies", web::get().to(list_currencies))
                .route("/currencies/{code}", web::get().to(get_currency))
                .route("/translations", web::get().to(list_translations))
                .route("/translations", web::post().to(upsert_translation))
                .route("/translate", web::post().to(translate))
                .route("/translate-plural", web::post().to(translate_plural_handler))
                .route("/catalog/po", web::post().to(load_po_catalog_handler))
                .route(
                    "/translations/{lang}/{key}",
                    web::delete().to(delete_translation),
                ),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(I18nModule::new()));
}
