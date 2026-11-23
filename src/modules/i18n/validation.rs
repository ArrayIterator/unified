// i18n -- Validation helpers.

use unified_core::error::UnifiedError;
use regex::Regex;
use std::sync::OnceLock;

use super::languages::*;

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// The compiled dot-separated key pattern, built once and shared by
/// every `validate_key` call instead of recompiling the regex per call.
static KEY_PATTERN: OnceLock<Regex> = OnceLock::new();

fn key_pattern() -> &'static Regex {
    KEY_PATTERN.get_or_init(|| {
        Regex::new(r"^[a-z0-9_]+(\.[a-z0-9_]+)*$").expect("static regex is valid")
    })
}

pub(crate) fn validate_lang(lang: &str) -> Result<String, UnifiedError> {
    let lang = lang.trim().to_lowercase();
    if !is_builtin_language(&lang) {
        return Err(UnifiedError::bad_request(format!(
            "unknown language code \"{lang}\""
        )));
    }
    Ok(lang)
}

pub(crate) fn validate_key(key: &str) -> Result<String, UnifiedError> {
    let key = key.trim();
    if !key_pattern().is_match(key) {
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

/// Validate a lookup key for the translate endpoint. Gettext msgids
/// are free-form source sentences (`"Hello"`), not dot-separated
/// identifiers, so only length is enforced: a trimmed, non-empty key
/// of at most 10000 characters. Use `validate_key` for admin-defined
/// keys (upsert/delete), which must stay dot-separated.
pub(crate) fn validate_lookup_key(key: &str) -> Result<String, UnifiedError> {
    let key = key.trim();
    if key.is_empty() {
        return Err(UnifiedError::bad_request("key must not be empty"));
    }
    if key.chars().count() > 10_000 {
        return Err(UnifiedError::bad_request(
            "key must be at most 10000 characters",
        ));
    }
    Ok(key.to_string())
}

// Tests for the validation helpers in this file.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_key_keeps_dot_separated_shape() {
        assert_eq!(validate_key("nav.dashboard").unwrap(), "nav.dashboard");
        assert_eq!(validate_key("  common.save  ").unwrap(), "common.save");
        assert!(validate_key("Hello").is_err());
        assert!(validate_key("Hello, world!").is_err());
        assert!(validate_key("").is_err());
        assert!(validate_key("nav..dashboard").is_err());
    }

    #[test]
    fn validate_lookup_key_accepts_free_form_msgids() {
        assert_eq!(validate_lookup_key("Hello").unwrap(), "Hello");
        assert_eq!(
            validate_lookup_key("Hello, world!").unwrap(),
            "Hello, world!"
        );
        assert_eq!(
            validate_lookup_key("  Hello, world!  ").unwrap(),
            "Hello, world!"
        );
        // Dot-separated admin keys are valid lookups too.
        assert_eq!(
            validate_lookup_key("nav.dashboard").unwrap(),
            "nav.dashboard"
        );
        assert!(validate_lookup_key("").is_err());
        assert!(validate_lookup_key("   ").is_err());
        assert!(validate_lookup_key(&"x".repeat(10_001)).is_err());
        assert_eq!(
            validate_lookup_key(&"x".repeat(10_000)).unwrap().len(),
            10_000
        );
    }

    #[test]
    fn key_pattern_is_compiled_once() {
        assert!(std::ptr::eq(key_pattern(), key_pattern()));
        assert!(key_pattern().is_match("nav.dashboard"));
        assert!(!key_pattern().is_match("Hello"));
    }
}
