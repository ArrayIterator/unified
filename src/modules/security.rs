// Security module.
//
// Runs real configuration security checks, exposes the password policy,
// performs a basic TLS-presence scan of a target host, and listens to the
// `auth.failed` hook so repeated failed logins per IP are visible as a
// rate-limit signal.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus};
use crate::core::json::{error_response, success_response};
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use dashmap::DashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

fn policy() -> HttpResponse {
    success_response(PasswordPolicy::default(), 200)
}

async fn checks(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<Arc<DashMap<String, u64>>>,
) -> HttpResponse {
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
    store: web::Data<Arc<DashMap<String, u64>>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
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

async fn scan(ctx: web::Data<Arc<AppContext>>, body: web::Json<ScanRequest>) -> HttpResponse {
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
        "Runs security checks, exposes the password policy, and scans targets for TLS presence."
    }

    fn register_hooks(&self, bus: &HookBus) {
        let failed_logins = self.failed_logins.clone();
        bus.register(events::AUTH_FAILED, "security", Arc::new(move |context| {
            if let Some(ip) = context.get("ip").and_then(|value| value.as_str()) {
                let mut count = failed_logins.entry(ip.to_string()).or_insert(0);
                *count += 1;
            }
        }));
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/security")
                .app_data(web::Data::new(self.failed_logins.clone()))
                .route("/checks", web::get().to(checks))
                .route("/policy", web::get().to(policy))
                .route("/scan", web::post().to(scan))
                .route("/failed-logins", web::get().to(failed_logins)),
        );
    }
}
