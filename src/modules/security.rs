// Security module.
//
// Runs real configuration security checks, exposes the password policy,
// performs a basic TLS-presence scan of a target host, and listens to the
// `auth.failed` hook so repeated failed logins per IP are visible as a
// rate-limit signal.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hardening::{
    shared_state, FailedAuthCluster, HeadersUpdate, IpListEntry, LockoutStatus, RateLimitEntry,
    ReputationEntry, SecuritySettingsUpdate,
};
use crate::core::hooks::{events, HookBus};
use crate::core::json::{error_response, success_response};
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::db::models::User;
use crate::db::DbPool;
use actix_web::http::header;
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use dashmap::DashMap;
use diesel::prelude::*;
use hmac::{Hmac, Mac};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// Capability that grants read access to the security surface
/// (checks, policy, failed logins, and every hardening status list).
pub const CAP_SECURITY_VIEW: &str = "security:view";
/// Capability that grants write access to the security surface
/// (network scan, settings and header updates, unlock, IP-list
/// changes, reputation flagging).
pub const CAP_SECURITY_MANAGE: &str = "security:manage";

/// Password policy enforced by the application. User creation and password
/// changes must satisfy these rules.
pub const PASSWORD_MIN_LENGTH: usize = 12;
pub const PASSWORD_MAX_LENGTH: usize = 128;
pub const PASSWORD_REQUIRE_UPPERCASE: bool = true;
pub const PASSWORD_REQUIRE_LOWERCASE: bool = true;
pub const PASSWORD_REQUIRE_DIGIT: bool = true;
pub const PASSWORD_REQUIRE_SYMBOL: bool = true;

/// Failed-login count at which an IP is flagged as a rate-limit signal.
pub const FAILED_LOGIN_FLAG_THRESHOLD: u64 = 10;

pub struct SecurityModule {
    /// Failed-login attempts per IP, fed by the `auth.failed` hook.
    failed_logins: Arc<DashMap<String, u64>>,
}

impl SecurityModule {
    pub fn new() -> Self {
        Self {
            failed_logins: Arc::new(DashMap::new()),
        }
    }

    fn record_failed_login(&self, ip: &str) {
        let mut count = self.failed_logins.entry(ip.to_string()).or_insert(0);
        *count += 1;
    }
}

impl Default for SecurityModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SecurityModule::new()));
}

/// The password policy, as returned by `GET /api/v1/security/policy`.
#[derive(Debug, Clone, Serialize)]
pub struct PasswordPolicy {
    pub min_length: usize,
    pub max_length: usize,
    pub require_uppercase: bool,
    pub require_lowercase: bool,
    pub require_digit: bool,
    pub require_symbol: bool,
}

impl Default for PasswordPolicy {
    fn default() -> Self {
        Self {
            min_length: PASSWORD_MIN_LENGTH,
            max_length: PASSWORD_MAX_LENGTH,
            require_uppercase: PASSWORD_REQUIRE_UPPERCASE,
            require_lowercase: PASSWORD_REQUIRE_LOWERCASE,
            require_digit: PASSWORD_REQUIRE_DIGIT,
            require_symbol: PASSWORD_REQUIRE_SYMBOL,
        }
    }
}

impl PasswordPolicy {
    /// Validate a candidate password against the policy. Returns a list of
    /// problems; an empty list means the password is acceptable.
    pub fn validate(&self, password: &str) -> Vec<String> {
        let mut problems = Vec::new();
        if password.chars().count() < self.min_length {
            problems.push(format!(
                "Password must be at least {} characters long",
                self.min_length
            ));
        }
        if password.chars().count() > self.max_length {
            problems.push(format!(
                "Password must be at most {} characters long",
                self.max_length
            ));
        }
        if self.require_uppercase && !password.chars().any(|c| c.is_uppercase()) {
            problems.push("Password must contain an uppercase letter".to_string());
        }
        if self.require_lowercase && !password.chars().any(|c| c.is_lowercase()) {
            problems.push("Password must contain a lowercase letter".to_string());
        }
        if self.require_digit && !password.chars().any(|c| c.is_ascii_digit()) {
            problems.push("Password must contain a digit".to_string());
        }
        if self.require_symbol && !password.chars().any(|c| !c.is_alphanumeric()) {
            problems.push("Password must contain a symbol".to_string());
        }
        problems
    }
}

