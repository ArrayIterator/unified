// Security hardening enforcement engine.
//
// All enforcement is driven by `SecuritySettings`: every limit, window,
// threshold, and header value is a real configuration value (see the
// `security` section snippet for `AppConfig` in the coordinator report;
// `SecuritySettings` is also the shape of that config section). Named
// constants below exist only as fallbacks for `Default`, never as
// enforcement values buried in code.
//
// The engine is split into four Actix middlewares (registered by
// `crate::http::server`, ordered by priority):
//   - `SecurityHeadersMiddleware` (priority 1000): response headers.
//   - `IpFilterMiddleware` (priority 900): CIDR allow/block lists.
//   - `RateLimitMiddleware` (priority 800): rate limiting, flood
//     detection, and per-IP concurrent-connection limits.
//   - `RequestSizeMiddleware` (priority 700): per-route-group body limits.
//
// State is shared between the middleware and the Security module HTTP
// handlers through one `HardeningState` behind `shared_state()` /
// `install_shared_state()`: the server installs the config-derived
// state at startup, and the module handlers read the same instance on
// every request, so settings updates and lockouts are visible on both
// sides immediately. Nothing here is persisted to disk or to the
// database: IP lists, attempt counters, and reputation scores are
// in-memory and reset on restart. Operators who need persistence
// should re-apply the lists from configuration on startup.
//
// Events emitted on the hook bus (so the Notifications and Audit
// modules can pick them up without a direct dependency):
//   - `security.bruteforce.locked` / `security.bruteforce.unlocked`
//   - `security.auth.failed` (failed-auth cluster reached its threshold)
//   - `security.ip.flagged` / `security.ip.unflagged`
//   - `security.settings.updated`
//   - `security.rate_limited`

use crate::core::error::UnifiedError;
use crate::core::hooks::HookBus;
use crate::core::json;
use crate::core::middleware::{Middleware, MiddlewareResult, NextFn};
use actix_web::dev::ServiceRequest;
use actix_web::http::header::{self, HeaderValue};
use actix_web::HttpResponse;
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

/// Well-known security events emitted on the hook bus.
pub mod security_events {
    pub const BRUTEFORCE_LOCKED: &str = "security.bruteforce.locked";
    pub const BRUTEFORCE_UNLOCKED: &str = "security.bruteforce.unlocked";
    pub const AUTH_FAILED: &str = "security.auth.failed";
    pub const IP_FLAGGED: &str = "security.ip.flagged";
    pub const IP_UNFLAGGED: &str = "security.ip.unflagged";
    pub const SETTINGS_UPDATED: &str = "security.settings.updated";
    pub const RATE_LIMITED: &str = "security.rate_limited";
}

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
    pub fn from_app_config(config: &crate::core::config::AppConfig) -> Self {
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

// ---------------------------------------------------------------------------
// CIDR ranges
// ---------------------------------------------------------------------------

/// One parsed CIDR range (or a bare IP, stored as a /32 or /128).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct CidrRange {
    pub network: IpAddr,
    pub prefix: u8,
    /// Normalized `network/prefix` form, used as the map key.
    pub cidr: String,
}

/// Parse an IP address or CIDR range. A bare IP becomes a host route
/// (/32 for IPv4, /128 for IPv6). Returns a human-readable error.
pub fn parse_cidr(input: &str) -> Result<CidrRange, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("IP list entry must not be empty".to_string());
    }
    let (ip_text, prefix) = match trimmed.split_once('/') {
        Some((ip_text, prefix_text)) => {
            let ip: IpAddr = ip_text.parse().map_err(|_| {
                format!("\"{trimmed}\" is not a valid IP address or CIDR range")
            })?;
            let max_prefix = match ip {
                IpAddr::V4(_) => 32,
                IpAddr::V6(_) => 128,
            };
            let prefix: u8 = prefix_text.parse().map_err(|_| {
                format!("\"{trimmed}\" has an invalid CIDR prefix")
            })?;
            if prefix > max_prefix {
                return Err(format!(
                    "\"{trimmed}\" has a prefix larger than {max_prefix}"
                ));
            }
            (ip_text, prefix)
        }
        None => {
            let ip: IpAddr = trimmed
                .parse()
                .map_err(|_| format!("\"{trimmed}\" is not a valid IP address or CIDR range"))?;
            let prefix = match ip {
                IpAddr::V4(_) => 32,
                IpAddr::V6(_) => 128,
            };
            (trimmed, prefix)
        }
    };
    let ip: IpAddr = ip_text.parse().expect("IP parsed above");
    let network = mask_address(ip, prefix);
    Ok(CidrRange {
        network,
        prefix,
        cidr: format!("{network}/{prefix}"),
    })
}

fn mask_address(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(addr) => {
            let bits = u32::from(addr);
            let mask = if prefix == 0 {
                0u32
            } else {
                u32::MAX << (32 - prefix)
            };
            IpAddr::V4((bits & mask).into())
        }
        IpAddr::V6(addr) => {
            let bits = u128::from(addr);
            let mask = if prefix == 0 {
                0u128
            } else {
                u128::MAX << (128 - prefix)
            };
            IpAddr::V6((bits & mask).into())
        }
    }
}

impl CidrRange {
    /// True when `ip` falls inside this range.
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.network, ip) {
            (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_)) => {
                mask_address(ip, self.prefix) == self.network
            }
            _ => false,
        }
    }
}

/// One IP list record, as returned by the list endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct IpListEntry {
    pub list: String,
    pub cidr: String,
    pub network: String,
    pub prefix: u8,
    pub added_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// State records
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct AttemptState {
    failures: u32,
    window_start: Instant,
    locked_until: Option<Instant>,
    lock_count: u32,
    last_failure: DateTime<Utc>,
}

impl AttemptState {
    fn new(now: Instant) -> Self {
        Self {
            failures: 0,
            window_start: now,
            locked_until: None,
            lock_count: 0,
            last_failure: Utc::now(),
        }
    }

    fn is_locked(&self, now: Instant) -> bool {
        self.locked_until.is_some_and(|until| until > now)
    }

