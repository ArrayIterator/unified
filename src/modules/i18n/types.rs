// i18n -- Translation and request types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// Types

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