/// One security check result: `pass`, `warn`, or `fail`.
#[derive(Debug, Clone, Serialize)]
pub struct SecurityCheck {
    pub id: String,
    pub name: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FailedLoginEntry {
    pub ip: String,
    pub attempts: u64,
    pub flagged: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChecksResponse {
    pub overall: String,
    pub checks: Vec<SecurityCheck>,
    pub failed_logins: Vec<FailedLoginEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScanRequest {
    pub target: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanFinding {
    pub severity: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanResponse {
    pub target: String,
    pub host: String,
    pub findings: Vec<ScanFinding>,
    pub scanned_at: chrono::DateTime<chrono::Utc>,
}

// ---------------------------------------------------------------------------
// Caller authentication and capability enforcement.
//
// Every endpoint in this module requires a valid Auth Manager JWT
// (Bearer header or session cookie) and a security capability:
// `security:view` for reads, `security:manage` for writes. The
// wildcard scope `*` and `admin:access` grant both, matching the
// dashboards module's capability semantics. Scopes are trustworthy
// here because the Auth Manager mints them only from the caller's
// database roles (see `issue_token`); a token can never carry a
// capability its owner does not hold in the database.
//
// Defence in depth: when a database pool is attached, the token's
// user must still exist and be active; a deleted or disabled user's
// token stops working immediately instead of living until expiry.
// Without a pool there is no user row to check, so the token scopes
// are the only check — deployments without a database must not
// expose this surface.
// ---------------------------------------------------------------------------

/// Claims of an Auth Manager JWT, mirroring the issuer exactly.
#[derive(Debug, Clone, Deserialize)]
struct JwtClaims {
    sub: String,
    #[allow(dead_code)]
    iss: String,
    #[allow(dead_code)]
    iat: i64,
    exp: i64,
    #[allow(dead_code)]
    jti: String,
    #[serde(default)]
    scopes: Vec<String>,
}

/// Extract a Bearer token from the `Authorization` header, falling
/// back to the session cookie (`app.session_name`).
fn extract_token(req: &HttpRequest, session_cookie_name: &str) -> Option<String> {
    if let Some(header_value) = req.headers().get(header::AUTHORIZATION) {
        if let Ok(value) = header_value.to_str() {
            let trimmed = value.trim();
            if let Some(token) = trimmed
                .strip_prefix("Bearer ")
                .or_else(|| trimmed.strip_prefix("bearer "))
            {
                if !token.trim().is_empty() {
                    return Some(token.trim().to_string());
                }
            }
        }
    }
    cookie_value(req, session_cookie_name)
}

/// Read one cookie value from the raw `Cookie` header.
fn cookie_value(req: &HttpRequest, name: &str) -> Option<String> {
    let header = req.headers().get(header::COOKIE)?.to_str().ok()?;
    for part in header.split(';') {
        let part = part.trim();
        if let Some((key, value)) = part.split_once('=') {
            if key.trim() == name && !value.trim().is_empty() {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

/// Verify an Auth Manager JWT: three base64url segments, `alg`
/// `HS256`, HMAC-SHA256 over `header.payload` with signing key
/// `"{secret_key}:{salt_key}"`, and an unexpired `exp`. Every
/// failure is a 401.
fn verify_jwt(token: &str, secret_key: &str, salt_key: &str) -> UnifiedResult<JwtClaims> {
    if secret_key.is_empty() || salt_key.is_empty() {
        return Err(UnifiedError::unauthorized(
            "Security authentication is not configured: app.secret_key and app.salt_key must both be set",
        )
        .with_metadata("kind", json!("auth_not_configured")));
    }
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(UnifiedError::unauthorized(
            "Token must have three base64url segments",
        ));
    }
    let header_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[0])
        .map_err(|_| UnifiedError::unauthorized("Token header is not valid base64url"))?;
    let header: Value = serde_json::from_slice(&header_bytes)
        .map_err(|_| UnifiedError::unauthorized("Token header is not valid JSON"))?;
    if header.get("alg").and_then(|value| value.as_str()) != Some("HS256") {
        return Err(UnifiedError::unauthorized(
            "Token algorithm must be HS256",
        ));
    }
    let signing_key = format!("{secret_key}:{salt_key}");
    let signing_input = format!("{}.{}", parts[0], parts[1]);
    let provided = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|_| UnifiedError::unauthorized("Token signature is not valid base64url"))?;
    let mut mac = HmacSha256::new_from_slice(signing_key.as_bytes())
        .map_err(|error| UnifiedError::internal(format!("HMAC initialisation failed: {error}")))?;
    mac.update(signing_input.as_bytes());
    mac.verify_slice(&provided)
        .map_err(|_| UnifiedError::unauthorized("Token signature is invalid"))?;
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| UnifiedError::unauthorized("Token payload is not valid base64url"))?;
    let claims: JwtClaims = serde_json::from_slice(&payload_bytes)
        .map_err(|_| UnifiedError::unauthorized("Token payload is not valid JSON"))?;
    if claims.exp <= chrono::Utc::now().timestamp() {
        return Err(UnifiedError::unauthorized("Token has expired"));
    }
    Ok(claims)
}

/// True when the token scope set grants `capability`. The wildcard
/// scope `*` grants everything; `admin:access` is the admin
/// baseline and grants every security capability.
fn has_security_capability(scopes: &[String], capability: &str) -> bool {
    scopes.iter().any(|scope| {
        scope == "*" || scope == "admin:access" || scope == capability
    })
}

/// Run blocking Diesel work off the async worker threads.
async fn run_blocking<F, T>(pool: DbPool, work: F) -> UnifiedResult<T>
where
    F: FnOnce(&mut diesel::PgConnection) -> UnifiedResult<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut connection = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot borrow a database connection: {error}"))
        })?;
        work(&mut connection)
    })
    .await
    .map_err(|error| UnifiedError::internal(format!("Database task failed: {error}")))?
}

