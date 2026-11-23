// hardening — settings.
// Security settings, defaults, and updates.
//
// Split from `crates/unified-core/src/hardening.rs`; moved verbatim, no behaviour changes.

use super::*;
use crate::error::UnifiedError;
use crate::hooks::HookBus;
use crate::json;
use crate::middleware::{Middleware, MiddlewareResult, NextFn};
use actix_web::HttpResponse;
use actix_web::dev::ServiceRequest;
use actix_web::http::header::{self, HeaderValue};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Default fallbacks, used only by `SecuritySettings::default()`.
/// Enforcement always reads the live settings value, never these.
pub const DEFAULT_BRUTE_FORCE_MAX_FAILURES: u32 = 5;
pub const DEFAULT_BRUTE_FORCE_WINDOW_SECS: u64 = 900;
pub const DEFAULT_BRUTE_FORCE_BASE_LOCKOUT_SECS: u64 = 300;
pub const DEFAULT_BRUTE_FORCE_LOCKOUT_MULTIPLIER: u32 = 2;
pub const DEFAULT_BRUTE_FORCE_MAX_LOCKOUT_SECS: u64 = 3600;
pub const DEFAULT_RATE_LIMIT_PER_IP_REQUESTS: u32 = 300;
pub const DEFAULT_RATE_LIMIT_PER_IP_WINDOW_SECS: u64 = 60;
pub const DEFAULT_RATE_LIMIT_PER_USER_REQUESTS: u32 = 600;
pub const DEFAULT_RATE_LIMIT_PER_USER_WINDOW_SECS: u64 = 60;
pub const DEFAULT_RATE_LIMIT_PER_ENDPOINT_REQUESTS: u32 = 120;
pub const DEFAULT_RATE_LIMIT_PER_ENDPOINT_WINDOW_SECS: u64 = 60;
pub const DEFAULT_FLOOD_BURST_THRESHOLD: u32 = 100;
pub const DEFAULT_FLOOD_BURST_WINDOW_SECS: u64 = 10;
pub const DEFAULT_MAX_CONCURRENT_PER_IP: u32 = 50;
pub const DEFAULT_MAX_BODY_BYTES: u64 = 1_048_576;
pub const DEFAULT_MAX_BODY_BYTES_UPLOAD: u64 = 104_857_600;
pub const DEFAULT_FAILED_AUTH_ALERT_THRESHOLD: u32 = 5;
pub const DEFAULT_FAILED_AUTH_ALERT_WINDOW_SECS: u64 = 900;
pub const DEFAULT_IP_REPUTATION_FLAG_THRESHOLD: i32 = 100;
pub const DEFAULT_IP_REPUTATION_BURST_PENALTY: i32 = 25;