    fn locked_until_wall(&self) -> Option<DateTime<Utc>> {
        self.locked_until.map(|until| {
            let remaining = until.saturating_duration_since(Instant::now());
            Utc::now() + chrono::Duration::from_std(remaining).unwrap_or(chrono::Duration::zero())
        })
    }
}

/// One lockout / attempt status, as returned by the status endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct LockoutStatus {
    pub kind: String,
    pub key: String,
    pub failures: u32,
    pub locked: bool,
    pub locked_until: Option<DateTime<Utc>>,
    pub lock_count: u32,
    pub last_failure: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct WindowBucket {
    count: u32,
    window_start: Instant,
    started_at: DateTime<Utc>,
}

/// One tracked rate-limit window, as returned by the status endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct RateLimitEntry {
    pub scope: String,
    pub key: String,
    pub limit: u32,
    pub count: u32,
    pub window_secs: u64,
    pub window_started_at: DateTime<Utc>,
    pub reset_in_secs: u64,
}

#[derive(Debug, Clone)]
struct BurstBucket {
    count: u32,
    window_start: Instant,
}

/// One IP reputation record, as returned by the reputation endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct ReputationEntry {
    pub ip: String,
    pub score: i32,
    pub flagged: bool,
    pub reason: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// One failed-auth cluster: repeated failures grouped by IP + account.
#[derive(Debug, Clone, Serialize)]
pub struct FailedAuthCluster {
    pub key: String,
    pub ip: Option<String>,
    pub account: Option<String>,
    pub count: u64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub locked: bool,
}

/// Outcome of one fixed-window rate-limit check.
#[derive(Debug, Clone)]
pub enum RateDecision {
    Allowed { remaining: u32, reset_in_secs: u64 },
    Limited { retry_after_secs: u64 },
}

// ---------------------------------------------------------------------------
// Shared enforcement state
// ---------------------------------------------------------------------------

/// The shared hardening engine. One instance is shared by the
/// middleware and the Security module handlers (see `shared_state`).
pub struct HardeningState {
    settings: RwLock<SecuritySettings>,
    hooks: Arc<HookBus>,
    brute_ip: DashMap<String, AttemptState>,
    brute_account: DashMap<String, AttemptState>,
    rate_ip: DashMap<String, WindowBucket>,
    rate_user: DashMap<String, WindowBucket>,
    rate_endpoint: DashMap<String, WindowBucket>,
    burst: DashMap<String, BurstBucket>,
    concurrent: DashMap<String, Arc<AtomicU64>>,
    allowlist: DashMap<String, IpListEntry>,
    blocklist: DashMap<String, IpListEntry>,
    reputation: DashMap<String, ReputationEntry>,
    failed_auth: DashMap<String, FailedAuthCluster>,
}

impl std::fmt::Debug for HardeningState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HardeningState")
    }
}