/// True when the user exists and is active. Without a database pool
/// there is no row to check, so this returns true and the token
/// scopes are the only check (see the section header).
async fn db_user_is_active(ctx: &AppContext, user_id: Uuid) -> UnifiedResult<bool> {
    let pool = match ctx.db.read().expect("db lock poisoned").clone() {
        Some(pool) => pool,
        None => return Ok(true),
    };
    run_blocking(pool, move |connection| {
        use crate::db::schema::users::dsl as users_dsl;
        let user = users_dsl::users
            .find(user_id)
            .select(User::as_select())
            .first::<User>(connection)
            .optional()
            .map_err(UnifiedError::from)?;
        Ok(user.is_some_and(|user| user.is_active))
    })
    .await
}

/// Enforce authentication plus one security capability for a
/// handler. Returns an error `HttpResponse` (standard envelope) on
/// failure; handlers return it directly.
async fn require_security_capability(
    req: &HttpRequest,
    ctx: &AppContext,
    capability: &str,
) -> Result<(), HttpResponse> {
    let debug = ctx.is_debug();
    let fail = |error: UnifiedError| error_response(&error, debug);
    let (secret_key, salt_key, session_name) = {
        let config = ctx.config.read().expect("config lock poisoned");
        (
            config.app.secret_key.clone(),
            config.app.salt_key.clone(),
            config.app.session_name.clone(),
        )
    };
    let token = extract_token(req, &session_name)
        .ok_or_else(|| fail(UnifiedError::unauthorized("Authentication required: provide a Bearer token")))?;
    let claims = verify_jwt(&token, &secret_key, &salt_key).map_err(fail)?;
    let user_id = Uuid::parse_str(claims.sub.trim()).map_err(|_| {
        fail(UnifiedError::unauthorized("Token subject is not a user id")
            .with_metadata("kind", json!("invalid_subject")))
    })?;
    if !has_security_capability(&claims.scopes, capability) {
        return Err(fail(
            UnifiedError::forbidden(format!(
                "Capability \"{capability}\" is required for this security endpoint"
            ))
            .with_metadata("capability", json!(capability)),
        ));
    }
    match db_user_is_active(ctx, user_id).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(fail(UnifiedError::unauthorized(
            "Authenticated user does not exist or is disabled",
        )
        .with_metadata("kind", json!("unknown_user")))),
        Err(error) => Err(fail(error)),
    }
}

async fn policy(req: HttpRequest, ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    success_response(PasswordPolicy::default(), 200)
}