/// Every security hardening setting. This struct is the shape of the
/// `security:` section in the configuration file (see the coordinator
/// report for the `AppConfig` snippet) and the runtime value held by
/// `HardeningState`; a `PUT /api/v1/security/hardening/settings`
/// updates the runtime copy only and does not rewrite the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SecuritySettings {
    /// Master switch for the whole hardening pipeline. When false, the
    /// middleware passes every request through unchanged.
    pub enabled: bool,
    /// Brute-force protection switch.
    pub brute_force_enabled: bool,
    /// Failed logins per IP inside `brute_force_window_secs` that
    /// trigger a lockout.
    pub brute_force_max_failures: u32,
    /// Failed logins per account inside `brute_force_window_secs`
    /// that trigger a lockout.
    pub brute_force_account_max_failures: u32,
    /// Window in seconds over which brute-force failures are counted.
    pub brute_force_window_secs: u64,
    /// Lockout duration in seconds after the first lockout.
    pub brute_force_base_lockout_secs: u64,
    /// Each repeat lockout multiplies the previous duration by this
    /// factor, up to `brute_force_max_lockout_secs`.
    pub brute_force_lockout_multiplier: u32,
    /// Maximum lockout duration in seconds.
    pub brute_force_max_lockout_secs: u64,
    /// Rate limiting switch.
    pub rate_limit_enabled: bool,
    /// Requests allowed per IP per `rate_limit_per_ip_window_secs`.
    pub rate_limit_per_ip_requests: u32,
    /// Window in seconds for the per-IP rate limit.
    pub rate_limit_per_ip_window_secs: u64,
    /// Requests allowed per authenticated user (per bearer token hash)
    /// per `rate_limit_per_user_window_secs`.
    pub rate_limit_per_user_requests: u32,
    /// Window in seconds for the per-user rate limit.
    pub rate_limit_per_user_window_secs: u64,
    /// Requests allowed per endpoint (method + normalized path) per
    /// `rate_limit_per_endpoint_window_secs`.
    pub rate_limit_per_endpoint_requests: u32,
    /// Window in seconds for the per-endpoint rate limit.
    pub rate_limit_per_endpoint_window_secs: u64,
    /// Path prefixes exempt from rate limiting and the blocklist.
    /// The ACME HTTP-01 challenge prefix must stay here: a blocked or
    /// throttled challenge would break certificate issuance and
    /// renewal, and the challenge token is unguessable and single-use.
    pub rate_limit_exempt_paths: Vec<String>,
    /// Requests from one IP inside `flood_burst_window_secs` that
    /// count as a flood burst.
    pub flood_burst_threshold: u32,
    /// Window in seconds for flood-burst detection.
    pub flood_burst_window_secs: u64,
    /// Simultaneous in-flight requests allowed per IP.
    pub max_concurrent_per_ip: u32,
    /// Maximum request body size in bytes for ordinary routes.
    pub max_body_bytes_default: u64,
    /// Maximum request body size in bytes for upload routes.
    pub max_body_bytes_upload: u64,
    /// Path prefixes that use `max_body_bytes_upload` instead of
    /// `max_body_bytes_default`.
    pub upload_path_prefixes: Vec<String>,
    /// Value of the Strict-Transport-Security response header.
    /// Empty disables the header.
    pub headers_hsts: String,
    /// Value of the X-Content-Type-Options response header.
    pub headers_content_type_options: String,
    /// Value of the X-Frame-Options response header.
    pub headers_frame_options: String,
    /// Value of the Referrer-Policy response header.
    pub headers_referrer_policy: String,
    /// Value of the Content-Security-Policy response header.
    pub headers_content_security_policy: String,
    /// Value of the Permissions-Policy response header.
    pub headers_permissions_policy: String,
    /// Failed-auth count inside `failed_auth_alert_window_secs` that
    /// raises a `security.auth.failed` alert.
    pub failed_auth_alert_threshold: u32,
    /// Window in seconds for failed-auth alert clustering.
    pub failed_auth_alert_window_secs: u64,
    /// Reputation score at which an IP is automatically flagged.
    pub ip_reputation_flag_threshold: i32,
    /// Reputation points added when an IP triggers flood detection.
    pub ip_reputation_burst_penalty: i32,
    /// CIDR ranges or bare IPs that bypass rate limiting.
    pub allowlist: Vec<String>,
    /// CIDR ranges or bare IPs rejected with 403.
    pub blocklist: Vec<String>,
    /// CIDR ranges or bare IPs of trusted reverse proxies / load
    /// balancers. Only a peer address inside this list may supply the
    /// client IP through `X-Forwarded-For` (see
    /// `trusted_client_ip`): when the list is empty (the default) no
    /// forwarded header is ever trusted and the TCP peer address is
    /// always the client IP. An untrusted client that can set
    /// `X-Forwarded-For` must never be able to spoof the IP the
    /// blocklist, rate limiter, and brute-force engine enforce on.
    pub trusted_proxies: Vec<String>,
}

