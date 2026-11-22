// Domain Verification module — ownership verification for custom domain
// aliases through admin-determined DNS records.
//
// A tenant adds a custom domain alias on the user dashboard (main site);
// this module issues a verification token and tells the dashboard the
// EXACT DNS record to create. The tenant creates the record, then the
// dashboard calls verify (or a scheduler does): the server checks DNS
// with the Network module's `DnsVerifier` and, on success, emits
// `domain.verification.completed` so the SaaS Domains module can flip
// the alias status. Unverified domains never serve: resolvers and
// middleware ask this module through the
// `domain.verification.status_requested` hook event instead of trusting
// any stored flag.
//
// This module is API-only. It serves no tenant-facing page: the
// tenant subdomain serves services only, while the settings and the
// verify action live on the user dashboard (main site).
//
// Fully config-driven (no hard-coded verification behaviour):
// the record format is DETERMINED BY ADMIN, not by this code. The
// admin settings choose the record type (TXT or CNAME), the host and
// value templates, the token length and alphabet, the DNS timeout,
// the retry policy, the token lifetime, and the rate-limit window.
// The `DEFAULT_SETTINGS` below are clearly-marked CONFIGURABLE
// DEFAULTS only: they seed the settings store at startup and are
// replaced wholesale by whatever the admin saves. Changing the
// settings does NOT invalidate already-verified domains; the new
// format applies only to tokens issued after the change (tokens
// already issued keep the format they were issued with, because the
// rendered host/value are stored on the token at issuance time).
// The status vocabulary (`verified` / `pending` / `failed` /
// `unknown`) is the one exception: those four strings are part of
// the inter-module event contract fixed by the platform spec, so
// they are literal constants, not settings. Everything about WHEN a
// domain moves between those statuses (timeouts, attempts, windows)
// comes from settings.
//
// Security:
//   - Tokens are cryptographically random (OS RNG via `rand`) and
//     stored only as SHA-256 hashes. The raw token is returned
//     exactly once, in the issuance response; it is never stored,
//     never logged, and never included in any hook event.
//   - Every endpoint requires caller identity (`X-User-Id` and
//     `X-Role` headers) and a capability checked through the
//     `role.check` hook, failing closed (no answer means not
//     allowed), exactly like the properties module.
//   - Verify attempts are rate-limited per domain: once
//     `max_attempts` attempts have been recorded inside
//     `rate_limit_window_seconds`, further verify calls fail closed
//     with HTTP 429.
//   - `verified` is never reported unless a successful DNS check is
//     recorded against the CURRENT token for the domain.
//
// Persistence: the live data lives in the in-memory DashMap stores
// below (authoritative, following the network_tools precedent). The
// migration `2026-11-22-000072_domain_verification` creates the
// matching PostgreSQL tables as persistence targets; the module does
// not read them yet (the schema registry is owned by the integrator).
//
// Events emitted on the hook bus:
//   - `domain.verification.token_issued` — a token was issued
//     (never carries the raw token).
//   - `domain.verification.attempted` — one DNS check attempt.
//   - `domain.verification.completed` — verification succeeded;
//     payload `{"alias_id", "domain", "tenant_id", "company_id",
//     "status": "verified", "record_found", "verified_at"}`.
//   - `domain.verification.failed` — attempts exhausted; payload
//     `{"alias_id", "domain", "tenant_id", "company_id",
//     "status": "failed", "record_found", "error", "attempts",
//     "verified_at": null}`.
//   - `domain.verification.settings_updated` — admin settings
//     changed (audit event).
// Events this module listens to:
//   - `domain.verification.status_requested` — payload
//     `{"domain"}`; answers with `{"domain", "status",
//     "verified_at"}` (no result when the domain is unknown).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
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

// ---------------------------------------------------------------------------
// Hook event names (inter-module contract).
// ---------------------------------------------------------------------------

/// Emitted when a verification token is issued. Never carries the raw token.
pub const EVENT_TOKEN_ISSUED: &str = "domain.verification.token_issued";
/// Emitted for every single DNS check attempt.
pub const EVENT_ATTEMPTED: &str = "domain.verification.attempted";
/// Emitted when verification succeeds. The SaaS Domains module listens
/// to this event to flip the alias status to verified.
pub const EVENT_COMPLETED: &str = "domain.verification.completed";
/// Emitted when verification attempts are exhausted without success.
pub const EVENT_FAILED: &str = "domain.verification.failed";
/// Emitted when an admin changes the verification settings (audit).
pub const EVENT_SETTINGS_UPDATED: &str = "domain.verification.settings_updated";
/// Answered by this module: payload `{"domain"}`; result
/// `{"domain", "status", "verified_at"}` (no result when unknown).
pub const EVENT_STATUS_REQUESTED: &str = "domain.verification.status_requested";