async fn checks(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<Arc<DashMap<String, u64>>>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    let config = ctx
        .config
        .read()
        .expect("config lock poisoned")
        .clone();
    let mut results: Vec<SecurityCheck> = Vec::new();

    // Secret key length.
    let secret_len = config.app.secret_key.chars().count();
    if config.app.mode == "production" {
        if secret_len >= 32 {
            results.push(SecurityCheck {
                id: "secret_key".to_string(),
                name: "Secret key length".to_string(),
                status: "pass".to_string(),
                detail: format!("Secret key is {secret_len} characters long"),
            });
        } else {
            results.push(SecurityCheck {
                id: "secret_key".to_string(),
                name: "Secret key length".to_string(),
                status: "fail".to_string(),
                detail: format!(
                    "Secret key is {secret_len} characters long; at least 32 are required in production"
                ),
            });
        }
    } else if secret_len >= 32 {
        results.push(SecurityCheck {
            id: "secret_key".to_string(),
            name: "Secret key length".to_string(),
            status: "pass".to_string(),
            detail: format!("Secret key is {secret_len} characters long"),
        });
    } else {
        results.push(SecurityCheck {
            id: "secret_key".to_string(),
            name: "Secret key length".to_string(),
            status: "warn".to_string(),
            detail: format!(
                "Secret key is {secret_len} characters long; at least 32 are required in production"
            ),
        });
    }

    // Salt key length.
    let salt_len = config.app.salt_key.chars().count();
    if config.app.mode == "production" && salt_len < 16 {
        results.push(SecurityCheck {
            id: "salt_key".to_string(),
            name: "Salt key length".to_string(),
            status: "fail".to_string(),
            detail: format!(
                "Salt key is {salt_len} characters long; at least 16 are required in production"
            ),
        });
    } else if salt_len >= 16 {
        results.push(SecurityCheck {
            id: "salt_key".to_string(),
            name: "Salt key length".to_string(),
            status: "pass".to_string(),
            detail: format!("Salt key is {salt_len} characters long"),
        });
    } else {
        results.push(SecurityCheck {
            id: "salt_key".to_string(),
            name: "Salt key length".to_string(),
            status: "warn".to_string(),
            detail: format!(
                "Salt key is {salt_len} characters long; at least 16 are recommended"
            ),
        });
    }

    // Secret and salt must differ.
    if !config.app.secret_key.is_empty() && config.app.secret_key == config.app.salt_key {
        results.push(SecurityCheck {
            id: "secret_salt_distinct".to_string(),
            name: "Secret and salt differ".to_string(),
            status: "fail".to_string(),
            detail: "Secret key and salt key are identical".to_string(),
        });
    } else {
        results.push(SecurityCheck {
            id: "secret_salt_distinct".to_string(),
            name: "Secret and salt differ".to_string(),
            status: "pass".to_string(),
            detail: "Secret key and salt key differ".to_string(),
        });
    }

    // Application mode.
    if config.app.mode == "production" {
        results.push(SecurityCheck {
            id: "mode".to_string(),
            name: "Application mode".to_string(),
            status: "pass".to_string(),
            detail: "Running in production mode".to_string(),
        });
    } else {
        results.push(SecurityCheck {
            id: "mode".to_string(),
            name: "Application mode".to_string(),
            status: "warn".to_string(),
            detail: format!(
                "Running in {} mode; debug output may be exposed",
                config.app.mode
            ),
        });
    }

    // Password policy.
    let policy = PasswordPolicy::default();
    results.push(SecurityCheck {
        id: "password_policy".to_string(),
        name: "Password policy".to_string(),
        status: "pass".to_string(),
        detail: format!(
            "Passwords require {}-{} characters, upper and lower case, a digit and a symbol",
            policy.min_length, policy.max_length
        ),
    });

    // TLS configuration.
    let tls_configured =
        !config.app.ssl.cert.is_empty() && !config.app.ssl.key.is_empty();
    if tls_configured {
        results.push(SecurityCheck {
            id: "tls".to_string(),
            name: "TLS configured".to_string(),
            status: "pass".to_string(),
            detail: format!(
                "TLS certificate configured ({}), listening on {}",
                config.app.ssl.cert,
                config.app.ssl.listen.join(", ")
            ),
        });
    } else if config.app.ssl.auto_create {
        results.push(SecurityCheck {
            id: "tls".to_string(),
            name: "TLS configured".to_string(),
            status: "warn".to_string(),
            detail: "No static certificate configured; automatic certificate creation is enabled"
                .to_string(),
        });
    } else if config.app.mode == "production" {
        results.push(SecurityCheck {
            id: "tls".to_string(),
            name: "TLS configured".to_string(),
            status: "fail".to_string(),
            detail: "No TLS certificate configured in production mode".to_string(),
        });
    } else {
        results.push(SecurityCheck {
            id: "tls".to_string(),
            name: "TLS configured".to_string(),
            status: "warn".to_string(),
            detail: "No TLS certificate configured".to_string(),
        });
    }

    // Database reachability, only when a pool is attached.
    let pool = ctx.db.read().expect("db lock poisoned").clone();
    match pool {
        Some(pool) => {
            let checkout = web::block(move || pool.get().map(|_| ()))
                .await
                .map_err(|error| {
                    UnifiedError::internal(format!("Database check failed: {error}"))
                })
                .and_then(|result| {
                    result.map_err(|error| {
                        UnifiedError::internal(format!(
                            "Database connection checkout failed: {error}"
                        ))
                    })
                });
            match checkout {
                Ok(()) => results.push(SecurityCheck {
                    id: "database".to_string(),
                    name: "Database reachable".to_string(),
                    status: "pass".to_string(),
                    detail: "Database connection checkout succeeded".to_string(),
                }),
                Err(error) => results.push(SecurityCheck {
                    id: "database".to_string(),
                    name: "Database reachable".to_string(),
                    status: "fail".to_string(),
                    detail: error.message,
                }),
            }
        }
        None => results.push(SecurityCheck {
            id: "database".to_string(),
            name: "Database reachable".to_string(),
            status: "warn".to_string(),
            detail: "No database pool attached; reachability not checked".to_string(),
        }),
    }

    // Failed-login rate-limit signal.
    let mut failed_logins: Vec<FailedLoginEntry> = store
        .iter()
        .map(|entry| FailedLoginEntry {
            ip: entry.key().clone(),
            attempts: *entry.value(),
            flagged: *entry.value() >= FAILED_LOGIN_FLAG_THRESHOLD,
        })
        .collect();
    failed_logins.sort_by(|a, b| b.attempts.cmp(&a.attempts));
    let flagged = failed_logins.iter().filter(|entry| entry.flagged).count();
    if flagged > 0 {
        results.push(SecurityCheck {
            id: "auth_failures".to_string(),
            name: "Failed login attempts".to_string(),
            status: "warn".to_string(),
            detail: format!(
                "{flagged} IP address(es) reached the failed-login threshold of {FAILED_LOGIN_FLAG_THRESHOLD} attempts"
            ),
        });
    } else {
        results.push(SecurityCheck {
            id: "auth_failures".to_string(),
            name: "Failed login attempts".to_string(),
            status: "pass".to_string(),
            detail: "No IP address reached the failed-login threshold".to_string(),
        });
    }

    let overall = if results.iter().any(|check| check.status == "fail") {
        "fail"
    } else if results.iter().any(|check| check.status == "warn") {
        "warn"
    } else {
        "pass"
    };

    success_response(
        ChecksResponse {
            overall: overall.to_string(),
            checks: results,
            failed_logins,
        },
        200,
    )
}

async fn failed_logins(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<Arc<DashMap<String, u64>>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    let pagination = query.normalized();
    let mut entries: Vec<FailedLoginEntry> = store
        .iter()
        .map(|entry| FailedLoginEntry {
            ip: entry.key().clone(),
            attempts: *entry.value(),
            flagged: *entry.value() >= FAILED_LOGIN_FLAG_THRESHOLD,
        })
        .collect();
    entries.sort_by(|a, b| b.attempts.cmp(&a.attempts));
    let total = entries.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(entries.len());
    let items = if start < entries.len() {
        entries[start..end].to_vec()
    } else {
        Vec::new()
    };
    success_response(PageResult::new(items, total, &pagination).to_json(), 200)
}