impl HardeningState {
    pub fn new(settings: SecuritySettings, hooks: Arc<HookBus>) -> Self {
        let state = Self {
            settings: RwLock::new(settings.clone()),
            hooks,
            brute_ip: DashMap::new(),
            brute_account: DashMap::new(),
            rate_ip: DashMap::new(),
            rate_user: DashMap::new(),
            rate_endpoint: DashMap::new(),
            burst: DashMap::new(),
            concurrent: DashMap::new(),
            allowlist: DashMap::new(),
            blocklist: DashMap::new(),
            reputation: DashMap::new(),
            failed_auth: DashMap::new(),
        };
        for entry in &settings.allowlist {
            if let Ok(range) = parse_cidr(entry) {
                state.allowlist.insert(
                    range.cidr.clone(),
                    IpListEntry {
                        list: "allow".to_string(),
                        cidr: range.cidr.clone(),
                        network: range.network.to_string(),
                        prefix: range.prefix,
                        added_at: Utc::now(),
                    },
                );
            } else {
                log::warn!("Ignoring invalid security allowlist entry \"{entry}\"");
            }
        }
        for entry in &settings.blocklist {
            if let Ok(range) = parse_cidr(entry) {
                state.blocklist.insert(
                    range.cidr.clone(),
                    IpListEntry {
                        list: "block".to_string(),
                        cidr: range.cidr.clone(),
                        network: range.network.to_string(),
                        prefix: range.prefix,
                        added_at: Utc::now(),
                    },
                );
            } else {
                log::warn!("Ignoring invalid security blocklist entry \"{entry}\"");
            }
        }
        state
    }

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
        if update.allowlist.is_some() {
            self.replace_ip_list("allow", &next.allowlist);
        }
        if update.blocklist.is_some() {
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

    fn replace_ip_list(&self, list: &str, entries: &[String]) {
        let map = if list == "allow" {
            &self.allowlist
        } else {
            &self.blocklist
        };
        map.clear();
        for entry in entries {
            if let Ok(range) = parse_cidr(entry) {
                map.insert(
                    range.cidr.clone(),
                    IpListEntry {
                        list: list.to_string(),
                        cidr: range.cidr.clone(),
                        network: range.network.to_string(),
                        prefix: range.prefix,
                        added_at: Utc::now(),
                    },
                );
            }
        }
    }

    // -- IP lists ------------------------------------------------------

    pub fn is_allowlisted(&self, ip: IpAddr) -> bool {
        self.allowlist
            .iter()
            .any(|entry| parse_cidr(&entry.value().cidr).is_ok_and(|range| range.contains(ip)))
    }

    pub fn is_blocklisted(&self, ip: IpAddr) -> bool {
        self.blocklist
            .iter()
            .any(|entry| parse_cidr(&entry.value().cidr).is_ok_and(|range| range.contains(ip)))
    }

    pub fn ip_list_entries(&self, list: &str) -> Vec<IpListEntry> {
        let map = if list == "allow" {
            &self.allowlist
        } else {
            &self.blocklist
        };
        let mut entries: Vec<IpListEntry> =
            map.iter().map(|entry| entry.value().clone()).collect();
        entries.sort_by(|a, b| a.cidr.cmp(&b.cidr));
        entries
    }

    pub fn add_ip_entry(&self, list: &str, cidr_text: &str) -> Result<IpListEntry, String> {
        if list != "allow" && list != "block" {
            return Err("list must be \"allow\" or \"block\"".to_string());
        }
        let range = parse_cidr(cidr_text)?;
        let entry = IpListEntry {
            list: list.to_string(),
            cidr: range.cidr.clone(),
            network: range.network.to_string(),
            prefix: range.prefix,
            added_at: Utc::now(),
        };
        if list == "allow" {
            self.allowlist.insert(range.cidr.clone(), entry.clone());
        } else {
            self.blocklist.insert(range.cidr.clone(), entry.clone());
        }
        Ok(entry)
    }

    pub fn remove_ip_entry(&self, list: &str, cidr_text: &str) -> Result<bool, String> {
        if list != "allow" && list != "block" {
            return Err("list must be \"allow\" or \"block\"".to_string());
        }
        let range = parse_cidr(cidr_text)?;
        let removed = if list == "allow" {
            self.allowlist.remove(&range.cidr).is_some()
        } else {
            self.blocklist.remove(&range.cidr).is_some()
        };
        Ok(removed)
    }

    // -- Brute force ---------------------------------------------------

    /// Record one failed login for an IP and/or an account. Applies
    /// progressive lockout and emits `security.bruteforce.locked` when
    /// a lockout starts. Also feeds the failed-auth cluster tracker so
    /// repeated failures raise `security.auth.failed` alerts.
    pub fn record_login_failure(&self, ip: Option<&str>, account: Option<&str>) {
        let settings = self.settings();
        if settings.brute_force_enabled {
            if let Some(ip) = ip.filter(|value| !value.is_empty()) {
                self.record_failure(
                    &self.brute_ip,
                    ip,
                    "ip",
                    settings.brute_force_max_failures,
                    &settings,
                );
            }
            if let Some(account) = account.filter(|value| !value.is_empty()) {
                self.record_failure(
                    &self.brute_account,
                    account,
                    "account",
                    settings.brute_force_account_max_failures,
                    &settings,
                );
            }
        }
        self.record_failed_auth_cluster(ip, account, &settings);
    }

    fn record_failure(
        &self,
        map: &DashMap<String, AttemptState>,
        key: &str,
        kind: &str,
        max_failures: u32,
        settings: &SecuritySettings,
    ) {
        let now = Instant::now();
        // Decide the lockout while the entry guard is held, then drop
        // the guard before emitting: a hook listener may query this
        // same map, and emitting under the write guard could deadlock.
        let locked_event: Option<(u32, u64, Option<DateTime<Utc>>)> = {
            let mut entry = map
                .entry(key.to_string())
                .or_insert_with(|| AttemptState::new(now));
            if entry.is_locked(now) {
                return;
            }
            if entry.window_start.elapsed().as_secs() >= settings.brute_force_window_secs {
                entry.failures = 0;
                entry.window_start = now;
                entry.locked_until = None;
            }
            entry.failures += 1;
            entry.last_failure = Utc::now();
            if entry.failures >= max_failures {
                entry.lock_count += 1;
                let multiplier = settings
                    .brute_force_lockout_multiplier
                    .checked_pow(entry.lock_count.saturating_sub(1))
                    .unwrap_or(u32::MAX);
                let duration_secs = settings
                    .brute_force_base_lockout_secs
                    .saturating_mul(u64::from(multiplier))
                    .min(settings.brute_force_max_lockout_secs);
                entry.locked_until = Some(now + Duration::from_secs(duration_secs));
                entry.failures = 0;
                let locked_until = entry.locked_until_wall();
                Some((entry.lock_count, duration_secs, locked_until))
            } else {
                None
            }
        };
        if let Some((lock_count, duration_secs, locked_until)) = locked_event {
            self.hooks.emit(
                security_events::BRUTEFORCE_LOCKED,
                serde_json::json!({
                    "kind": kind,
                    "key": key,
                    "lock_count": lock_count,
                    "lockout_secs": duration_secs,
                    "locked_until": locked_until,
                }),
            );
        }
    }

    /// A successful login resets the brute-force counters for the IP
    /// and the account, so a legitimate user is never locked out by
    /// earlier failures.
    pub fn record_login_success(&self, ip: Option<&str>, account: Option<&str>) {
        if let Some(ip) = ip.filter(|value| !value.is_empty()) {
            self.brute_ip.remove(ip);
        }
        if let Some(account) = account.filter(|value| !value.is_empty()) {
            self.brute_account.remove(account);
        }
    }

    /// True when the IP or the account is currently locked out.
    pub fn is_locked(&self, kind: &str, key: &str) -> bool {
        let now = Instant::now();
        let map = if kind == "account" {
            &self.brute_account
        } else {
            &self.brute_ip
        };
        map.get(key).is_some_and(|entry| entry.is_locked(now))
    }

    /// Every lockout / attempt status, most recently failed first.
    pub fn lockout_statuses(&self) -> Vec<LockoutStatus> {
        let now = Instant::now();
        let mut out: Vec<LockoutStatus> = Vec::new();
        for entry in self.brute_ip.iter() {
            out.push(LockoutStatus {
                kind: "ip".to_string(),
                key: entry.key().clone(),
                failures: entry.value().failures,
                locked: entry.value().is_locked(now),
                locked_until: entry.value().locked_until_wall(),
                lock_count: entry.value().lock_count,
                last_failure: entry.value().last_failure,
            });
        }
        for entry in self.brute_account.iter() {
            out.push(LockoutStatus {
                kind: "account".to_string(),
                key: entry.key().clone(),
                failures: entry.value().failures,
                locked: entry.value().is_locked(now),
                locked_until: entry.value().locked_until_wall(),
                lock_count: entry.value().lock_count,
                last_failure: entry.value().last_failure,
            });
        }
        out.sort_by(|a, b| b.last_failure.cmp(&a.last_failure));
        out
    }

    /// Manually clear a lockout / attempt record. Returns true when a
    /// record existed. Emits `security.bruteforce.unlocked`.
    pub fn unlock(&self, kind: &str, key: &str) -> bool {
        let removed = if kind == "account" {
            self.brute_account.remove(key).is_some()
        } else {
            self.brute_ip.remove(key).is_some()
        };
        if removed {
            self.hooks.emit(
                security_events::BRUTEFORCE_UNLOCKED,
                serde_json::json!({ "kind": kind, "key": key }),
            );
        }
        removed
    }

    // -- Failed-auth clusters ------------------------------------------

    fn record_failed_auth_cluster(
        &self,
        ip: Option<&str>,
        account: Option<&str>,
        settings: &SecuritySettings,
    ) {
        let ip = ip.filter(|value| !value.is_empty());
        let account = account.filter(|value| !value.is_empty());
        if ip.is_none() && account.is_none() {
            return;
        }
        let key = format!(
            "{}|{}",
            ip.unwrap_or("-"),
            account.unwrap_or("-")
        );
        let now = Utc::now();
        // Update the cluster under the entry guard, then drop the guard
        // before emitting: a hook listener may query the clusters.
        let (count, threshold) = {
            let mut cluster = self.failed_auth.entry(key.clone()).or_insert_with(|| {
                FailedAuthCluster {
                    key: key.clone(),
                    ip: ip.map(str::to_string),
                    account: account.map(str::to_string),
                    count: 0,
                    first_seen: now,
                    last_seen: now,
                    locked: false,
                }
            });
            if now
                .signed_duration_since(cluster.first_seen)
                .num_seconds()
                .unsigned_abs()
                >= settings.failed_auth_alert_window_secs
            {
                cluster.count = 0;
                cluster.first_seen = now;
            }
            cluster.count += 1;
            cluster.last_seen = now;
            cluster.locked = ip.is_some_and(|value| self.is_locked("ip", value))
                || account.is_some_and(|value| self.is_locked("account", value));
            (
                cluster.count,
                u64::from(settings.failed_auth_alert_threshold),
            )
        };
        if threshold > 0 && count % threshold == 0 {
            self.hooks.emit(
                security_events::AUTH_FAILED,
                serde_json::json!({
                    "key": key,
                    "ip": ip,
                    "account": account,
                    "count": count,
                    "threshold": threshold,
                }),
            );
        }
    }

    /// Failed-auth clusters, most recently active first.
    pub fn failed_auth_clusters(&self) -> Vec<FailedAuthCluster> {
        let mut clusters: Vec<FailedAuthCluster> = self
            .failed_auth
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        clusters.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
        clusters
    }

    // -- Rate limiting ---------------------------------------------------

    fn check_window(
        map: &DashMap<String, WindowBucket>,
        key: &str,
        limit: u32,
        window_secs: u64,
    ) -> RateDecision {
        let now = Instant::now();
        let mut bucket = map.entry(key.to_string()).or_insert_with(|| WindowBucket {
            count: 0,
            window_start: now,
            started_at: Utc::now(),
        });
        if bucket.window_start.elapsed().as_secs() >= window_secs {
            bucket.count = 0;
            bucket.window_start = now;
            bucket.started_at = Utc::now();
        }
        bucket.count += 1;
        if bucket.count > limit {
            let elapsed = bucket.window_start.elapsed().as_secs();
            RateDecision::Limited {
                retry_after_secs: window_secs.saturating_sub(elapsed).max(1),
            }
        } else {
            let elapsed = bucket.window_start.elapsed().as_secs();
            RateDecision::Allowed {
                remaining: limit.saturating_sub(bucket.count),
                reset_in_secs: window_secs.saturating_sub(elapsed).max(1),
            }
        }
    }

    /// Run the per-IP, per-user, and per-endpoint fixed-window checks.
    /// Returns the first limiting decision, or the per-IP allowance
    /// (used for the response headers) when all checks pass.
    pub fn check_rate_limits(
        &self,
        ip_key: &str,
        user_key: Option<&str>,
        endpoint_key: &str,
        settings: &SecuritySettings,
    ) -> RateDecision {
        match Self::check_window(
            &self.rate_ip,
            ip_key,
            settings.rate_limit_per_ip_requests,
            settings.rate_limit_per_ip_window_secs,
        ) {
            RateDecision::Limited { retry_after_secs } => {
                return RateDecision::Limited { retry_after_secs };
            }
            allowed @ RateDecision::Allowed { .. } => {
                if let Some(user_key) = user_key {
                    if let RateDecision::Limited { retry_after_secs } = Self::check_window(
                        &self.rate_user,
                        user_key,
                        settings.rate_limit_per_user_requests,
                        settings.rate_limit_per_user_window_secs,
                    ) {
                        return RateDecision::Limited { retry_after_secs };
                    }
                }
                if let RateDecision::Limited { retry_after_secs } = Self::check_window(
                    &self.rate_endpoint,
                    endpoint_key,
                    settings.rate_limit_per_endpoint_requests,
                    settings.rate_limit_per_endpoint_window_secs,
                ) {
                    return RateDecision::Limited { retry_after_secs };
                }
                allowed
            }
        }
    }

    /// Every tracked rate-limit window, for the status endpoint.
    pub fn rate_limit_entries(&self) -> Vec<RateLimitEntry> {
        let settings = self.settings();
        let mut out: Vec<RateLimitEntry> = Vec::new();
        let mut collect = |scope: &str,
                           map: &DashMap<String, WindowBucket>,
                           limit: u32,
                           window_secs: u64| {
            for entry in map.iter() {
                let bucket = entry.value();
                let elapsed = bucket.window_start.elapsed().as_secs();
                out.push(RateLimitEntry {
                    scope: scope.to_string(),
                    key: entry.key().clone(),
                    limit,
                    count: bucket.count,
                    window_secs,
                    window_started_at: bucket.started_at,
                    reset_in_secs: window_secs.saturating_sub(elapsed).max(1),
                });
            }
        };
        collect(
            "ip",
            &self.rate_ip,
            settings.rate_limit_per_ip_requests,
            settings.rate_limit_per_ip_window_secs,
        );
        collect(
            "user",
            &self.rate_user,
            settings.rate_limit_per_user_requests,
            settings.rate_limit_per_user_window_secs,
        );
        collect(
            "endpoint",
            &self.rate_endpoint,
            settings.rate_limit_per_endpoint_requests,
            settings.rate_limit_per_endpoint_window_secs,
        );
        out.sort_by(|a, b| b.count.cmp(&a.count));
        out
    }

    // -- Flood detection and concurrent connections ----------------------

    /// Flood check: returns the seconds to wait when the IP sent more
    /// than `flood_burst_threshold` requests inside
    /// `flood_burst_window_secs`. On a burst the IP also gains
    /// reputation points and is auto-flagged at the configured
    /// threshold. The chosen behaviour is an immediate 429 with a
    /// `challenge_required` flag in the error metadata (implemented by
    /// the rate-limit middleware), not a held request: holding would
    /// occupy a worker for the whole hold and amplify the flood.
    pub fn check_burst(&self, ip: &str, settings: &SecuritySettings) -> Option<u64> {
        let now = Instant::now();
        let mut bucket = self.burst.entry(ip.to_string()).or_insert_with(|| BurstBucket {
            count: 0,
            window_start: now,
        });
        if bucket.window_start.elapsed().as_secs() >= settings.flood_burst_window_secs {
            bucket.count = 0;
            bucket.window_start = now;
        }
        bucket.count += 1;
        if bucket.count > settings.flood_burst_threshold {
            let elapsed = bucket.window_start.elapsed().as_secs();
            let retry_after = settings.flood_burst_window_secs.saturating_sub(elapsed).max(1);
            drop(bucket);
            self.add_reputation(
                ip,
                settings.ip_reputation_burst_penalty,
                Some("request flood burst".to_string()),
                settings,
            );
            return Some(retry_after);
        }
        None
    }

    /// Current in-flight request count for one IP, after incrementing.
    pub fn inc_concurrent(&self, ip: &str) -> u64 {
        let counter = self
            .concurrent
            .entry(ip.to_string())
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .value()
            .clone();
        counter.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Decrement the in-flight request count for one IP. Saturating:
    /// a decrement without a matching increment (for example a slot
    /// released twice, or a counter left over from an older build)
    /// never wraps the counter to `u64::MAX`.
    pub fn dec_concurrent(&self, ip: &str) {
        if let Some(counter) = self.concurrent.get(ip) {
            let _ = counter.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                if value == 0 {
                    None
                } else {
                    Some(value - 1)
                }
            });
        }
    }