impl Default for SecuritySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            brute_force_enabled: true,
            brute_force_max_failures: DEFAULT_BRUTE_FORCE_MAX_FAILURES,
            brute_force_account_max_failures: DEFAULT_BRUTE_FORCE_MAX_FAILURES,
            brute_force_window_secs: DEFAULT_BRUTE_FORCE_WINDOW_SECS,
            brute_force_base_lockout_secs: DEFAULT_BRUTE_FORCE_BASE_LOCKOUT_SECS,
            brute_force_lockout_multiplier: DEFAULT_BRUTE_FORCE_LOCKOUT_MULTIPLIER,
            brute_force_max_lockout_secs: DEFAULT_BRUTE_FORCE_MAX_LOCKOUT_SECS,
            rate_limit_enabled: true,
            rate_limit_per_ip_requests: DEFAULT_RATE_LIMIT_PER_IP_REQUESTS,
            rate_limit_per_ip_window_secs: DEFAULT_RATE_LIMIT_PER_IP_WINDOW_SECS,
            rate_limit_per_user_requests: DEFAULT_RATE_LIMIT_PER_USER_REQUESTS,
            rate_limit_per_user_window_secs: DEFAULT_RATE_LIMIT_PER_USER_WINDOW_SECS,
            rate_limit_per_endpoint_requests: DEFAULT_RATE_LIMIT_PER_ENDPOINT_REQUESTS,
            rate_limit_per_endpoint_window_secs: DEFAULT_RATE_LIMIT_PER_ENDPOINT_WINDOW_SECS,
            rate_limit_exempt_paths: vec![
                "/health".to_string(),
                "/.well-known/acme-challenge/".to_string(),
            ],
            flood_burst_threshold: DEFAULT_FLOOD_BURST_THRESHOLD,
            flood_burst_window_secs: DEFAULT_FLOOD_BURST_WINDOW_SECS,
            max_concurrent_per_ip: DEFAULT_MAX_CONCURRENT_PER_IP,
            max_body_bytes_default: DEFAULT_MAX_BODY_BYTES,
            max_body_bytes_upload: DEFAULT_MAX_BODY_BYTES_UPLOAD,
            upload_path_prefixes: vec![
                "/api/v1/files".to_string(),
                "/api/v1/storage".to_string(),
                // Documents uploads declare up to 100 MiB
                // (MAX_UPLOAD_BYTES) with 8 MiB chunks; without this
                // prefix the 1 MiB default body cap rejects them
                // with 413 before the handler runs.
                "/api/v1/documents".to_string(),
                // Agent screenshot uploads (same 1 MiB default-cap
                // problem; see the documents prefix above).
                "/api/v1/timetracker/agent".to_string(),
            ],
            headers_hsts: "max-age=31536000; includeSubDomains".to_string(),
            headers_content_type_options: "nosniff".to_string(),
            headers_frame_options: "DENY".to_string(),
            headers_referrer_policy: "strict-origin-when-cross-origin".to_string(),
            headers_content_security_policy: "default-src 'self'; frame-ancestors 'none'"
                .to_string(),
            headers_permissions_policy: "camera=(), microphone=(), geolocation=()".to_string(),
            failed_auth_alert_threshold: DEFAULT_FAILED_AUTH_ALERT_THRESHOLD,
            failed_auth_alert_window_secs: DEFAULT_FAILED_AUTH_ALERT_WINDOW_SECS,
            ip_reputation_flag_threshold: DEFAULT_IP_REPUTATION_FLAG_THRESHOLD,
            ip_reputation_burst_penalty: DEFAULT_IP_REPUTATION_BURST_PENALTY,
            allowlist: Vec::new(),
            blocklist: Vec::new(),
            trusted_proxies: Vec::new(),
        }
    }
}

impl SecuritySettings {
    /// Build settings from the application configuration. Reads the
    /// `security` section added to `AppConfig` by the integrator (see
    /// the coordinator report); missing keys fall back to the defaults
    /// above because the section is `#[serde(default)]`.
    pub fn from_app_config(config: &crate::config::AppConfig) -> Self {
        config.security.clone()
    }