/// Validate that a target is a domain name or an IP address, with an
/// optional port. Returns the bare host (without port or scheme).
fn parse_target(target: &str) -> UnifiedResult<String> {
    let host = target
        .trim()
        .strip_prefix("https://")
        .or_else(|| target.trim().strip_prefix("http://"))
        .unwrap_or_else(|| target.trim());
    let host = host
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if host.is_empty() {
        return Err(UnifiedError::bad_request("Scan target must not be empty"));
    }
    let domain_pattern = Regex::new(
        r"^(?i)([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)+$",
    )
    .expect("domain validation pattern is valid");
    let is_ip = host.parse::<std::net::IpAddr>().is_ok();
    if !is_ip && !domain_pattern.is_match(&host) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid scan target: \"{host}\" is not a valid domain name or IP address"
        )));
    }
    Ok(host)
}

/// Probe whether a TCP port on the host accepts connections.
fn port_open(host: &str, port: u16, timeout: Duration) -> (bool, u64) {
    let started = Instant::now();
    let open = format!("{host}:{port}")
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.next())
        .and_then(|address| TcpStream::connect_timeout(&address, timeout).ok())
        .is_some();
    (open, started.elapsed().as_millis() as u64)
}

async fn scan(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<ScanRequest>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_MANAGE).await {
        return response;
    }
    let debug = ctx.is_debug();
    let host = match parse_target(&body.target) {
        Ok(host) => host,
        Err(error) => return error_response(&error, debug),
    };
    // Network probes block, so run them off the async worker threads.
    let host_for_probe = host.clone();
    let probed = web::block(move || {
        let timeout = Duration::from_secs(5);
        let (tls_open, tls_latency) = port_open(&host_for_probe, 443, timeout);
        let (http_open, http_latency) = port_open(&host_for_probe, 80, timeout);
        (tls_open, tls_latency, http_open, http_latency)
    })
    .await;
    let (tls_open, tls_latency, http_open, http_latency) = match probed {
        Ok(values) => values,
        Err(error) => {
            return error_response(
                &UnifiedError::internal(format!("Scan failed: {error}")),
                debug,
            )
        }
    };

    let mut findings = Vec::new();
    if tls_open {
        findings.push(ScanFinding {
            severity: "info".to_string(),
            code: "tls_port_open".to_string(),
            message: format!("TCP port 443 is open ({tls_latency} ms); TLS is served"),
        });
    } else {
        findings.push(ScanFinding {
            severity: "high".to_string(),
            code: "tls_port_closed".to_string(),
            message: "TCP port 443 is closed or unreachable; no TLS endpoint was found"
                .to_string(),
        });
    }
    if http_open {
        findings.push(ScanFinding {
            severity: "info".to_string(),
            code: "http_port_open".to_string(),
            message: format!("TCP port 80 is open ({http_latency} ms)"),
        });
    } else {
        findings.push(ScanFinding {
            severity: "low".to_string(),
            code: "http_port_closed".to_string(),
            message: "TCP port 80 is closed or unreachable".to_string(),
        });
    }
    if tls_open && !http_open {
        findings.push(ScanFinding {
            severity: "info".to_string(),
            code: "https_only".to_string(),
            message: "Host serves HTTPS only; plain HTTP is not reachable".to_string(),
        });
    }

    success_response(
        ScanResponse {
            target: body.target.clone(),
            host,
            findings,
            scanned_at: chrono::Utc::now(),
        },
        200,
    )
}

