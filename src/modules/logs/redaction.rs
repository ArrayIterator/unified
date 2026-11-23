// logs — redaction.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{http::header, web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, OnceLock, RwLock,
};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::service::*;
use super::tracing::*;
use super::common::*;
use super::handlers::*;
use super::hooks::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Constants and redaction
// ---------------------------------------------------------------------------

/// Valid log levels, lowest to highest severity.
pub const LOG_LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error"];

/// Valid log entry kinds.
pub const LOG_KINDS: &[&str] = &["application", "access", "error"];

/// Default number of days log entries are kept.
pub const DEFAULT_RETENTION_DAYS: u32 = 30;

/// Default maximum number of entries held in the ring buffer.
pub const DEFAULT_MAX_ENTRIES: usize = 10_000;

/// Context keys that name credentials. A key is sensitive when one
/// of these keywords appears as a whole token (or run of tokens) in
/// the key, splitting on `_`, `-` and lower-to-upper case
/// transitions, case-insensitively. Token matching — not raw
/// substring matching — so `author` and `author_name` stay readable
/// while `password`, `PasswordHash` and `X-Auth-Token` are redacted.
pub(crate) const SENSITIVE_KEYS: &[&str] = &[
    "password",
    "passwd",
    "token",
    "secret",
    "api_key",
    "apikey",
    "api-key",
    "authorization",
    "auth",
    "cookie",
    "session",
    "private_key",
    "privatekey",
    "bearer",
    "credential",
    "access_key",
    "accesskey",
    "refresh_token",
    "client_secret",
];

/// Split a key into lowercase tokens on `_`, `-`, spaces and other
/// non-alphanumeric separators, and on lower-to-upper case
/// transitions (`PasswordHash` becomes `password`, `hash`).
fn split_key_tokens(key: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut previous: Option<char> = None;
    for ch in key.chars() {
        if ch.is_alphanumeric() {
            if ch.is_uppercase()
                && previous.is_some_and(|prev| prev.is_lowercase() || prev.is_ascii_digit())
                && !current.is_empty()
            {
                tokens.push(std::mem::take(&mut current));
            }
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
        previous = Some(ch);
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

pub(crate) fn is_sensitive_key(key: &str) -> bool {
    let tokens = split_key_tokens(key);
    if tokens.is_empty() {
        return false;
    }
    SENSITIVE_KEYS.iter().any(|keyword| {
        let wanted = split_key_tokens(keyword);
        !wanted.is_empty()
            && tokens
                .windows(wanted.len())
                .any(|window| window == wanted.as_slice())
    })
}

/// Recursively replace credential values in a JSON context object.
pub(crate) fn redact_value(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, item)| {
                    if is_sensitive_key(&key) {
                        (key, Value::String("[redacted]".to_string()))
                    } else {
                        (key, redact_value(item))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.into_iter().map(redact_value).collect())
        }
        other => other,
    }
}

/// Keywords scrubbed out of message text, kept in sync with
/// `SENSITIVE_KEYS` so a secret that is redacted in context keys is
/// also scrubbed from free-form messages. Each entry is a regex
/// fragment for one keyword (`[_-]?` covers `api_key`, `api-key`
/// and `apikey` spellings).
pub(crate) const MESSAGE_SCRUB_KEYWORDS: &[&str] = &[
    "password",
    "passwd",
    "token",
    "secret",
    "api[_-]?key",
    "authorization",
    "bearer",
    "session",
    "cookie",
    "client[_-]?secret",
    "credential",
    "refresh[_-]?token",
    "private[_-]?key",
    "access[_-]?key",
];

pub(crate) fn secret_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        let alternation = MESSAGE_SCRUB_KEYWORDS.join("|");
        Regex::new(&format!(r#"(?i)({alternation})\s*[:=]\s*\S+"#))
            .expect("secret scrub pattern is valid")
    })
}

/// Scrub `password=...`, `token: ...` style fragments out of a message.
pub(crate) fn scrub_message(message: &str) -> String {
    secret_pattern()
        .replace_all(message, "$1[redacted]")
        .to_string()
}

