// Domain Verification — Hook support: answer `domain.verification.status_requested`.

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
// Hook support: answer `domain.verification.status_requested`.
//
// Synchronous handler (the stores are in-memory DashMaps): payload
// `{"domain"}`. Adds a result `{"domain", "status",
// "verified_at"}` when the domain is known, and no result when it is
// unknown, so resolvers/middleware treat unknown as "not verified"
// and never serve the domain.
// ---------------------------------------------------------------------------

pub(crate) fn on_status_requested(ctx: &mut HookContext) {
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
// Scheduler retry sweep: re-check pending tokens out of band.
//
// The verify HTTP handler performs exactly one DNS check per call;
// retries happen by calling verify again. The scheduler drives those
// retries through kind `domain.verify.retry` (see `KNOWN_KINDS` in
// the scheduler and the dispatch arm in its `service.rs`), which
// emits `domain.verify.retry_requested`; this listener spawns the
// sweep below. The sweep re-checks every still-pending, unexpired
// token once, inside the same per-domain rate-limit budget the
// verify handler enforces, and records attempts + token transitions
// with the same events (`attempted` / `completed` / `failed`) so
// the SaaS Domains module reacts exactly as it does to a manual
// verify call.
// ---------------------------------------------------------------------------

/// Stashed application context, following the same pattern as the
/// users / network-tools / licensing modules: rebuilt from the
/// shared `Arc`s so synchronous hook handlers can reach the hook
/// bus. Set lazily by the first verify call (see
/// `verify_domain_alias`); until then the retry listener reports
/// the sweep as skipped instead of running without a bus to emit
/// the outcome events on.
pub(crate) static DOMAIN_VERIFICATION_CTX: OnceLock<Arc<AppContext>> = OnceLock::new();

pub(crate) fn stash_ctx(ctx: &AppContext) -> Arc<AppContext> {
    Arc::new(AppContext {
        config: ctx.config.clone(),
        hooks: ctx.hooks.clone(),
        modules: ctx.modules.clone(),
        cache: ctx.cache.clone(),
        geo: ctx.geo.clone(),
        db: ctx.db.clone(),
        storage_registry: ctx.storage_registry.clone(),
        routes: ctx.routes.clone(),
        tls: ctx.tls.clone(),
        start_time: std::time::Instant::now(),
        paused: std::sync::atomic::AtomicBool::new(ctx.is_paused()),
        maintenance: std::sync::atomic::AtomicBool::new(ctx.is_maintenance()),
    })
}

pub(crate) fn hook_ctx() -> Option<Arc<AppContext>> {
    DOMAIN_VERIFICATION_CTX.get().cloned()
}

/// Scheduler listener for `domain.verify.retry_requested`: spawn
/// the pending-token sweep on the runtime and report it as
/// started. The sweep itself emits `domain.verify.retry_completed`
/// with its summary when it finishes.
pub(crate) fn on_verify_retry_requested(ctx: &mut HookContext) {
    let Some(app_ctx) = hook_ctx() else {
        ctx.add_result(json!({
            "domain_verify_retry": "skipped",
            "reason": "application context not stashed yet (no verify call has run since startup, so there is no hook bus to emit the outcome events on)",
        }));
        return;
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                let summary = retry_pending_verifications(&app_ctx).await;
                app_ctx.hooks.emit(EVENT_VERIFY_RETRY_COMPLETED, summary);
            });
            ctx.add_result(json!({ "domain_verify_retry": "started" }));
        }
        Err(_) => {
            // `tokio::spawn` would panic outside a runtime; the
            // scheduler dispatches inside one, but a direct emit
            // from a non-async caller must not take the process down.
            ctx.add_result(json!({
                "domain_verify_retry": "skipped",
                "reason": "no async runtime available to run the sweep",
            }));
        }
    }
}

/// Re-check every still-pending, unexpired verification token
/// once, within the per-domain rate-limit budget. Returns the
/// sweep summary as a JSON value. Tokens whose budget is already
/// spent are skipped (not recorded as attempts); tokens whose
/// budget runs out on this attempt are recorded as `failed`,
/// exactly like the verify handler's final attempt.
pub(crate) async fn retry_pending_verifications(app_ctx: &AppContext) -> Value {
    let settings = current_settings();
    let now = Utc::now();
    let pending: Vec<VerificationToken> = tokens_store()
        .iter()
        .filter(|entry| {
            let token = entry.value();
            token.status == STATUS_PENDING && token.expires_at > now
        })
        .map(|entry| entry.value().clone())
        .collect();
    let mut checked: u64 = 0;
    let mut verified: u64 = 0;
    let mut failed: u64 = 0;
    let mut skipped: u64 = 0;
    for mut token in pending {
        let domain = token.domain.clone();
        // Rate-limit budget, re-checked before and after the DNS
        // round-trip exactly like the verify handler: a token whose
        // budget is spent is skipped, never recorded.
        let recent_before = attempts_in_window(&domain, settings.rate_limit_window_seconds);
        if super::handlers::rate_limit_exhausted(recent_before, settings.max_attempts) {
            skipped += 1;
            continue;
        }
        let attempt_number = recent_before as u32 + 1;
        let (matched, record_found, error) = check_once(&token, &settings).await;
        let recent_after = attempts_in_window(&domain, settings.rate_limit_window_seconds);
        if super::handlers::rate_limit_exhausted(recent_after, settings.max_attempts) {
            skipped += 1;
            continue;
        }
        let exhausted =
            super::handlers::rate_limit_exhausted(recent_after + 1, settings.max_attempts);
        let attempt_result = if matched {
            STATUS_VERIFIED.to_string()
        } else if exhausted {
            STATUS_FAILED.to_string()
        } else {
            STATUS_PENDING.to_string()
        };
        let looked_up = json!({
            "host": token.host,
            "record_type": token.record_type,
            "expected_value_prefix": value_prefix(&settings.value_template, &token.host),
        });
        let attempt = VerificationAttempt {
            id: Uuid::now_v7(),
            token_id: token.id,
            domain: domain.clone(),
            alias_id: token.alias_id,
            attempt_number,
            result: attempt_result,
            record_found: record_found.clone(),
            looked_up: looked_up.clone(),
            error: error.clone(),
            checked_at: Utc::now(),
        };
        attempts_store().insert(attempt.id, attempt.clone());
        app_ctx.hooks.emit(
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
        checked += 1;
        let now = Utc::now();
        if matched {
            let record_found = record_found
                .expect("a matched check carries its record");
            token.status = STATUS_VERIFIED.to_string();
            token.verified_at = Some(now);
            tokens_store().insert(token.id, token.clone());
            app_ctx.hooks.emit(
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
            verified += 1;
        } else if exhausted {
            token.status = STATUS_FAILED.to_string();
            tokens_store().insert(token.id, token.clone());
            app_ctx.hooks.emit(
                EVENT_FAILED,
                json!({
                    "alias_id": token.alias_id,
                    "domain": domain,
                    "tenant_id": token.tenant_id,
                    "company_id": token.company_id,
                    "status": STATUS_FAILED,
                    "record_found": Value::Null,
                    "error": error,
                    "attempts": attempt_number,
                    "verified_at": Value::Null,
                }),
            );
            failed += 1;
        }
        // Otherwise the token stays pending for the next sweep.
    }
    json!({
        "checked": checked,
        "verified": verified,
        "failed": failed,
        "skipped": skipped,
    })
}