impl Module for SecurityModule {
    fn id(&self) -> &'static str {
        "security"
    }

    fn name(&self) -> &'static str {
        "Security"
    }

    fn description(&self) -> &'static str {
        "Runs security checks, exposes the password policy, scans targets for TLS presence, and serves the hardening surface: brute-force status and unlock, rate-limit status, IP allow/block lists, security headers, and failed-auth alerts. Every endpoint requires a valid Auth Manager JWT and a security capability (`security:view` for reads, `security:manage` for writes)."
    }

    fn register_hooks(&self, bus: &HookBus) {
        let failed_logins = self.failed_logins.clone();
        bus.register(events::AUTH_FAILED, "security", Arc::new(move |context| {
            // The auth manager emits `auth.failed` without an IP on the
            // token-verification path and with `ip` / `user_id` on the
            // login paths; feed the hardening engine with whatever the
            // payload carries so repeated failures still cluster.
            let ip = context
                .get("ip")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            let account = context
                .get("user_id")
                .or_else(|| context.get("account_id"))
                .or_else(|| context.get("email"))
                .and_then(|value| value.as_str())
                .map(str::to_string);
            if let Some(ip) = &ip {
                let mut count = failed_logins.entry(ip.clone()).or_insert(0);
                *count += 1;
            }
            shared_state().record_login_failure(ip.as_deref(), account.as_deref());
        }));
        // A successful login resets the brute-force counters for the IP
        // and the account, so earlier failures never lock out a
        // legitimate user. The auth manager emits `auth.login` with
        // `user_id` on both the JWT issue path and the Google SSO path.
        bus.register(events::AUTH_LOGIN, "security", Arc::new(move |context| {
            let ip = context
                .get("ip")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            let account = context
                .get("user_id")
                .or_else(|| context.get("account_id"))
                .or_else(|| context.get("email"))
                .and_then(|value| value.as_str())
                .map(str::to_string);
            shared_state().record_login_success(ip.as_deref(), account.as_deref());
        }));
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/security/checks", owner: "security", description: "Checks" },
            RouteInfo { method: "GET", path: "/api/v1/security/policy", owner: "security", description: "Policy" },
            RouteInfo { method: "POST", path: "/api/v1/security/scan", owner: "security", description: "Scan" },
            RouteInfo { method: "GET", path: "/api/v1/security/failed-logins", owner: "security", description: "Failed logins" },
            RouteInfo { method: "GET", path: "/api/v1/security/hardening/settings", owner: "security", description: "`GET /api/v1/security/hardening/settings`: the live hardening settings plus live counts (list sizes, current lockouts, flagged IPs, failed-auth clusters)." },
            RouteInfo { method: "PUT", path: "/api/v1/security/hardening/settings", owner: "security", description: "`PUT /api/v1/security/hardening/settings`: partial settings update." },
            RouteInfo { method: "GET", path: "/api/v1/security/hardening/brute-force", owner: "security", description: "`GET /api/v1/security/hardening/brute-force`: paginated lockout / attempt status per IP and per account." },
            RouteInfo { method: "POST", path: "/api/v1/security/hardening/brute-force/unlock", owner: "security", description: "`POST /api/v1/security/hardening/brute-force/unlock`: manually clear a lockout / attempt record for one IP (`kind: \"ip\"`) or one account (`kind: \"account\"`)." },
            RouteInfo { method: "GET", path: "/api/v1/security/hardening/rate-limit", owner: "security", description: "`GET /api/v1/security/hardening/rate-limit`: paginated tracked rate-limit windows." },
            RouteInfo { method: "GET", path: "/api/v1/security/hardening/ip-lists", owner: "security", description: "`GET /api/v1/security/hardening/ip-lists`: paginated allow/block list entries." },
            RouteInfo { method: "POST", path: "/api/v1/security/hardening/ip-lists", owner: "security", description: "`POST /api/v1/security/hardening/ip-lists`: add one CIDR range or bare IP to the allowlist or the blocklist." },
            RouteInfo { method: "DELETE", path: "/api/v1/security/hardening/ip-lists", owner: "security", description: "`DELETE /api/v1/security/hardening/ip-lists?list=allow&cidr=...`: remove one entry from the allowlist or the blocklist." },
            RouteInfo { method: "GET", path: "/api/v1/security/hardening/headers", owner: "security", description: "`GET /api/v1/security/hardening/headers`: the configured security header values (empty means that header is disabled)." },
            RouteInfo { method: "PUT", path: "/api/v1/security/hardening/headers", owner: "security", description: "`PUT /api/v1/security/hardening/headers`: partial header update." },
            RouteInfo { method: "GET", path: "/api/v1/security/hardening/failed-auth-alerts", owner: "security", description: "`GET /api/v1/security/hardening/failed-auth-alerts`: paginated failed-auth clusters (repeated failures grouped by IP + account), most recently active first." },
            RouteInfo { method: "GET", path: "/api/v1/security/hardening/reputation", owner: "security", description: "`GET /api/v1/security/hardening/reputation`: paginated IP reputation records, highest score first." },
            RouteInfo { method: "POST", path: "/api/v1/security/hardening/reputation/flag", owner: "security", description: "`POST /api/v1/security/hardening/reputation/flag`: manually flag an IP with a reason." },
            RouteInfo { method: "POST", path: "/api/v1/security/hardening/reputation/unflag", owner: "security", description: "`POST /api/v1/security/hardening/reputation/unflag`: manually unflag an IP and reset its score." },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/security")
                .app_data(web::Data::new(self.failed_logins.clone()))
                .route("/checks", web::get().to(checks))
                .route("/policy", web::get().to(policy))
                .route("/scan", web::post().to(scan))
                .route("/failed-logins", web::get().to(failed_logins))
                .route("/hardening/settings", web::get().to(get_hardening_settings))
                .route(
                    "/hardening/settings",
                    web::put().to(update_hardening_settings),
                )
                .route(
                    "/hardening/brute-force",
                    web::get().to(list_brute_force_status),
                )
                .route(
                    "/hardening/brute-force/unlock",
                    web::post().to(unlock_brute_force),
                )
                .route(
                    "/hardening/rate-limit",
                    web::get().to(list_rate_limit_status),
                )
                .route("/hardening/ip-lists", web::get().to(list_ip_entries))
                .route("/hardening/ip-lists", web::post().to(add_ip_entry))
                .route(
                    "/hardening/ip-lists",
                    web::delete().to(remove_ip_entry),
                )
                .route("/hardening/headers", web::get().to(get_headers_config))
                .route("/hardening/headers", web::put().to(update_headers_config))
                .route(
                    "/hardening/failed-auth-alerts",
                    web::get().to(list_failed_auth_alerts),
                )
                .route("/hardening/reputation", web::get().to(list_reputation))
                .route(
                    "/hardening/reputation/flag",
                    web::post().to(flag_reputation),
                )
                .route(
                    "/hardening/reputation/unflag",
                    web::post().to(unflag_reputation),
                ),
        );
    }
}