    /// Validate the settings. Returns a list of problems; an empty
    /// list means the settings are valid. Used by `AppConfig::validate`
    /// and by the settings-update endpoint.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.brute_force_max_failures == 0 {
            problems.push("security.brute_force_max_failures must be at least 1".to_string());
        }
        if self.brute_force_account_max_failures == 0 {
            problems.push(
                "security.brute_force_account_max_failures must be at least 1".to_string(),
            );
        }
        if self.brute_force_window_secs == 0 {
            problems.push("security.brute_force_window_secs must be at least 1".to_string());
        }
        if self.brute_force_base_lockout_secs == 0 {
            problems.push(
                "security.brute_force_base_lockout_secs must be at least 1".to_string(),
            );
        }
        if self.brute_force_lockout_multiplier == 0 {
            problems.push(
                "security.brute_force_lockout_multiplier must be at least 1".to_string(),
            );
        }
        if self.brute_force_max_lockout_secs < self.brute_force_base_lockout_secs {
            problems.push(
                "security.brute_force_max_lockout_secs must be at least brute_force_base_lockout_secs"
                    .to_string(),
            );
        }
        if self.rate_limit_per_ip_requests == 0 {
            problems.push("security.rate_limit_per_ip_requests must be at least 1".to_string());
        }
        if self.rate_limit_per_ip_window_secs == 0 {
            problems.push(
                "security.rate_limit_per_ip_window_secs must be at least 1".to_string(),
            );
        }
        if self.rate_limit_per_user_requests == 0 {
            problems.push(
                "security.rate_limit_per_user_requests must be at least 1".to_string(),
            );
        }
        if self.rate_limit_per_user_window_secs == 0 {
            problems.push(
                "security.rate_limit_per_user_window_secs must be at least 1".to_string(),
            );
        }
        if self.rate_limit_per_endpoint_requests == 0 {
            problems.push(
                "security.rate_limit_per_endpoint_requests must be at least 1".to_string(),
            );
        }
        if self.rate_limit_per_endpoint_window_secs == 0 {
            problems.push(
                "security.rate_limit_per_endpoint_window_secs must be at least 1".to_string(),
            );
        }
        if self.flood_burst_threshold == 0 {
            problems.push("security.flood_burst_threshold must be at least 1".to_string());
        }
        if self.flood_burst_window_secs == 0 {
            problems.push("security.flood_burst_window_secs must be at least 1".to_string());
        }
        if self.max_concurrent_per_ip == 0 {
            problems.push("security.max_concurrent_per_ip must be at least 1".to_string());
        }
        if self.max_body_bytes_default == 0 {
            problems.push("security.max_body_bytes_default must be at least 1".to_string());
        }
        if self.max_body_bytes_upload == 0 {
            problems.push("security.max_body_bytes_upload must be at least 1".to_string());
        }
        if self.failed_auth_alert_threshold == 0 {
            problems.push(
                "security.failed_auth_alert_threshold must be at least 1".to_string(),
            );
        }
        if self.failed_auth_alert_window_secs == 0 {
            problems.push(
                "security.failed_auth_alert_window_secs must be at least 1".to_string(),
            );
        }
        for entry in self.allowlist.iter().chain(self.blocklist.iter()) {
            if parse_cidr(entry).is_err() {
                problems.push(format!(
                    "security IP list entry \"{entry}\" is not a valid IP address or CIDR range"
                ));
            }
        }
        for entry in &self.trusted_proxies {
            if parse_cidr(entry).is_err() {
                problems.push(format!(
                    "security.trusted_proxies entry \"{entry}\" is not a valid IP address or CIDR range"
                ));
            }
        }
        problems
    }
}