// ---------------------------------------------------------------------------
// Verification statuses.
//
// Contract vocabulary (see the module docs): these four strings are
// fixed by the platform event contract, so resolvers, middleware, and
// the SaaS Domains module can match on them. WHEN a domain moves
// between them is driven entirely by admin settings.
// ---------------------------------------------------------------------------

/// DNS check matched the expected record for the current token.
pub const STATUS_VERIFIED: &str = "verified";
/// A current token exists and verification has not finished yet.
pub const STATUS_PENDING: &str = "pending";
/// Attempts were exhausted (or the token expired) without a match.
pub const STATUS_FAILED: &str = "failed";
/// No token is known for the domain at all.
pub const STATUS_UNKNOWN: &str = "unknown";

/// All four contract statuses, in lifecycle order.
pub const ALL_STATUSES: [&str; 4] = [
    STATUS_VERIFIED,
    STATUS_PENDING,
    STATUS_FAILED,
    STATUS_UNKNOWN,
];

// Capability names checked through the `role.check` hook (fail-closed).
const PERMISSION_MANAGE: &str = "domain_verification.manage";
const PERMISSION_ISSUE: &str = "domain_verification.issue";
const PERMISSION_VERIFY: &str = "domain_verification.verify";
const PERMISSION_READ: &str = "domain_verification.read";

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

static SETTINGS: OnceLock<RwLock<VerificationSettings>> = OnceLock::new();

fn settings_store() -> &'static RwLock<VerificationSettings> {
    SETTINGS.get_or_init(|| RwLock::new(default_settings()))
}

/// Snapshot of the current admin settings. All verification logic
/// reads settings through here — never through a literal.
fn current_settings() -> VerificationSettings {
    settings_store()
        .read()
        .expect("verification settings lock poisoned")
        .clone()
}

// ---------------------------------------------------------------------------
// Tokens and attempts (in-memory stores; authoritative).
// ---------------------------------------------------------------------------

/// One issued verification token. The raw token is NEVER stored:
/// only its SHA-256 hex digest (`token_hash`) is kept. The rendered
/// `host` / `record_type` / `expected_value` are snapshotted at
/// issuance so a later admin settings change never rewrites what an
/// already-issued token must match.
#[derive(Debug, Clone, Serialize)]
pub struct VerificationToken {
    pub id: Uuid,
    pub domain: String,
    #[serde(skip_serializing)]
    pub alias_id: Option<Uuid>,
    #[serde(skip_serializing)]
    pub tenant_id: Option<Uuid>,
    #[serde(skip_serializing)]
    pub company_id: Option<Uuid>,
    /// SHA-256 hex digest of the raw token. Never exposed via the API.
    #[serde(skip_serializing)]
    pub token_hash: String,
    /// Record type the token was issued for (`"txt"` / `"cname"`).
    pub record_type: String,
    /// Exact host the tenant must publish the record at.
    pub host: String,
    /// Exact value the record must hold (contains the raw token;
    /// stored so verification compares against issuance-time
    /// settings, not current settings).
    #[serde(skip_serializing)]
    pub expected_value: String,
    /// Settings version the token was issued under.
    pub settings_version: u64,
    pub status: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
}

/// Public view of a token: metadata only. The hash and the expected
/// value are never exposed after issuance.
#[derive(Debug, Clone, Serialize)]
pub struct VerificationTokenView {
    pub id: Uuid,
    pub domain: String,
    pub record_type: String,
    pub host: String,
    pub settings_version: u64,
    pub status: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
}

impl From<&VerificationToken> for VerificationTokenView {
    fn from(token: &VerificationToken) -> Self {
        Self {
            id: token.id,
            domain: token.domain.clone(),
            record_type: token.record_type.clone(),
            host: token.host.clone(),
            settings_version: token.settings_version,
            status: token.status.clone(),
            issued_at: token.issued_at,
            expires_at: token.expires_at,
            verified_at: token.verified_at,
        }
    }
}

/// One DNS check attempt inside a verify call.
#[derive(Debug, Clone, Serialize)]
pub struct VerificationAttempt {
    pub id: Uuid,
    pub token_id: Uuid,
    pub domain: String,
    pub alias_id: Option<Uuid>,
    pub attempt_number: u32,
    /// `"verified"` / `"pending"` / `"failed"` for this attempt.
    pub result: String,
    /// The exact DNS record that matched, when one did.
    pub record_found: Option<String>,
    /// What was looked up (host, type, expected-value prefix).
    pub looked_up: Value,
    pub error: Option<String>,
    pub checked_at: DateTime<Utc>,
}

