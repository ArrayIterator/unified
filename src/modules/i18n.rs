// Translations / i18n Manager module.
//
// Built-in language catalogue (ISO 639-1 codes), a key/value translation
// store held in memory and persisted to `data/i18n/<lang>.json` files,
// dynamic translation overrides, and a translate lookup that falls back
// to a caller-supplied string when no translation exists.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::json as api_json;
use crate::core::modules::{Module, ModuleRegistry};
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
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct I18nState {
    /// Keyed by (language code, translation key).
    pub translations: Arc<DashMap<(String, String), Translation>>,
    pub dir: PathBuf,
}

impl I18nState {
    pub fn new(dir: PathBuf) -> Self {
        let state = Self {
            translations: Arc::new(DashMap::new()),
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
        Ok(())
    }
}

impl Default for I18nState {
    fn default() -> Self {
        Self::new(PathBuf::from("data/i18n"))
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
    if let Some(translation) = state.translations.get(&(lang.clone(), key.clone())) {
        return api_json::success_response(
            json!({
                "lang": lang,
                "key": key,
                "value": translation.value().value,
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

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/i18n")
                .app_data(state)
                .route("/languages", web::get().to(list_languages))
                .route("/translations", web::get().to(list_translations))
                .route("/translations", web::post().to(upsert_translation))
                .route("/translate", web::post().to(translate))
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