/// Partial settings update: every field is optional, only the fields
/// present in the request are changed. `allowlist` / `blocklist`
/// replace the whole list when present.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SecuritySettingsUpdate {
    pub enabled: Option<bool>,
    pub brute_force_enabled: Option<bool>,
    pub brute_force_max_failures: Option<u32>,
    pub brute_force_account_max_failures: Option<u32>,
    pub brute_force_window_secs: Option<u64>,
    pub brute_force_base_lockout_secs: Option<u64>,
    pub brute_force_lockout_multiplier: Option<u32>,
    pub brute_force_max_lockout_secs: Option<u64>,
    pub rate_limit_enabled: Option<bool>,
    pub rate_limit_per_ip_requests: Option<u32>,
    pub rate_limit_per_ip_window_secs: Option<u64>,
    pub rate_limit_per_user_requests: Option<u32>,
    pub rate_limit_per_user_window_secs: Option<u64>,
    pub rate_limit_per_endpoint_requests: Option<u32>,
    pub rate_limit_per_endpoint_window_secs: Option<u64>,
    pub rate_limit_exempt_paths: Option<Vec<String>>,
    pub flood_burst_threshold: Option<u32>,
    pub flood_burst_window_secs: Option<u64>,
    pub max_concurrent_per_ip: Option<u32>,
    pub max_body_bytes_default: Option<u64>,
    pub max_body_bytes_upload: Option<u64>,
    pub upload_path_prefixes: Option<Vec<String>>,
    pub headers_hsts: Option<String>,
    pub headers_content_type_options: Option<String>,
    pub headers_frame_options: Option<String>,
    pub headers_referrer_policy: Option<String>,
    pub headers_content_security_policy: Option<String>,
    pub headers_permissions_policy: Option<String>,
    pub failed_auth_alert_threshold: Option<u32>,
    pub failed_auth_alert_window_secs: Option<u64>,
    pub ip_reputation_flag_threshold: Option<i32>,
    pub ip_reputation_burst_penalty: Option<i32>,
    pub allowlist: Option<Vec<String>>,
    pub blocklist: Option<Vec<String>>,
    pub trusted_proxies: Option<Vec<String>>,
}

/// Partial security-header update: only the fields present are changed.
/// An empty string disables that header.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HeadersUpdate {
    pub strict_transport_security: Option<String>,
    pub content_type_options: Option<String>,
    pub frame_options: Option<String>,
    pub referrer_policy: Option<String>,
    pub content_security_policy: Option<String>,
    pub permissions_policy: Option<String>,
}

impl HardeningState {
    /// Current settings (a snapshot copy).
    pub fn settings(&self) -> SecuritySettings {
        self.settings
            .read()
            .expect("security settings lock poisoned")
            .clone()
    }