    // -- IP reputation ---------------------------------------------------

    /// Add reputation points to an IP. At the configured threshold the
    /// IP is auto-flagged and `security.ip.flagged` is emitted (once,
    /// on the transition to flagged).
    pub fn add_reputation(
        &self,
        ip: &str,
        points: i32,
        reason: Option<String>,
        settings: &SecuritySettings,
    ) {
        // Update under the entry guard, then drop it before emitting:
        // a hook listener may query the reputation map.
        let flag_event: Option<(i32, Option<String>)> = {
            let mut entry = self.reputation.entry(ip.to_string()).or_insert_with(|| {
                ReputationEntry {
                    ip: ip.to_string(),
                    score: 0,
                    flagged: false,
                    reason: None,
                    updated_at: Utc::now(),
                }
            });
            entry.score = entry.score.saturating_add(points);
            entry.updated_at = Utc::now();
            if let Some(reason) = reason {
                entry.reason = Some(reason.clone());
            }
            if !entry.flagged && entry.score >= settings.ip_reputation_flag_threshold {
                entry.flagged = true;
                Some((entry.score, entry.reason.clone()))
            } else {
                None
            }
        };
        if let Some((score, reason)) = flag_event {
            self.hooks.emit(
                security_events::IP_FLAGGED,
                serde_json::json!({
                    "ip": ip,
                    "score": score,
                    "reason": reason,
                    "automatic": true,
                }),
            );
        }
    }