// ---------------------------------------------------------------------------
// Hardening HTTP surface
//
// Every handler below reads the shared `HardeningState` (the same
// instance the middleware enforces), so a settings change or an
// unlock takes effect immediately. All list endpoints use the shared
// pagination parameters (`page`, `per_page`, `sort`, `order`,
// `search`, plus exact-match filters) and return
// `PageResult::to_json()` inside the standard success envelope.
// ---------------------------------------------------------------------------

/// Paginate an already-collected, already-sorted item list.
fn paginate_items<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> serde_json::Value {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination).to_json()
}

fn matches_search(haystack: &str, search: &Option<String>) -> bool {
    match search {
        Some(needle) => haystack
            .to_lowercase()
            .contains(&needle.to_lowercase()),
        None => true,
    }
}

/// `GET /api/v1/security/hardening/settings`: the live hardening
/// settings plus live counts (list sizes, current lockouts, flagged
/// IPs, failed-auth clusters).
async fn get_hardening_settings(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    let state = shared_state();
    let settings = state.settings();
    let counts = state.overview_counts();
    success_response(
        json!({ "settings": settings, "counts": counts }),
        200,
    )
}

/// `PUT /api/v1/security/hardening/settings`: partial settings
/// update. Only the fields present are changed; the merged settings
/// are validated and nothing changes on failure. Updates the runtime
/// copy only and does not rewrite the configuration file.
async fn update_hardening_settings(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<SecuritySettingsUpdate>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_MANAGE).await {
        return response;
    }
    let debug = ctx.is_debug();
    let state = shared_state();
    match state.update_settings(body.into_inner()) {
        Ok(settings) => success_response(
            json!({ "settings": settings, "counts": state.overview_counts() }),
            200,
        ),
        Err(message) => error_response(&UnifiedError::bad_request(message), debug),
    }
}

/// `GET /api/v1/security/hardening/brute-force`: paginated lockout /
/// attempt status per IP and per account. Filters: `kind` (`ip` or
/// `account`), `locked` (`true` / `false`); `search` matches the key.
async fn list_brute_force_status(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    let pagination = query.normalized();
    let mut statuses: Vec<LockoutStatus> = shared_state().lockout_statuses();
    if let Some(kind) = pagination.filters.get("kind") {
        statuses.retain(|entry| &entry.kind == kind);
    }
    if let Some(locked) = pagination.filters.get("locked") {
        let want = locked == "true";
        statuses.retain(|entry| entry.locked == want);
    }
    statuses.retain(|entry| matches_search(&entry.key, &pagination.search));
    success_response(paginate_items(statuses, &query), 200)
}

#[derive(Debug, Clone, Deserialize)]
pub struct UnlockRequest {
    pub target: String,
    pub kind: String,
}

/// `POST /api/v1/security/hardening/brute-force/unlock`: manually
/// clear a lockout / attempt record for one IP (`kind: "ip"`) or one
/// account (`kind: "account"`). Emits `security.bruteforce.unlocked`.
async fn unlock_brute_force(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<UnlockRequest>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_MANAGE).await {
        return response;
    }
    let debug = ctx.is_debug();
    let kind = body.kind.trim();
    if kind != "ip" && kind != "account" {
        return error_response(
            &UnifiedError::bad_request("kind must be \"ip\" or \"account\""),
            debug,
        );
    }
    let target = body.target.trim();
    if target.is_empty() {
        return error_response(&UnifiedError::bad_request("target must not be empty"), debug);
    }
    if shared_state().unlock(kind, target) {
        success_response(
            json!({ "unlocked": true, "kind": kind, "key": target }),
            200,
        )
    } else {
        error_response(
            &UnifiedError::not_found(format!(
                "No brute-force record found for {kind} \"{target}\""
            )),
            debug,
        )
    }
}

/// `GET /api/v1/security/hardening/rate-limit`: paginated tracked
/// rate-limit windows. Filters: `scope` (`ip`, `user`, or
/// `endpoint`); `search` matches the window key.
async fn list_rate_limit_status(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    let pagination = query.normalized();
    let mut entries: Vec<RateLimitEntry> = shared_state().rate_limit_entries();
    if let Some(scope) = pagination.filters.get("scope") {
        entries.retain(|entry| &entry.scope == scope);
    }
    entries.retain(|entry| matches_search(&entry.key, &pagination.search));
    success_response(paginate_items(entries, &query), 200)
}

/// `GET /api/v1/security/hardening/ip-lists`: paginated allow/block
/// list entries. Filter: `list` (`allow` or `block`); `search`
/// matches the CIDR text.
async fn list_ip_entries(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    let pagination = query.normalized();
    let state = shared_state();
    let mut entries: Vec<IpListEntry> = Vec::new();
    match pagination.filters.get("list").map(String::as_str) {
        Some("allow") => entries.extend(state.ip_list_entries("allow")),
        Some("block") => entries.extend(state.ip_list_entries("block")),
        _ => {
            entries.extend(state.ip_list_entries("allow"));
            entries.extend(state.ip_list_entries("block"));
            entries.sort_by(|a, b| a.cidr.cmp(&b.cidr));
        }
    }
    entries.retain(|entry| matches_search(&entry.cidr, &pagination.search));
    success_response(paginate_items(entries, &query), 200)
}

#[derive(Debug, Clone, Deserialize)]
pub struct IpListAddRequest {
    pub list: String,
    pub cidr: String,
}