pub(crate) fn normalize_level(level: &str) -> Option<String> {
    let level = level.trim().to_lowercase();
    if LOG_LEVELS.contains(&level.as_str()) {
        Some(level)
    } else {
        None
    }
}

pub(crate) fn level_rank(level: &str) -> u8 {
    match level {
        "trace" => 0,
        "debug" => 1,
        "info" => 2,
        "warn" => 3,
        "error" => 4,
        _ => 2,
    }
}

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn sensitive_keys_match_case_insensitively_by_token() {
        assert!(is_sensitive_key("password"));
        assert!(is_sensitive_key("PasswordHash"));
        assert!(is_sensitive_key("api_key"));
        assert!(is_sensitive_key("X-Auth-Token"));
        assert!(is_sensitive_key("client_secret"));
        assert!(!is_sensitive_key("username"));
        assert!(!is_sensitive_key("email"));
        assert!(!is_sensitive_key("request_id"));
    }

    #[test]
    fn sensitive_keys_do_not_match_inside_other_words() {
        // Token matching, not substrings: "author" contains "auth"
        // but is not a credential key.
        assert!(!is_sensitive_key("author"));
        assert!(!is_sensitive_key("author_name"));
        assert!(!is_sensitive_key("authority"));
        assert!(!is_sensitive_key("tokenizer"));
    }

    #[test]
    fn redact_value_replaces_credentials_recursively() {
        let redacted = redact_value(json!({
            "username": "ada",
            "password": "hunter2",
            "nested": { "api_key": "abc", "safe": 1 },
            "list": [{ "token": "t", "other": "kept" }]
        }));
        assert_eq!(redacted["username"], "ada");
        assert_eq!(redacted["password"], "[redacted]");
        assert_eq!(redacted["nested"]["api_key"], "[redacted]");
        assert_eq!(redacted["nested"]["safe"], 1);
        assert_eq!(redacted["list"][0]["token"], "[redacted]");
        assert_eq!(redacted["list"][0]["other"], "kept");
        assert_eq!(redact_value(json!("plain")), "plain");
        assert_eq!(redact_value(json!(42)), 42);
    }

    #[test]
    fn scrub_message_redacts_credential_fragments() {
        assert_eq!(
            scrub_message("login failed password=hunter2"),
            "login failed password[redacted]"
        );
        assert_eq!(scrub_message("token: abc123 here"), "token[redacted] here");
        assert_eq!(scrub_message("nothing secret here"), "nothing secret here");
    }

    #[test]
    fn scrub_message_covers_the_full_sensitive_keyword_list() {
        assert_eq!(
            scrub_message("session=abc123 done"),
            "session[redacted] done"
        );
        assert_eq!(
            scrub_message("cookie: xyz here"),
            "cookie[redacted] here"
        );
        assert_eq!(
            scrub_message("client_secret=shh"),
            "client_secret[redacted]"
        );
        assert_eq!(
            scrub_message("refresh_token: r123"),
            "refresh_token[redacted]"
        );
        assert_eq!(
            scrub_message("private_key=---BEGIN---"),
            "private_key[redacted]"
        );
        assert_eq!(
            scrub_message("access_key: AKIA123"),
            "access_key[redacted]"
        );
        assert_eq!(
            scrub_message("credential=leaked"),
            "credential[redacted]"
        );
        // Plain words without a `key=value` shape are left alone.
        assert_eq!(scrub_message("session expired"), "session expired");
    }

    #[test]
    fn levels_normalize_and_rank() {
        assert_eq!(normalize_level(" INFO "), Some("info".to_string()));
        assert_eq!(normalize_level("Error"), Some("error".to_string()));
        assert_eq!(normalize_level("verbose"), None);
        assert_eq!(normalize_level(""), None);
        assert!(level_rank("trace") < level_rank("debug"));
        assert!(level_rank("debug") < level_rank("info"));
        assert!(level_rank("info") < level_rank("warn"));
        assert!(level_rank("warn") < level_rank("error"));
        assert_eq!(level_rank("unknown"), 2);
    }

    #[test]
    fn generated_ids_parse_as_uuids() {
        assert!(Uuid::parse_str(&new_id()).is_ok());
        assert_ne!(new_id(), new_id());
    }
}