static TOKENS: OnceLock<DashMap<Uuid, VerificationToken>> = OnceLock::new();
static ATTEMPTS: OnceLock<DashMap<Uuid, VerificationAttempt>> = OnceLock::new();

fn tokens_store() -> &'static DashMap<Uuid, VerificationToken> {
    TOKENS.get_or_init(DashMap::new)
}

fn attempts_store() -> &'static DashMap<Uuid, VerificationAttempt> {
    ATTEMPTS.get_or_init(DashMap::new)
}

// ---------------------------------------------------------------------------
// Helpers: identity, capabilities, templates, tokens.
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the properties module.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the properties module).
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Normalise and validate a domain name to its ASCII (punycode) form.
fn normalize_domain(raw: &str) -> UnifiedResult<String> {
    let canonical = DnsVerifier::normalize_domain_input(raw)?;
    let syntax = DnsVerifier::check_domain_syntax(&canonical);
    if !syntax.valid {
        return Err(UnifiedError::bad_request(format!(
            "Invalid domain: {}",
            syntax.issues.join("; ")
        )));
    }
    Ok(syntax.punycode.unwrap_or(canonical))
}

/// Render the lookup host from the admin host template. A template
/// containing `{domain}` is substituted verbatim; a bare label is
/// looked up as `<label>.<domain>`.
fn render_host(host_template: &str, domain: &str) -> String {
    if host_template.contains("{domain}") {
        host_template.replace("{domain}", domain)
    } else {
        let label = host_template.trim().trim_end_matches('.');
        if label.is_empty() {
            domain.to_string()
        } else {
            format!("{label}.{domain}")
        }
    }
}

/// Render the expected record value from the admin value template.
fn render_value(value_template: &str, token: &str) -> String {
    value_template.replace("{token}", token)
}

/// Generate a cryptographically random token from the admin alphabet
/// and length (OS RNG via `rand`).
fn generate_token(settings: &VerificationSettings) -> String {
    let mut rng = rand::rng();
    let alphabet: Vec<char> = settings.token_alphabet.chars().collect();
    (0..settings.token_length)
        .map(|_| {
            let index = rng.random_range(0..alphabet.len());
            alphabet[index]
        })
        .collect()
}

/// SHA-256 hex digest of a token. Only the digest is ever stored.
fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// First characters of a value, for "expected value prefix" reporting
/// without leaking the full token-bearing value.
fn value_prefix(value: &str) -> String {
    let prefix: String = value.chars().take(12).collect();
    if value.chars().count() > 12 {
        format!("{prefix}...")
    } else {
        prefix
    }
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

/// The current (latest, non-superseded, non-revoked) token for a
/// domain, optionally scoped to one alias. A token is "current" when
/// it is the newest issued token for the domain/alias that has not
/// been superseded by a newer issuance or revoked.
fn current_token_for(domain: &str, alias_id: Option<Uuid>) -> Option<VerificationToken> {
    let mut candidates: Vec<VerificationToken> = tokens_store()
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|token| token.domain == domain)
        .filter(|token| alias_id.map_or(true, |alias| token.alias_id == Some(alias)))
        .filter(|token| token.status != "superseded" && token.status != "revoked")
        .collect();
    candidates.sort_by(|a, b| b.issued_at.cmp(&a.issued_at));
    candidates.into_iter().next()
}

/// Status of a domain from its CURRENT token only. Never returns
/// `verified` unless a successful DNS check is recorded for the
/// current token (a verified token whose successor was issued, or an
/// expired token, does not count).
fn domain_status(domain: &str) -> (String, Option<DateTime<Utc>>) {
    let Some(token) = current_token_for(domain, None) else {
        return (STATUS_UNKNOWN.to_string(), None);
    };
    let now = Utc::now();
    if token.status == STATUS_VERIFIED {
        if let Some(verified_at) = token.verified_at {
            return (STATUS_VERIFIED.to_string(), Some(verified_at));
        }
    }
    if token.status == STATUS_PENDING && token.expires_at <= now {
        return (STATUS_FAILED.to_string(), None);
    }
    (token.status.clone(), token.verified_at)
}