    /// Manually flag an IP with a reason. Emits `security.ip.flagged`.
    /// The score is raised to at least the flag threshold so the flag
    /// survives later score changes.
    pub fn flag_ip(&self, ip: &str, reason: Option<String>) -> Result<ReputationEntry, String> {
        let parsed: IpAddr = ip
            .trim()
            .parse()
            .map_err(|_| format!("\"{ip}\" is not a valid IP address"))?;
        let settings = self.settings();
        let snapshot = {
            let mut entry = self
                .reputation
                .entry(parsed.to_string())
                .or_insert_with(|| ReputationEntry {
                    ip: parsed.to_string(),
                    score: 0,
                    flagged: false,
                    reason: None,
                    updated_at: Utc::now(),
                });
            entry.score = entry.score.max(settings.ip_reputation_flag_threshold);
            entry.flagged = true;
            entry.reason = reason.clone();
            entry.updated_at = Utc::now();
            entry.clone()
        };
        self.hooks.emit(
            security_events::IP_FLAGGED,
            serde_json::json!({
                "ip": parsed.to_string(),
                "score": snapshot.score,
                "reason": reason,
                "automatic": false,
            }),
        );
        Ok(snapshot)
    }

    /// Manually unflag an IP and reset its score. Emits
    /// `security.ip.unflagged`.
    pub fn unflag_ip(&self, ip: &str) -> Result<ReputationEntry, String> {
        let parsed: IpAddr = ip
            .trim()
            .parse()
            .map_err(|_| format!("\"{ip}\" is not a valid IP address"))?;
        let snapshot = {
            let mut entry = self
                .reputation
                .entry(parsed.to_string())
                .or_insert_with(|| ReputationEntry {
                    ip: parsed.to_string(),
                    score: 0,
                    flagged: false,
                    reason: None,
                    updated_at: Utc::now(),
                });
            entry.flagged = false;
            entry.score = 0;
            entry.reason = None;
            entry.updated_at = Utc::now();
            entry.clone()
        };
        self.hooks.emit(
            security_events::IP_UNFLAGGED,
            serde_json::json!({ "ip": parsed.to_string() }),
        );
        Ok(snapshot)
    }

    /// Reputation records, highest score first.
    pub fn reputation_entries(&self) -> Vec<ReputationEntry> {
        let mut entries: Vec<ReputationEntry> = self
            .reputation
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        entries.sort_by(|a, b| b.score.cmp(&a.score));
        entries
    }

    // -- Helpers for the middleware --------------------------------------

    /// True when the path is exempt from rate limiting and the
    /// blocklist (health probe and ACME challenge by default).
    pub fn is_exempt_path(&self, path: &str, settings: &SecuritySettings) -> bool {
        settings
            .rate_limit_exempt_paths
            .iter()
            .any(|prefix| path == prefix || path.starts_with(prefix.as_str()))
    }

