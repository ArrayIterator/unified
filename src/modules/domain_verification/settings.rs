// Domain Verification — Admin settings.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Admin settings.
//
// Every field is admin-editable through
// `GET/PUT /api/v1/domains/verification/settings`. The defaults below
// are CONFIGURABLE DEFAULTS ONLY: they seed the store at startup and
// an admin save replaces them wholesale. No handler reads a literal
// record format, timeout, attempt count, or window from anywhere
// else; verification logic always reads the current settings.
// ---------------------------------------------------------------------------

/// Admin-determined verification settings (one live version; every
/// PUT bumps `version` and is announced with
/// `domain.verification.settings_updated`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationSettings {
    /// DNS record type to check: `"txt"` or `"cname"`.
    pub record_type: String,
    /// Host template for the lookup. Either a bare label such as
    /// `"_unified-verify"` (looked up as
    /// `"_unified-verify.<domain>"`) or a template containing
    /// `"{domain}"`, used verbatim after substitution.
    pub host_template: String,
    /// Expected record value template. MUST contain `"{token}"`.
    /// Example TXT: `"unified-verify={token}"`. Example CNAME
    /// target: `"{token}.verify.example.net"`.
    pub value_template: String,
    /// Length of generated tokens, in characters.
    pub token_length: usize,
    /// Characters tokens are drawn from (each character must be a
    /// single ASCII character; duplicates are removed).
    pub token_alphabet: String,
    /// Per-attempt DNS lookup timeout, in seconds.
    pub verify_timeout_seconds: u64,
    /// Maximum DNS check attempts per verify call, and also the
    /// rate-limit budget: that many attempts inside
    /// `rate_limit_window_seconds` fail closed with HTTP 429.
    pub max_attempts: u32,
    /// Delay between attempts inside one verify call, in seconds.
    pub retry_delay_seconds: u64,
    /// Token lifetime from issuance, in seconds.
    pub token_ttl_seconds: i64,
    /// Rate-limit window for verify attempts per domain, in seconds.
    pub rate_limit_window_seconds: i64,
    /// Settings version, bumped on every admin save.
    pub version: u64,
    /// When these settings were last saved.
    pub updated_at: DateTime<Utc>,
}

/// CONFIGURABLE DEFAULTS (secure by default: TXT record, 32-char
/// token). These seed the settings store only; an admin save
/// replaces them entirely. Do NOT copy these literals into handler
/// logic — handlers must read `current_settings()`.
pub fn default_settings() -> VerificationSettings {
    VerificationSettings {
        record_type: "txt".to_string(),
        host_template: "_unified-verify".to_string(),
        value_template: "unified-verify={token}".to_string(),
        token_length: 32,
        token_alphabet: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"
            .to_string(),
        verify_timeout_seconds: 10,
        max_attempts: 5,
        retry_delay_seconds: 10,
        token_ttl_seconds: 604800,
        rate_limit_window_seconds: 3600,
        version: 1,
        updated_at: Utc::now(),
    }
}

impl VerificationSettings {
    /// Validate admin-supplied settings. Every bound is checked
    /// here so an invalid save can never reach the store.
    pub fn validate(&self) -> UnifiedResult<()> {
        if self.record_type != "txt" && self.record_type != "cname" {
            return Err(UnifiedError::bad_request(
                "record_type must be \"txt\" or \"cname\"",
            ));
        }
        let host = self.host_template.trim();
        if host.is_empty() || host.len() > 253 {
            return Err(UnifiedError::bad_request(
                "host_template must be 1-253 characters",
            ));
        }
        if host.chars().any(|c| {
            !(c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_' || c == '{' || c == '}')
        }) {
            return Err(UnifiedError::bad_request(
                "host_template may only contain letters, digits, hyphens, dots, underscores, and the {domain} placeholder",
            ));
        }
        if host.matches("{domain}").count() > 1 {
            return Err(UnifiedError::bad_request(
                "host_template may contain {domain} at most once",
            ));
        }
        if host.contains('{') && !host.contains("{domain}") {
            return Err(UnifiedError::bad_request(
                "host_template placeholders: only {domain} is supported",
            ));
        }
        if !self.value_template.contains("{token}") {
            return Err(UnifiedError::bad_request(
                "value_template must contain the {token} placeholder",
            ));
        }
        if self.value_template.len() > 1000 {
            return Err(UnifiedError::bad_request(
                "value_template must be at most 1000 characters",
            ));
        }
        if !(16..=128).contains(&self.token_length) {
            return Err(UnifiedError::bad_request(
                "token_length must be between 16 and 128",
            ));
        }
        let alphabet: Vec<char> = {
            let mut seen: Vec<char> = Vec::new();
            for c in self.token_alphabet.chars() {
                if !c.is_ascii() || c.is_whitespace() || c.is_control() {
                    return Err(UnifiedError::bad_request(
                        "token_alphabet must contain only visible ASCII characters",
                    ));
                }
                if !seen.contains(&c) {
                    seen.push(c);
                }
            }
            seen
        };
        if alphabet.len() < 8 {
            return Err(UnifiedError::bad_request(
                "token_alphabet must contain at least 8 distinct characters",
            ));
        }
        if !(1..=120).contains(&self.verify_timeout_seconds) {
            return Err(UnifiedError::bad_request(
                "verify_timeout_seconds must be between 1 and 120",
            ));
        }
        if !(1..=10).contains(&self.max_attempts) {
            return Err(UnifiedError::bad_request(
                "max_attempts must be between 1 and 10",
            ));
        }
        if self.retry_delay_seconds > 300 {
            return Err(UnifiedError::bad_request(
                "retry_delay_seconds must be between 0 and 300",
            ));
        }
        if !(300..=2_592_000).contains(&self.token_ttl_seconds) {
            return Err(UnifiedError::bad_request(
                "token_ttl_seconds must be between 300 and 2592000 (30 days)",
            ));
        }
        if !(60..=86_400).contains(&self.rate_limit_window_seconds) {
            return Err(UnifiedError::bad_request(
                "rate_limit_window_seconds must be between 60 and 86400",
            ));
        }
        Ok(())
    }
}

pub(crate) static SETTINGS: OnceLock<RwLock<VerificationSettings>> = OnceLock::new();

pub(crate) fn settings_store() -> &'static RwLock<VerificationSettings> {
    SETTINGS.get_or_init(|| RwLock::new(default_settings()))
}

/// Snapshot of the current admin settings. All verification logic
/// reads settings through here — never through a literal.
pub(crate) fn current_settings() -> VerificationSettings {
    settings_store()
        .read()
        .expect("verification settings lock poisoned")
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_are_valid() {
        assert!(default_settings().validate().is_ok());
    }

    #[test]
    fn default_settings_are_secure_txt_with_32_char_tokens() {
        let settings = default_settings();
        assert_eq!(settings.record_type, "txt");
        assert_eq!(settings.token_length, 32);
        assert_eq!(settings.token_ttl_seconds, 604800);
    }

    #[test]
    fn validate_rejects_unknown_record_type() {
        let mut settings = default_settings();
        settings.record_type = "mx".to_string();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn validate_rejects_short_token_length() {
        let mut settings = default_settings();
        settings.token_length = 8;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn validate_rejects_value_template_without_token() {
        let mut settings = default_settings();
        settings.value_template = "no-placeholder-here".to_string();
        assert!(settings.validate().is_err());
    }
}
