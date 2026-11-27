// i18n -- Built-in language catalogue.

use serde::Serialize;

// Translations / i18n Manager module.
//
// Built-in language catalogue (ISO 639-1 codes), a key/value translation
// store held in memory and persisted to `data/i18n/<lang>.json` files,
// dynamic translation overrides, and a translate lookup that falls back
// to a caller-supplied string when no translation exists.

// Language catalogue

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
        Language {
            code: "hi".to_string(),
            name: "Hindi".to_string(),
            native_name: "हिन्दी".to_string(),
            rtl: false,
        },
        Language {
            code: "it".to_string(),
            name: "Italian".to_string(),
            native_name: "Italiano".to_string(),
            rtl: false,
        },
        Language {
            code: "tr".to_string(),
            name: "Turkish".to_string(),
            native_name: "Türkçe".to_string(),
            rtl: false,
        },
        Language {
            code: "vi".to_string(),
            name: "Vietnamese".to_string(),
            native_name: "Tiếng Việt".to_string(),
            rtl: false,
        },
        Language {
            code: "th".to_string(),
            name: "Thai".to_string(),
            native_name: "ไทย".to_string(),
            rtl: false,
        },
        Language {
            code: "ms".to_string(),
            name: "Malay".to_string(),
            native_name: "Bahasa Melayu".to_string(),
            rtl: false,
        },
        Language {
            code: "uk".to_string(),
            name: "Ukrainian".to_string(),
            native_name: "Українська".to_string(),
            rtl: false,
        },
        Language {
            code: "pl".to_string(),
            name: "Polish".to_string(),
            native_name: "Polski".to_string(),
            rtl: false,
        },
        Language {
            code: "sv".to_string(),
            name: "Swedish".to_string(),
            native_name: "Svenska".to_string(),
            rtl: false,
        },
        Language {
            code: "da".to_string(),
            name: "Danish".to_string(),
            native_name: "Dansk".to_string(),
            rtl: false,
        },
        Language {
            code: "fi".to_string(),
            name: "Finnish".to_string(),
            native_name: "Suomi".to_string(),
            rtl: false,
        },
        Language {
            code: "nb".to_string(),
            name: "Norwegian Bokmål".to_string(),
            native_name: "Norsk Bokmål".to_string(),
            rtl: false,
        },
        Language {
            code: "he".to_string(),
            name: "Hebrew".to_string(),
            native_name: "עברית".to_string(),
            rtl: true,
        },
        Language {
            code: "fa".to_string(),
            name: "Persian".to_string(),
            native_name: "فارسی".to_string(),
            rtl: true,
        },
        Language {
            code: "ur".to_string(),
            name: "Urdu".to_string(),
            native_name: "اردو".to_string(),
            rtl: true,
        },
        Language {
            code: "bn".to_string(),
            name: "Bengali".to_string(),
            native_name: "বাংলা".to_string(),
            rtl: false,
        },
        Language {
            code: "ta".to_string(),
            name: "Tamil".to_string(),
            native_name: "தமிழ்".to_string(),
            rtl: false,
        },
        Language {
            code: "ro".to_string(),
            name: "Romanian".to_string(),
            native_name: "Română".to_string(),
            rtl: false,
        },
        Language {
            code: "hu".to_string(),
            name: "Hungarian".to_string(),
            native_name: "Magyar".to_string(),
            rtl: false,
        },
        Language {
            code: "cs".to_string(),
            name: "Czech".to_string(),
            native_name: "Čeština".to_string(),
            rtl: false,
        },
        Language {
            code: "el".to_string(),
            name: "Greek".to_string(),
            native_name: "Ελληνικά".to_string(),
            rtl: false,
        },
        Language {
            code: "sr".to_string(),
            name: "Serbian".to_string(),
            native_name: "Српски".to_string(),
            rtl: false,
        },
        Language {
            code: "hr".to_string(),
            name: "Croatian".to_string(),
            native_name: "Hrvatski".to_string(),
            rtl: false,
        },
        Language {
            code: "bg".to_string(),
            name: "Bulgarian".to_string(),
            native_name: "Български".to_string(),
            rtl: false,
        },
        Language {
            code: "ca".to_string(),
            name: "Catalan".to_string(),
            native_name: "Català".to_string(),
            rtl: false,
        },
        Language {
            code: "sk".to_string(),
            name: "Slovak".to_string(),
            native_name: "Slovenčina".to_string(),
            rtl: false,
        },
        Language {
            code: "sl".to_string(),
            name: "Slovenian".to_string(),
            native_name: "Slovenščina".to_string(),
            rtl: false,
        },
        Language {
            code: "lt".to_string(),
            name: "Lithuanian".to_string(),
            native_name: "Lietuvių".to_string(),
            rtl: false,
        },
        Language {
            code: "lv".to_string(),
            name: "Latvian".to_string(),
            native_name: "Latviešu".to_string(),
            rtl: false,
        },
        Language {
            code: "et".to_string(),
            name: "Estonian".to_string(),
            native_name: "Eesti".to_string(),
            rtl: false,
        },
    ]
}

pub fn is_builtin_language(code: &str) -> bool {
    builtin_languages()
        .iter()
        .any(|language| language.code == code)
}

/// One built-in language by ISO 639-1 code (lowercase, trimmed),
/// or `None` for an unknown code.
pub fn language_info(code: &str) -> Option<Language> {
    let code = code.trim().to_lowercase();
    builtin_languages()
        .into_iter()
        .find(|language| language.code == code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_has_unique_sorted_codes() {
        let languages = builtin_languages();
        assert_eq!(languages.len(), 42);
        let mut codes: Vec<&str> = languages.iter().map(|language| language.code.as_str()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), languages.len());
        // The original twelve are all still present.
        for code in ["en", "id", "ar", "zh", "es", "fr", "de", "pt", "ja", "ko", "nl", "ru"] {
            assert!(is_builtin_language(code), "{code} must stay in the catalogue");
        }
        // RTL flags are set for the right-to-left scripts only.
        assert!(language_info("he").unwrap().rtl);
        assert!(language_info("fa").unwrap().rtl);
        assert!(language_info("ur").unwrap().rtl);
        assert!(!language_info("en").unwrap().rtl);
        assert_eq!(language_info("xx"), None);
    }
}