    /// Body-size limit in bytes for a path: the upload limit when the
    /// path matches an upload prefix, otherwise the default limit.
    pub fn body_limit_for_path(&self, path: &str, settings: &SecuritySettings) -> (u64, &'static str) {
        if settings
            .upload_path_prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix.as_str()))
        {
            (settings.max_body_bytes_upload, "upload")
        } else {
            (settings.max_body_bytes_default, "default")
        }
    }

    /// Counts used by the settings overview endpoint.
    pub fn overview_counts(&self) -> HashMap<String, u64> {
        let now = Instant::now();
        let mut counts = HashMap::new();
        counts.insert("allowlist_entries".to_string(), self.allowlist.len() as u64);
        counts.insert("blocklist_entries".to_string(), self.blocklist.len() as u64);
        counts.insert(
            "locked_ips".to_string(),
            self.brute_ip
                .iter()
                .filter(|entry| entry.value().is_locked(now))
                .count() as u64,
        );
        counts.insert(
            "locked_accounts".to_string(),
            self.brute_account
                .iter()
                .filter(|entry| entry.value().is_locked(now))
                .count() as u64,
        );
        counts.insert(
            "flagged_ips".to_string(),
            self.reputation
                .iter()
                .filter(|entry| entry.value().flagged)
                .count() as u64,
        );
        counts.insert(
            "failed_auth_clusters".to_string(),
            self.failed_auth.len() as u64,
        );
        counts
    }
}

impl Default for HardeningState {
    fn default() -> Self {
        Self::new(SecuritySettings::default(), Arc::new(HookBus::new()))
    }
}

// ---------------------------------------------------------------------------
// Shared instance
// ---------------------------------------------------------------------------

static SHARED_STATE: RwLock<Option<Arc<HardeningState>>> = RwLock::new(None);

/// Install the shared hardening state, normally the config-derived one
/// built by the server at startup. Replaces any state installed before,
/// so a reload can swap in fresh settings.
pub fn install_shared_state(state: Arc<HardeningState>) {
    *SHARED_STATE
        .write()
        .expect("hardening shared state lock poisoned") = Some(state);
}

/// The shared hardening state: the installed one, or a lazily created
/// default when the server has not installed one yet.
pub fn shared_state() -> Arc<HardeningState> {
    if let Some(state) = SHARED_STATE
        .read()
        .expect("hardening shared state lock poisoned")
        .clone()
    {
        return state;
    }
    let state = Arc::new(HardeningState::default());
    *SHARED_STATE
        .write()
        .expect("hardening shared state lock poisoned") = Some(state.clone());
    state
}

// ---------------------------------------------------------------------------
// Request helpers shared by the middleware
// ---------------------------------------------------------------------------

/// Client IP for a request: the first forwarded address reported by
/// Actix `ConnectionInfo` when present, otherwise the peer address.
///
/// WARNING: Actix trusts `X-Forwarded-For` from any client, so this
/// helper is spoofable by a direct client and must NOT be used for
/// enforcement (blocklist, rate limiting, brute-force keys). It is
/// kept for display / logging callers that already use it.
/// Enforcement paths must use `trusted_client_ip` instead.
pub fn client_ip(req: &ServiceRequest) -> Option<IpAddr> {
    if let Some(real_ip) = req.connection_info().realip_remote_addr() {
        if let Ok(ip) = real_ip.parse::<IpAddr>() {
            return Some(ip);
        }
    }
    req.peer_addr().map(|address| address.ip())
}

/// Client IP for enforcement, honouring `X-Forwarded-For` only when
/// the TCP peer is a configured trusted proxy
/// (`security.trusted_proxies`). With no trusted proxies configured
/// (the default) the peer address is always the client IP, so a
/// direct client can never spoof the IP the blocklist, rate limiter,
/// and brute-force engine enforce on. When the peer is trusted, the
/// first `X-Forwarded-For` entry is the client IP; an unparseable or
/// absent entry falls back to the peer address (fail closed: never
/// invent an address).
pub fn trusted_client_ip(
    peer: Option<std::net::SocketAddr>,
    headers: &actix_web::http::header::HeaderMap,
    settings: &SecuritySettings,
) -> Option<IpAddr> {
    let peer_ip = peer.map(|address| address.ip())?;
    if settings.trusted_proxies.is_empty() {
        return Some(peer_ip);
    }
    let trusted = settings.trusted_proxies.iter().any(|entry| {
        parse_cidr(entry).is_ok_and(|range| range.contains(peer_ip))
    });
    if !trusted {
        return Some(peer_ip);
    }
    let forwarded = headers
        .get(header::X_FORWARDED_FOR)
        .and_then(|value| value.to_str().ok())?;
    let first = forwarded.split(',').next().map(str::trim).unwrap_or("");
    match first.parse::<IpAddr>() {
        Ok(ip) => Some(ip),
        Err(_) => Some(peer_ip),
    }
}

/// `trusted_client_ip` for a middleware `ServiceRequest`.
pub fn trusted_client_ip_for_request(
    req: &ServiceRequest,
    settings: &SecuritySettings,
) -> Option<IpAddr> {
    trusted_client_ip(req.peer_addr(), req.headers(), settings)
}

/// `trusted_client_ip` for a handler `HttpRequest`.
pub fn trusted_client_ip_for_http_request(
    req: &actix_web::HttpRequest,
    settings: &SecuritySettings,
) -> Option<IpAddr> {
    trusted_client_ip(req.peer_addr(), req.headers(), settings)
}

/// One held concurrent-connection slot. Dropping the slot releases
/// it, so a request that is dropped mid-flight (client disconnect)
/// can never leak a slot and permanently throttle its IP.
pub struct ConcurrentSlot {
    state: Arc<HardeningState>,
    ip: String,
}

impl ConcurrentSlot {
    /// Take one slot for `ip` and return the in-flight count after
    /// incrementing, with the guard that releases it on drop.
    pub fn acquire(state: &Arc<HardeningState>, ip: &str) -> (u64, Self) {
        let count = state.inc_concurrent(ip);
        (
            count,
            Self {
                state: Arc::clone(state),
                ip: ip.to_string(),
            },
        )
    }
}