/// `POST /api/v1/security/hardening/ip-lists`: add one CIDR range or
/// bare IP to the allowlist or the blocklist. CIDR notation is
/// validated for IPv4 and IPv6; a bare IP becomes a host route.
async fn add_ip_entry(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<IpListAddRequest>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_MANAGE).await {
        return response;
    }
    let debug = ctx.is_debug();
    match shared_state().add_ip_entry(body.list.trim(), body.cidr.trim()) {
        Ok(entry) => success_response(entry, 201),
        Err(message) => error_response(&UnifiedError::bad_request(message), debug),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct IpListRemoveQuery {
    pub list: Option<String>,
    pub cidr: Option<String>,
}

/// `DELETE /api/v1/security/hardening/ip-lists?list=allow&cidr=...`:
/// remove one entry from the allowlist or the blocklist. Removing a
/// missing entry is a 404.
async fn remove_ip_entry(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<IpListRemoveQuery>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_MANAGE).await {
        return response;
    }
    let debug = ctx.is_debug();
    let (Some(list), Some(cidr)) = (query.list.clone(), query.cidr.clone()) else {
        return error_response(
            &UnifiedError::bad_request("query parameters \"list\" and \"cidr\" are required"),
            debug,
        );
    };
    match shared_state().remove_ip_entry(list.trim(), cidr.trim()) {
        Ok(true) => success_response(
            json!({ "removed": true, "list": list, "cidr": cidr }),
            200,
        ),
        Ok(false) => error_response(
            &UnifiedError::not_found(format!(
                "No \"{list}\" list entry found for \"{cidr}\""
            )),
            debug,
        ),
        Err(message) => error_response(&UnifiedError::bad_request(message), debug),
    }
}

/// `GET /api/v1/security/hardening/headers`: the configured security
/// header values (empty means that header is disabled).
async fn get_headers_config(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    let settings = shared_state().settings();
    success_response(
        json!({
            "strict_transport_security": settings.headers_hsts,
            "content_type_options": settings.headers_content_type_options,
            "frame_options": settings.headers_frame_options,
            "referrer_policy": settings.headers_referrer_policy,
            "content_security_policy": settings.headers_content_security_policy,
            "permissions_policy": settings.headers_permissions_policy,
        }),
        200,
    )
}

/// `PUT /api/v1/security/hardening/headers`: partial header update.
/// Only the fields present are changed; an empty string disables
/// that header.
async fn update_headers_config(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<HeadersUpdate>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_MANAGE).await {
        return response;
    }
    let settings = shared_state().update_headers(body.into_inner());
    success_response(
        json!({
            "strict_transport_security": settings.headers_hsts,
            "content_type_options": settings.headers_content_type_options,
            "frame_options": settings.headers_frame_options,
            "referrer_policy": settings.headers_referrer_policy,
            "content_security_policy": settings.headers_content_security_policy,
            "permissions_policy": settings.headers_permissions_policy,
        }),
        200,
    )
}

/// `GET /api/v1/security/hardening/failed-auth-alerts`: paginated
/// failed-auth clusters (repeated failures grouped by IP + account),
/// most recently active first. `search` matches the IP or account.
async fn list_failed_auth_alerts(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    let pagination = query.normalized();
    let mut clusters: Vec<FailedAuthCluster> = shared_state().failed_auth_clusters();
    clusters.retain(|cluster| {
        let haystack = format!(
            "{} {} {}",
            cluster.key,
            cluster.ip.clone().unwrap_or_default(),
            cluster.account.clone().unwrap_or_default()
        );
        matches_search(&haystack, &pagination.search)
    });
    success_response(paginate_items(clusters, &query), 200)
}

/// `GET /api/v1/security/hardening/reputation`: paginated IP
/// reputation records, highest score first. Filter: `flagged`
/// (`true` / `false`); `search` matches the IP.
async fn list_reputation(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    let pagination = query.normalized();
    let mut entries: Vec<ReputationEntry> = shared_state().reputation_entries();
    if let Some(flagged) = pagination.filters.get("flagged") {
        let want = flagged == "true";
        entries.retain(|entry| entry.flagged == want);
    }
    entries.retain(|entry| matches_search(&entry.ip, &pagination.search));
    success_response(paginate_items(entries, &query), 200)
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReputationFlagRequest {
    pub ip: String,
    pub reason: Option<String>,
}

/// `POST /api/v1/security/hardening/reputation/flag`: manually flag
/// an IP with a reason. Emits `security.ip.flagged`.
async fn flag_reputation(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<ReputationFlagRequest>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_MANAGE).await {
        return response;
    }
    let debug = ctx.is_debug();
    match shared_state().flag_ip(body.ip.trim(), body.reason.clone()) {
        Ok(entry) => success_response(entry, 200),
        Err(message) => error_response(&UnifiedError::bad_request(message), debug),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReputationUnflagRequest {
    pub ip: String,
}

/// `POST /api/v1/security/hardening/reputation/unflag`: manually
/// unflag an IP and reset its score. Emits `security.ip.unflagged`.
async fn unflag_reputation(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<ReputationUnflagRequest>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_MANAGE).await {
        return response;
    }
    let debug = ctx.is_debug();
    match shared_state().unflag_ip(body.ip.trim()) {
        Ok(entry) => success_response(entry, 200),
        Err(message) => error_response(&UnifiedError::bad_request(message), debug),
    }
}
