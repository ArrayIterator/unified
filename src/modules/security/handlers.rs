// security -- Policy, checks and failed-login handlers.

use crate::auth::CsrfManager;
use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::json::success_response;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use dashmap::DashMap;
use serde_json::json;
use std::sync::Arc;

use super::auth::*;
use super::constants::*;
use super::types::*;

/// `GET /api/v1/security/csrf-token`: issue a CSRF token bound to
/// the caller's session token, for cookie-authenticated
/// state-changing requests (`X-CSRF-Token` header). Either security
/// capability suffices, so a `security:manage`-only caller can fetch
/// the token its writes require. The token expires after
/// `app.csrf_duration` minutes and is validated with the shared
/// `crate::auth::CsrfManager`.
pub(crate) async fn csrf_token(req: HttpRequest, ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let token = match authenticate_for_csrf_token(&req, &ctx).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let config = ctx.config.read().expect("config lock poisoned").clone();
    let csrf_token = CsrfManager::from_config(&config).generate(&token);
    success_response(json!({ "csrf_token": csrf_token }), 200)
}

pub(crate) async fn policy(req: HttpRequest, ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    success_response(PasswordPolicy::default(), 200)
}

pub(crate) async fn checks(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<Arc<DashMap<String, u64>>>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    let config = ctx.config.read().expect("config lock poisoned").clone();
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
            detail: format!("Salt key is {salt_len} characters long; at least 16 are recommended"),
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
    let tls_configured = !config.app.ssl.cert.is_empty() && !config.app.ssl.key.is_empty();
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
                .map_err(|error| UnifiedError::internal(format!("Database check failed: {error}")))
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

pub(crate) async fn failed_logins(
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