impl Drop for ConcurrentSlot {
    fn drop(&mut self) {
        self.state.dec_concurrent(&self.ip);
    }
}

/// True when the path is the ACME HTTP-01 challenge route.
pub fn is_acme_challenge_path(path: &str) -> bool {
    path.starts_with("/.well-known/acme-challenge/")
}

/// Normalize a request path for the per-endpoint rate-limit key:
/// numeric segments and UUID segments become `{id}` so one resource
/// with many ids does not create one window per id.
pub fn normalize_endpoint_path(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            if segment.is_empty() {
                segment.to_string()
            } else if segment.chars().all(|c| c.is_ascii_digit())
                || segment.parse::<uuid::Uuid>().is_ok()
            {
                "{id}".to_string()
            } else {
                segment.to_string()
            }
        })
        .collect::<Vec<String>>()
        .join("/")
}

/// Per-user rate-limit key from the Authorization header. The raw
/// token is never stored: the key is a truncated base64url SHA-256 of
/// the header value, so one bearer token gets one window without the
/// token itself ever entering the rate-limit store or any log.
pub fn user_key(req: &ServiceRequest) -> Option<String> {
    let header = req.headers().get(header::AUTHORIZATION)?;
    let value = header.to_str().ok()?;
    if value.trim().is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&digest[..8]);
    Some(format!("user:{encoded}"))
}

/// Build a standard-envelope 429 response with `Retry-After` and the
/// `X-RateLimit-*` headers.
fn rate_limited_response(
    limit: u32,
    remaining: u32,
    reset_in_secs: u64,
    retry_after_secs: u64,
    challenge_required: bool,
) -> HttpResponse {
    let mut error = UnifiedError::new("Rate limit exceeded", 429)
        .with_metadata("retry_after_secs", serde_json::json!(retry_after_secs))
        .with_metadata("limit", serde_json::json!(limit))
        .with_metadata("remaining", serde_json::json!(remaining))
        .with_metadata("reset_in_secs", serde_json::json!(reset_in_secs));
    if challenge_required {
        error = error.with_metadata("challenge_required", serde_json::json!(true));
    }
    HttpResponse::TooManyRequests()
        .insert_header(("Retry-After", retry_after_secs.to_string()))
        .insert_header(("X-RateLimit-Limit", limit.to_string()))
        .insert_header(("X-RateLimit-Remaining", remaining.to_string()))
        .insert_header((
            "X-RateLimit-Reset",
            (Utc::now().timestamp() + reset_in_secs as i64).to_string(),
        ))
        .json(json::error_json(&error, false))
}

fn set_header(
    headers: &mut actix_web::http::header::HeaderMap,
    name: header::HeaderName,
    value: &str,
) {
    if value.is_empty() {
        return;
    }
    if let Ok(header_value) = HeaderValue::from_str(value) {
        headers.insert(name, header_value);
    }
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// Adds the configured security headers to every response. Runs first
/// (priority 1000) so error responses short-circuited by the later
/// hardening middleware still carry the headers.
#[derive(Clone)]
pub struct SecurityHeadersMiddleware {
    state: Arc<HardeningState>,
}

impl std::fmt::Debug for SecurityHeadersMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecurityHeadersMiddleware")
    }
}

impl SecurityHeadersMiddleware {
    pub fn new(state: Arc<HardeningState>) -> Self {
        Self { state }
    }
}

impl Middleware for SecurityHeadersMiddleware {
    fn priority(&self) -> isize {
        1000
    }

    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult {
        let state = self.state.clone();
        Box::pin(async move {
            let mut res = next(req).await?;
            let settings = state.settings();
            if settings.enabled {
                let headers = res.headers_mut();
                set_header(
                    headers,
                    header::STRICT_TRANSPORT_SECURITY,
                    &settings.headers_hsts,
                );
                set_header(
                    headers,
                    header::X_CONTENT_TYPE_OPTIONS,
                    &settings.headers_content_type_options,
                );
                set_header(
                    headers,
                    header::X_FRAME_OPTIONS,
                    &settings.headers_frame_options,
                );
                set_header(
                    headers,
                    header::REFERRER_POLICY,
                    &settings.headers_referrer_policy,
                );
                set_header(
                    headers,
                    header::CONTENT_SECURITY_POLICY,
                    &settings.headers_content_security_policy,
                );
                set_header(
                    headers,
                    header::PERMISSIONS_POLICY,
                    &settings.headers_permissions_policy,
                );
            }
            Ok(res)
        })
    }
}

/// Rejects blocklisted IPs with 403. Allowlisted IPs are left alone
/// here (the rate limiter gives them their bypass). The ACME challenge
/// route is exempt: a blocked challenge would break certificate
/// issuance and renewal.
#[derive(Clone)]
pub struct IpFilterMiddleware {
    state: Arc<HardeningState>,
}

impl std::fmt::Debug for IpFilterMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IpFilterMiddleware")
    }
}

impl IpFilterMiddleware {
    pub fn new(state: Arc<HardeningState>) -> Self {
        Self { state }
    }
}

impl Middleware for IpFilterMiddleware {
    fn priority(&self) -> isize {
        900
    }

    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult {
        let state = self.state.clone();
        let settings = state.settings();
        if !settings.enabled {
            return Box::pin(async move { next(req).await });
        }
        let path = req.path().to_string();
        if is_acme_challenge_path(&path) || state.is_exempt_path(&path, &settings) {
            return Box::pin(async move { next(req).await });
        }
        if let Some(ip) = trusted_client_ip_for_request(&req, &settings) {
            if state.is_blocklisted(ip) {
                let error = UnifiedError::forbidden("Request rejected: IP address is blocked")
                    .with_metadata("ip", serde_json::json!(ip.to_string()))
                    .with_metadata("list", serde_json::json!("block"));
                let response = HttpResponse::Forbidden().json(json::error_json(&error, false));
                return Box::pin(async move { Ok(req.into_response(response)) });
            }
        }
        Box::pin(async move { next(req).await })
    }
}