/// Attempts recorded for a domain inside the last `window_seconds`.
fn attempts_in_window(domain: &str, window_seconds: i64) -> usize {
    let cutoff = Utc::now() - chrono::Duration::seconds(window_seconds);
    attempts_store()
        .iter()
        .filter(|entry| entry.value().domain == domain && entry.value().checked_at >= cutoff)
        .count()
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct UpdateSettingsRequest {
    #[serde(default)]
    record_type: Option<String>,
    #[serde(default)]
    host_template: Option<String>,
    #[serde(default)]
    value_template: Option<String>,
    #[serde(default)]
    token_length: Option<usize>,
    #[serde(default)]
    token_alphabet: Option<String>,
    #[serde(default)]
    verify_timeout_seconds: Option<u64>,
    #[serde(default)]
    max_attempts: Option<u32>,
    #[serde(default)]
    retry_delay_seconds: Option<u64>,
    #[serde(default)]
    token_ttl_seconds: Option<i64>,
    #[serde(default)]
    rate_limit_window_seconds: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct IssueTokenRequest {
    domain: String,
    #[serde(default)]
    alias_id: Option<Uuid>,
    #[serde(default)]
    tenant_id: Option<Uuid>,
    #[serde(default)]
    company_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct VerifyRequest {
    domain: String,
    #[serde(default)]
    alias_id: Option<Uuid>,
    #[serde(default)]
    tenant_id: Option<Uuid>,
    #[serde(default)]
    company_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct StatusQuery {
    domain: String,
}

/// Issuance response: the EXACT DNS record the user dashboard must
/// display, plus the raw token exactly once. Nothing in this payload
/// is stored anywhere else.
#[derive(Debug, Serialize)]
struct TokenIssuance {
    token_id: Uuid,
    domain: String,
    alias_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
    company_id: Option<Uuid>,
    record_type: String,
    host: String,
    value: String,
    /// The raw token, shown exactly once. It is never stored.
    token: String,
    settings_version: u64,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    status: String,
}

/// Outcome of one verify call: the result, the exact record found
/// (or an explicit "not found" with what was looked up), and the
/// attempt history summary.
#[derive(Debug, Serialize)]
struct VerifyOutcome {
    domain: String,
    alias_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
    company_id: Option<Uuid>,
    status: String,
    record_type: String,
    host: String,
    record_found: Option<String>,
    looked_up: Value,
    attempts_made: u32,
    verified_at: Option<DateTime<Utc>>,
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn get_settings(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationSettings> = (|| {
        let who = caller(&req)?;
        require_permission(&ctx, &who.role, PERMISSION_MANAGE)?;
        Ok(current_settings())
    })();
    json::respond(result, debug)
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationSettings> = (|| {
        let who = caller(&req)?;
        require_permission(&ctx, &who.role, PERMISSION_MANAGE)?;
        let mut next = current_settings();
        if let Some(value) = &body.record_type {
            next.record_type = value.trim().to_lowercase();
        }
        if let Some(value) = &body.host_template {
            next.host_template = value.trim().to_string();
        }
        if let Some(value) = &body.value_template {
            next.value_template = value.trim().to_string();
        }
        if let Some(value) = body.token_length {
            next.token_length = value;
        }
        if let Some(value) = &body.token_alphabet {
            next.token_alphabet = value.clone();
        }
        if let Some(value) = body.verify_timeout_seconds {
            next.verify_timeout_seconds = value;
        }
        if let Some(value) = body.max_attempts {
            next.max_attempts = value;
        }
        if let Some(value) = body.retry_delay_seconds {
            next.retry_delay_seconds = value;
        }
        if let Some(value) = body.token_ttl_seconds {
            next.token_ttl_seconds = value;
        }
        if let Some(value) = body.rate_limit_window_seconds {
            next.rate_limit_window_seconds = value;
        }
        next.validate()?;
        next.version += 1;
        next.updated_at = Utc::now();
        let previous_version = next.version - 1;
        {
            let mut guard = settings_store()
                .write()
                .expect("verification settings lock poisoned");
            *guard = next.clone();
        }
        // Audit event: settings changed. Changing the format does NOT
        // invalidate already-verified domains; it applies only to
        // tokens issued after this change.
        ctx.hooks.emit(
            EVENT_SETTINGS_UPDATED,
            json!({
                "previous_version": previous_version,
                "version": next.version,
                "record_type": next.record_type,
                "host_template": next.host_template,
                "value_template": next.value_template,
                "token_length": next.token_length,
                "verify_timeout_seconds": next.verify_timeout_seconds,
                "max_attempts": next.max_attempts,
                "retry_delay_seconds": next.retry_delay_seconds,
                "token_ttl_seconds": next.token_ttl_seconds,
                "rate_limit_window_seconds": next.rate_limit_window_seconds,
                "updated_by": who.actor,
                "updated_at": next.updated_at,
            }),
        );
        Ok(next)
    })();
    json::respond(result, debug)
}

async fn issue_token(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<IssueTokenRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TokenIssuance> = (|| {
        let who = caller(&req)?;
        require_permission(&ctx, &who.role, PERMISSION_ISSUE)?;
        let domain = normalize_domain(&body.domain)?;
        // Settings are read at issuance time and snapshotted onto the
        // token, so a later admin change never rewrites what this
        // token must match.
        let settings = current_settings();
        let token = generate_token(&settings);
        let host = render_host(&settings.host_template, &domain);
        let expected_value = render_value(&settings.value_template, &token);
        let now = Utc::now();
        // A new token supersedes any still-pending token for the same
        // domain (and alias, when one is given); the old token can
        // never verify again, so a stale record cannot flip status.
        for mut entry in tokens_store().iter_mut() {
            let existing = entry.value_mut();
            if existing.domain == domain
                && body.alias_id.map_or(true, |alias| existing.alias_id == Some(alias))
                && existing.status == STATUS_PENDING
            {
                existing.status = "superseded".to_string();
            }
        }
        let record = VerificationToken {
            id: Uuid::now_v7(),
            domain: domain.clone(),
            alias_id: body.alias_id,
            tenant_id: body.tenant_id,
            company_id: body.company_id,
            token_hash: hash_token(&token),
            record_type: settings.record_type.clone(),
            host: host.clone(),
            expected_value: expected_value.clone(),
            settings_version: settings.version,
            status: STATUS_PENDING.to_string(),
            issued_at: now,
            expires_at: now + chrono::Duration::seconds(settings.token_ttl_seconds),
            verified_at: None,
        };
        tokens_store().insert(record.id, record.clone());
        ctx.hooks.emit(
            EVENT_TOKEN_ISSUED,
            json!({
                "token_id": record.id,
                "alias_id": record.alias_id,
                "domain": record.domain,
                "tenant_id": record.tenant_id,
                "company_id": record.company_id,
                "record_type": record.record_type,
                "host": record.host,
                "settings_version": record.settings_version,
                "issued_by": who.actor,
                "issued_at": record.issued_at,
                "expires_at": record.expires_at,
            }),
        );
        Ok(TokenIssuance {
            token_id: record.id,
            domain,
            alias_id: body.alias_id,
            tenant_id: body.tenant_id,
            company_id: body.company_id,
            record_type: settings.record_type,
            host,
            value: expected_value,
            token,
            settings_version: settings.version,
            issued_at: record.issued_at,
            expires_at: record.expires_at,
            status: STATUS_PENDING.to_string(),
        })
    })();
    match result {
        Ok(issuance) => json::success_response(issuance, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_tokens(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let who = caller(&req)?;
        require_permission(&ctx, &who.role, PERMISSION_READ)?;
        let mut items: Vec<VerificationTokenView> = tokens_store()
            .iter()
            .map(|entry| VerificationTokenView::from(entry.value()))
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| row.domain.to_lowercase().contains(&needle));
        }
        if let Some(domain) = params.filters.get("domain") {
            let needle = domain.to_lowercase();
            items.retain(|row| row.domain.to_lowercase() == needle);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|row| row.status == *status);
        }
        if let Some(record_type) = params.filters.get("record_type") {
            items.retain(|row| row.record_type == *record_type);
        }
        let sort = params.allowed_sort(&["domain", "status", "issued_at", "expires_at"], "issued_at");
        match sort.as_str() {
            "domain" => items.sort_by(|a, b| a.domain.cmp(&b.domain)),
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            "expires_at" => items.sort_by(|a, b| a.expires_at.cmp(&b.expires_at)),
            _ => items.sort_by(|a, b| a.issued_at.cmp(&b.issued_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn revoke_token(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationTokenView> = (|| {
        let who = caller(&req)?;
        require_permission(&ctx, &who.role, PERMISSION_MANAGE)?;
        let id = id.into_inner();
        let mut entry = tokens_store()
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Verification token not found"))?;
        if entry.status == STATUS_VERIFIED {
            return Err(UnifiedError::conflict(
                "A verified token cannot be revoked; issue a new token to replace it",
            ));
        }
        entry.status = "revoked".to_string();
        Ok(VerificationTokenView::from(entry.value()))
    })();
    json::respond(result, debug)
}

/// One DNS check attempt against the token's snapshotted record.
/// Returns `(matched, record_found, error)`. The lookup type and
/// timeout come from the settings live at call time; the host and
/// expected value come from the token (issuance-time settings).
async fn check_once(
    token: &VerificationToken,
    settings: &VerificationSettings,
) -> (bool, Option<String>, Option<String>) {
    let verifier = DnsVerifier::new();
    let timeout = Duration::from_secs(settings.verify_timeout_seconds);
    if token.record_type == "cname" {
        match tokio::time::timeout(verifier.lookup_cname(&token.host), timeout).await {
            Ok(Ok(Some(target))) => {
                let matched = target
                    .trim_end_matches('.')
                    .eq_ignore_ascii_case(token.expected_value.trim_end_matches('.'));
                if matched {
                    (true, Some(target), None)
                } else {
                    (
                        false,
                        Some(target),
                        Some(format!(
                            "CNAME at {} points to \"{target}\", expected \"{}\"",
                            token.host, token.expected_value
                        )),
                    )
                }
            }
            Ok(Ok(None)) => (
                false,
                None,
                Some(format!("No CNAME record found at {}", token.host)),
            ),
            Ok(Err(error)) => (
                false,
                None,
                Some(format!("CNAME lookup failed for {}: {error}", token.host)),
            ),
            Err(_) => (
                false,
                None,
                Some(format!(
                    "CNAME lookup for {} timed out after {}s",
                    token.host, settings.verify_timeout_seconds
                )),
            ),
        }
    } else {
        match tokio::time::timeout(verifier.lookup_txt(&token.host), timeout).await {
            Ok(Ok(records)) => {
                let matched = records
                    .iter()
                    .find(|record| record.trim() == token.expected_value.trim())
                    .cloned();
                match matched {
                    Some(record) => (true, Some(record), None),
                    None => (
                        false,
                        None,
                        Some(format!(
                            "No TXT record at {} matches the expected value ({} TXT record(s) found)",
                            token.host,
                            records.len()
                        )),
                    ),
                }
            }
            Ok(Err(error)) => (
                false,
                None,
                Some(format!("TXT lookup failed for {}: {error}", token.host)),
            ),
            Err(_) => (
                false,
                None,
                Some(format!(
                    "TXT lookup for {} timed out after {}s",
                    token.host, settings.verify_timeout_seconds
                )),
            ),
        }
    }
}

async fn verify_domain_alias(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<VerifyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerifyOutcome> = (|| async {
        let who = caller(&req)?;
        require_permission(&ctx, &who.role, PERMISSION_VERIFY)?;
        let domain = normalize_domain(&body.domain)?;
        let settings = current_settings();
        let mut token = current_token_for(&domain, body.alias_id)
            .ok_or_else(|| {
                UnifiedError::not_found(
                    "No verification token issued for this domain; issue a token first",
                )
            })?;
        // Ownership guard: when both the caller and the token name a
        // tenant/company, they must agree — a tenant may not drive
        // verification for another tenant's alias.
        if let (Some(want), Some(have)) = (body.tenant_id, token.tenant_id) {
            if want != have {
                return Err(UnifiedError::forbidden(
                    "Verification token belongs to a different tenant",
                ));
            }
        }
        if let (Some(want), Some(have)) = (body.company_id, token.company_id) {
            if want != have {
                return Err(UnifiedError::forbidden(
                    "Verification token belongs to a different company",
                ));
            }
        }
        if token.status == STATUS_VERIFIED {
            // Already verified for the current token: report it
            // without another DNS round-trip.
            return Ok(VerifyOutcome {
                domain,
                alias_id: token.alias_id,
                tenant_id: token.tenant_id,
                company_id: token.company_id,
                status: STATUS_VERIFIED.to_string(),
                record_type: token.record_type,
                host: token.host,
                record_found: None,
                looked_up: json!({
                    "host": token.host,
                    "record_type": token.record_type,
                    "expected_value_prefix": value_prefix(&token.expected_value),
                }),
                attempts_made: 0,
                verified_at: token.verified_at,
                error: None,
            });
        }
        if token.expires_at <= Utc::now() {
            token.status = STATUS_FAILED.to_string();
            tokens_store().insert(token.id, token.clone());
            return Err(UnifiedError::conflict(
                "Verification token has expired; issue a new token",
            )
            .with_metadata("status", json!(STATUS_FAILED)));
        }
        // Rate limit: fail closed with 429 once max_attempts attempts
        // have been recorded inside the settings window. Both the
        // budget and the window come from admin settings.
        let recent = attempts_in_window(&domain, settings.rate_limit_window_seconds);
        if recent as u32 >= settings.max_attempts {
            return Err(UnifiedError::new(
                format!(
                    "Too many verification attempts for \"{domain}\"; try again after the rate-limit window ({}s)",
                    settings.rate_limit_window_seconds
                ),
                429,
            )
            .with_metadata("attempts_in_window", json!(recent))
            .with_metadata(
                "rate_limit_window_seconds",
                json!(settings.rate_limit_window_seconds),
            ));
        }
        let looked_up = json!({
            "host": token.host,
            "record_type": token.record_type,
            "expected_value_prefix": value_prefix(&token.expected_value),
        });
        let mut attempts_made: u32 = 0;
        let mut matched_record: Option<String> = None;
        let mut last_error: Option<String> = None;
        // Retry loop: at most max_attempts DNS checks, retry_delay
        // between attempts — both from admin settings.
        for attempt_number in 1..=settings.max_attempts {
            attempts_made = attempt_number;
            let (matched, record_found, error) = check_once(&token, &settings).await;
            let attempt = VerificationAttempt {
                id: Uuid::now_v7(),
                token_id: token.id,
                domain: domain.clone(),
                alias_id: token.alias_id,
                attempt_number,
                result: if matched {
                    STATUS_VERIFIED.to_string()
                } else {
                    STATUS_PENDING.to_string()
                },
                record_found: record_found.clone(),
                looked_up: looked_up.clone(),
                error: error.clone(),
                checked_at: Utc::now(),
            };
            attempts_store().insert(attempt.id, attempt.clone());
            ctx.hooks.emit(
                EVENT_ATTEMPTED,
                json!({
                    "attempt_id": attempt.id,
                    "token_id": token.id,
                    "alias_id": token.alias_id,
                    "domain": domain,
                    "tenant_id": token.tenant_id,
                    "company_id": token.company_id,
                    "attempt": attempt_number,
                    "result": attempt.result,
                    "record_found": attempt.record_found,
                    "error": attempt.error,
                }),
            );
            if matched {
                matched_record = record_found;
                last_error = None;
                break;
            }
            last_error = error;
            if attempt_number < settings.max_attempts {
                tokio::time::sleep(Duration::from_secs(settings.retry_delay_seconds)).await;
            }
        }
        let now = Utc::now();
        if let Some(record_found) = matched_record {
            token.status = STATUS_VERIFIED.to_string();
            token.verified_at = Some(now);
            tokens_store().insert(token.id, token.clone());
            ctx.hooks.emit(
                EVENT_COMPLETED,
                json!({
                    "alias_id": token.alias_id,
                    "domain": domain,
                    "tenant_id": token.tenant_id,
                    "company_id": token.company_id,
                    "status": STATUS_VERIFIED,
                    "record_found": record_found,
                    "verified_at": now,
                }),
            );
            Ok(VerifyOutcome {
                domain,
                alias_id: token.alias_id,
                tenant_id: token.tenant_id,
                company_id: token.company_id,
                status: STATUS_VERIFIED.to_string(),
                record_type: token.record_type,
                host: token.host,
                record_found: Some(record_found),
                looked_up,
                attempts_made,
                verified_at: Some(now),
                error: None,
            })
        } else {
            // Final failure: mark the token failed and emit the
            // failure event so the SaaS Domains module can react.
            token.status = STATUS_FAILED.to_string();
            tokens_store().insert(token.id, token.clone());
            ctx.hooks.emit(
                EVENT_FAILED,
                json!({
                    "alias_id": token.alias_id,
                    "domain": domain,
                    "tenant_id": token.tenant_id,
                    "company_id": token.company_id,
                    "status": STATUS_FAILED,
                    "record_found": Value::Null,
                    "error": last_error,
                    "attempts": attempts_made,
                    "verified_at": Value::Null,
                }),
            );
            Ok(VerifyOutcome {
                domain,
                alias_id: token.alias_id,
                tenant_id: token.tenant_id,
                company_id: token.company_id,
                status: STATUS_FAILED.to_string(),
                record_type: token.record_type,
                host: token.host,
                record_found: None,
                looked_up,
                attempts_made,
                verified_at: None,
                error: last_error,
            })
        }
    })()
    .await;
    json::respond(result, debug)
}

async fn list_attempts(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let who = caller(&req)?;
        require_permission(&ctx, &who.role, PERMISSION_READ)?;
        let mut items: Vec<VerificationAttempt> = attempts_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| row.domain.to_lowercase().contains(&needle));
        }
        if let Some(domain) = params.filters.get("domain") {
            let needle = domain.to_lowercase();
            items.retain(|row| row.domain.to_lowercase() == needle);
        }
        if let Some(token_id) = params.filters.get("token_id") {
            if let Ok(wanted) = token_id.parse::<Uuid>() {
                items.retain(|row| row.token_id == wanted);
            }
        }
        if let Some(result_filter) = params.filters.get("result") {
            items.retain(|row| row.result == *result_filter);
        }
        if let Some(alias_id) = params.filters.get("alias_id") {
            if let Ok(wanted) = alias_id.parse::<Uuid>() {
                items.retain(|row| row.alias_id == Some(wanted));
            }
        }
        let sort = params.allowed_sort(&["checked_at", "attempt_number", "result"], "checked_at");
        match sort.as_str() {
            "attempt_number" => items.sort_by(|a, b| a.attempt_number.cmp(&b.attempt_number)),
            "result" => items.sort_by(|a, b| a.result.cmp(&b.result)),
            _ => items.sort_by(|a, b| a.checked_at.cmp(&b.checked_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_status(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<StatusQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let who = caller(&req)?;
        require_permission(&ctx, &who.role, PERMISSION_READ)?;
        let domain = normalize_domain(&query.domain)?;
        let (status, verified_at) = domain_status(&domain);
        Ok(json!({
            "domain": domain,
            "status": status,
            "verified_at": verified_at,
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Hook support: answer `domain.verification.status_requested`.
//
// Synchronous handler (the stores are in-memory DashMaps): payload
// `{"domain"}`. Adds a result `{"domain", "status",
// "verified_at"}` when the domain is known, and no result when it is
// unknown, so resolvers/middleware treat unknown as "not verified"
// and never serve the domain.
// ---------------------------------------------------------------------------

fn on_status_requested(ctx: &mut HookContext) {
    let Some(domain) = ctx
        .get("domain")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        return;
    };
    let Ok(domain) = normalize_domain(&domain) else {
        return;
    };
    let (status, verified_at) = domain_status(&domain);
    if status == STATUS_UNKNOWN {
        return;
    }
    ctx.add_result(json!({
        "domain": domain,
        "status": status,
        "verified_at": verified_at,
    }));
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct DomainVerificationModule;

impl Module for DomainVerificationModule {
    fn id(&self) -> &'static str {
        "domain_verification"
    }

    fn name(&self) -> &'static str {
        "Domain Verification"
    }

    fn description(&self) -> &'static str {
        "Custom domain ownership verification through admin-determined DNS records (TXT or CNAME): token issuance with SHA-256 hashed storage, DNS checks via the Network DnsVerifier with settings-driven retries and rate limits, and domain.verification.completed / domain.verification.failed events for the SaaS Domains module."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["domains", "network"]
    }

    fn requires(&self) -> &[&'static str] {
        &["domains", "network"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["dashmap", "serde_json", "sha2", "rand"]
    }

    fn available_as_product(&self) -> bool {
        false
    }

    fn product_category(&self) -> Option<&'static str> {
        None
    }

    fn register_hooks(&self, bus: &HookBus) {
        bus.register(
            EVENT_STATUS_REQUESTED,
            "domain_verification",
            Arc::new(on_status_requested),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/domains/verification/settings", owner: "domain_verification", description: "Get verification settings" },
            RouteInfo { method: "PUT", path: "/api/v1/domains/verification/settings", owner: "domain_verification", description: "Update verification settings" },
            RouteInfo { method: "POST", path: "/api/v1/domains/verification/tokens", owner: "domain_verification", description: "Issue token" },
            RouteInfo { method: "GET", path: "/api/v1/domains/verification/tokens", owner: "domain_verification", description: "List tokens" },
            RouteInfo { method: "DELETE", path: "/api/v1/domains/verification/tokens/{id}", owner: "domain_verification", description: "Revoke token" },
            RouteInfo { method: "POST", path: "/api/v1/domains/verification/verify", owner: "domain_verification", description: "Verify domain alias" },
            RouteInfo { method: "GET", path: "/api/v1/domains/verification/attempts", owner: "domain_verification", description: "List attempts" },
            RouteInfo { method: "GET", path: "/api/v1/domains/verification/status", owner: "domain_verification", description: "Get status" },
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/domains/verification")
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/tokens", web::post().to(issue_token))
                .route("/tokens", web::get().to(list_tokens))
                .route("/tokens/{id}", web::delete().to(revoke_token))
                .route("/verify", web::post().to(verify_domain_alias))
                .route("/attempts", web::get().to(list_attempts))
                .route("/status", web::get().to(get_status)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(DomainVerificationModule));
}