    /// Apply a partial settings update. Validates the merged result;
    /// on failure nothing is changed. Emits `security.settings.updated`.
    pub fn update_settings(
        &self,
        update: SecuritySettingsUpdate,
    ) -> Result<SecuritySettings, String> {
        let mut next = self.settings();
        if let Some(value) = update.enabled {
            next.enabled = value;
        }
        if let Some(value) = update.brute_force_enabled {
            next.brute_force_enabled = value;
        }
        if let Some(value) = update.brute_force_max_failures {
            next.brute_force_max_failures = value;
        }
        if let Some(value) = update.brute_force_account_max_failures {
            next.brute_force_account_max_failures = value;
        }
        if let Some(value) = update.brute_force_window_secs {
            next.brute_force_window_secs = value;
        }
        if let Some(value) = update.brute_force_base_lockout_secs {
            next.brute_force_base_lockout_secs = value;
        }
        if let Some(value) = update.brute_force_lockout_multiplier {
            next.brute_force_lockout_multiplier = value;
        }
        if let Some(value) = update.brute_force_max_lockout_secs {
            next.brute_force_max_lockout_secs = value;
        }
        if let Some(value) = update.rate_limit_enabled {
            next.rate_limit_enabled = value;
        }
        if let Some(value) = update.rate_limit_per_ip_requests {
            next.rate_limit_per_ip_requests = value;
        }
        if let Some(value) = update.rate_limit_per_ip_window_secs {
            next.rate_limit_per_ip_window_secs = value;
        }
        if let Some(value) = update.rate_limit_per_user_requests {
            next.rate_limit_per_user_requests = value;
        }
        if let Some(value) = update.rate_limit_per_user_window_secs {
            next.rate_limit_per_user_window_secs = value;
        }
        if let Some(value) = update.rate_limit_per_endpoint_requests {
            next.rate_limit_per_endpoint_requests = value;
        }
        if let Some(value) = update.rate_limit_per_endpoint_window_secs {
            next.rate_limit_per_endpoint_window_secs = value;
        }
        if let Some(value) = update.rate_limit_exempt_paths {
            next.rate_limit_exempt_paths = value;
        }
        if let Some(value) = update.flood_burst_threshold {
            next.flood_burst_threshold = value;
        }
        if let Some(value) = update.flood_burst_window_secs {
            next.flood_burst_window_secs = value;
        }
        if let Some(value) = update.max_concurrent_per_ip {
            next.max_concurrent_per_ip = value;
        }
        if let Some(value) = update.max_body_bytes_default {
            next.max_body_bytes_default = value;
        }
        if let Some(value) = update.max_body_bytes_upload {
            next.max_body_bytes_upload = value;
        }
        if let Some(value) = update.upload_path_prefixes {
            next.upload_path_prefixes = value;
        }
        if let Some(value) = update.headers_hsts {
            next.headers_hsts = value;
        }
        if let Some(value) = update.headers_content_type_options {
            next.headers_content_type_options = value;
        }
        if let Some(value) = update.headers_frame_options {
            next.headers_frame_options = value;
        }
        if let Some(value) = update.headers_referrer_policy {
            next.headers_referrer_policy = value;
        }
        if let Some(value) = update.headers_content_security_policy {
            next.headers_content_security_policy = value;
        }
        if let Some(value) = update.headers_permissions_policy {
            next.headers_permissions_policy = value;
        }
        if let Some(value) = update.failed_auth_alert_threshold {
            next.failed_auth_alert_threshold = value;
        }
        if let Some(value) = update.failed_auth_alert_window_secs {
            next.failed_auth_alert_window_secs = value;
        }
        if let Some(value) = update.ip_reputation_flag_threshold {
            next.ip_reputation_flag_threshold = value;
        }
        if let Some(value) = update.ip_reputation_burst_penalty {
            next.ip_reputation_burst_penalty = value;
        }
        // Record which lists changed before the moves below take the
        // vectors out of `update`; the flags are read after.
        let allowlist_changed = update.allowlist.is_some();
        let blocklist_changed = update.blocklist.is_some();
        if let Some(value) = update.allowlist {
            next.allowlist = value;
        }
        if let Some(value) = update.blocklist {
            next.blocklist = value;
        }
        if let Some(value) = update.trusted_proxies {
            next.trusted_proxies = value;
        }
        let problems = next.validate();
        if !problems.is_empty() {
            return Err(problems.join("; "));
        }
        if allowlist_changed {
            self.replace_ip_list("allow", &next.allowlist);
        }
        if blocklist_changed {
            self.replace_ip_list("block", &next.blocklist);
        }
        *self
            .settings
            .write()
            .expect("security settings lock poisoned") = next.clone();
        self.hooks.emit(
            security_events::SETTINGS_UPDATED,
            serde_json::json!({ "updated_by": "security.settings" }),
        );
        Ok(next)
    }

    /// Apply a partial security-header update.
    pub fn update_headers(&self, update: HeadersUpdate) -> SecuritySettings {
        let mut next = self.settings();
        if let Some(value) = update.strict_transport_security {
            next.headers_hsts = value;
        }
        if let Some(value) = update.content_type_options {
            next.headers_content_type_options = value;
        }
        if let Some(value) = update.frame_options {
            next.headers_frame_options = value;
        }
        if let Some(value) = update.referrer_policy {
            next.headers_referrer_policy = value;
        }
        if let Some(value) = update.content_security_policy {
            next.headers_content_security_policy = value;
        }
        if let Some(value) = update.permissions_policy {
            next.headers_permissions_policy = value;
        }
        *self
            .settings
            .write()
            .expect("security settings lock poisoned") = next.clone();
        self.hooks.emit(
            security_events::SETTINGS_UPDATED,
            serde_json::json!({ "updated_by": "security.headers" }),
        );
        next
    }
}