/// Fixed-window rate limiting per IP, per user, and per endpoint, plus
/// flood-burst detection and per-IP concurrent-connection limits.
/// Exceeding any limit returns 429 in the standard error envelope
/// with `Retry-After` and `X-RateLimit-*` headers. Allowlisted IPs
/// bypass this middleware entirely. The ACME challenge route and the
/// other exempt paths bypass it as well.
#[derive(Clone)]
pub struct RateLimitMiddleware {
    state: Arc<HardeningState>,
}

impl std::fmt::Debug for RateLimitMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RateLimitMiddleware")
    }
}

impl RateLimitMiddleware {
    pub fn new(state: Arc<HardeningState>) -> Self {
        Self { state }
    }
}

impl Middleware for RateLimitMiddleware {
    fn priority(&self) -> isize {
        800
    }

    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult {
        let state = self.state.clone();
        let settings = state.settings();
        if !settings.enabled || !settings.rate_limit_enabled {
            return Box::pin(async move { next(req).await });
        }
        let path = req.path().to_string();
        if is_acme_challenge_path(&path) || state.is_exempt_path(&path, &settings) {
            return Box::pin(async move { next(req).await });
        }
        let ip = match trusted_client_ip_for_request(&req, &settings) {
            Some(ip) => ip,
            None => return Box::pin(async move { next(req).await }),
        };
        if state.is_allowlisted(ip) {
            return Box::pin(async move { next(req).await });
        }
        let ip_text = ip.to_string();

        // Flood burst: reject immediately with a challenge flag (see
        // `check_burst` for why the request is not held instead).
        if let Some(retry_after) = state.check_burst(&ip_text, &settings) {
            state.hooks.emit(
                security_events::RATE_LIMITED,
                serde_json::json!({
                    "ip": ip_text,
                    "path": path,
                    "scope": "flood",
                    "retry_after_secs": retry_after,
                }),
            );
            let response = rate_limited_response(
                settings.flood_burst_threshold,
                0,
                retry_after,
                retry_after,
                true,
            );
            return Box::pin(async move { Ok(req.into_response(response)) });
        }

        let user = user_key(&req);
        let endpoint_key = format!(
            "{}:{}",
            req.method(),
            normalize_endpoint_path(&path)
        );
        let decision = state.check_rate_limits(
            &format!("ip:{ip_text}"),
            user.as_deref(),
            &endpoint_key,
            &settings,
        );
        let (limit, remaining, reset_in_secs) = match decision {
            RateDecision::Allowed {
                remaining,
                reset_in_secs,
            } => (
                settings.rate_limit_per_ip_requests,
                remaining,
                reset_in_secs,
            ),
            RateDecision::Limited { retry_after_secs } => {
                state.hooks.emit(
                    security_events::RATE_LIMITED,
                    serde_json::json!({
                        "ip": ip_text,
                        "path": path,
                        "scope": "rate_limit",
                        "retry_after_secs": retry_after_secs,
                    }),
                );
                let response = rate_limited_response(
                    settings.rate_limit_per_ip_requests,
                    0,
                    retry_after_secs,
                    retry_after_secs,
                    false,
                );
                return Box::pin(async move { Ok(req.into_response(response)) });
            }
        };

        // Concurrent-connection limit: counted only around the inner
        // call so a rejected request never occupies a slot. The slot
        // is a drop guard, so a request dropped mid-flight (client
        // disconnect) releases it instead of leaking the count.
        let (in_flight, slot) = ConcurrentSlot::acquire(&state, &ip_text);
        if in_flight > u64::from(settings.max_concurrent_per_ip) {
            drop(slot);
            let retry_after = 1u64;
            state.hooks.emit(
                security_events::RATE_LIMITED,
                serde_json::json!({
                    "ip": ip_text,
                    "path": path,
                    "scope": "concurrent",
                    "retry_after_secs": retry_after,
                }),
            );
            let response = rate_limited_response(
                settings.max_concurrent_per_ip,
                0,
                retry_after,
                retry_after,
                false,
            );
            return Box::pin(async move { Ok(req.into_response(response)) });
        }

        Box::pin(async move {
            let _slot = slot;
            let mut res = next(req).await;
            if let Ok(res) = &mut res {
                let headers = res.headers_mut();
                headers.insert(
                    "X-RateLimit-Limit",
                    HeaderValue::from_str(&limit.to_string())
                        .unwrap_or(HeaderValue::from_static("0")),
                );
                headers.insert(
                    "X-RateLimit-Remaining",
                    HeaderValue::from_str(&remaining.to_string())
                        .unwrap_or(HeaderValue::from_static("0")),
                );
                headers.insert(
                    "X-RateLimit-Reset",
                    HeaderValue::from_str(&(Utc::now().timestamp() + reset_in_secs as i64).to_string())
                        .unwrap_or(HeaderValue::from_static("0")),
                );
            }
            res
        })
    }
}

/// Enforces the configured maximum request body size per route group
/// from the `Content-Length` header. Requests without a
/// `Content-Length` (chunked bodies) pass this check; the Actix
/// payload limits bound them instead. Oversize requests get 413 in
/// the standard error envelope.
#[derive(Clone)]
pub struct RequestSizeMiddleware {
    state: Arc<HardeningState>,
}

impl std::fmt::Debug for RequestSizeMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RequestSizeMiddleware")
    }
}

impl RequestSizeMiddleware {
    pub fn new(state: Arc<HardeningState>) -> Self {
        Self { state }
    }
}

impl Middleware for RequestSizeMiddleware {
    fn priority(&self) -> isize {
        700
    }

    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult {
        let state = self.state.clone();
        let settings = state.settings();
        if !settings.enabled {
            return Box::pin(async move { next(req).await });
        }
        let path = req.path().to_string();
        let content_length = req
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        if let Some(content_length) = content_length {
            let (limit, group) = state.body_limit_for_path(&path, &settings);
            if content_length > limit {
                let error = UnifiedError::new("Request body is too large", 413)
                    .with_metadata("content_length", serde_json::json!(content_length))
                    .with_metadata("max_bytes", serde_json::json!(limit))
                    .with_metadata("limit_group", serde_json::json!(group));
                let response =
                    HttpResponse::PayloadTooLarge().json(json::error_json(&error, false));
                return Box::pin(async move { Ok(req.into_response(response)) });
            }
        }
        Box::pin(async move { next(req).await })
    }
}
