// i18n -- Built-in language catalogue.

use serde::Serialize;

// Translations / i18n Manager module.
//
// Built-in language catalogue (ISO 639-1 codes), a key/value translation
// store held in memory and persisted to `data/i18n/<lang>.json` files,
// dynamic translation overrides, and a translate lookup that falls back
// to a caller-supplied string when no translation exists.

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
    builtin_languages()
        .iter()
        .any(|language| language.code == code)
}
