// Domain Verification — Handlers.

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
// Handlers.
// ---------------------------------------------------------------------------

pub(crate) async fn get_settings(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let who = caller(&req)?;
        require_permission(&ctx, &who.role, PERMISSION_MANAGE)?;
        Ok(current_settings())
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationSettings> = (|| {
        let _tenant = require_tenant(&req)?;
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

pub(crate) async fn issue_token(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<IssueTokenRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TokenIssuance> = (|| {
        let _tenant = require_tenant(&req)?;
        let who = caller(&req)?;
        require_permission(&ctx, &who.role, PERMISSION_ISSUE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        require_body_tenant_match(&_tenant, &body.tenant_id)?;
        let company_id = require_company_match_uuid(&_tenant, &req, &body.company_id)?;
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
        // domain AND the same alias: alias ids must match exactly,
        // including None == None, so issuing a token for one alias
        // never kills another alias's pending token (and issuing a
        // no-alias token supersedes only the no-alias token).
        for mut entry in tokens_store().iter_mut() {
            let existing = entry.value_mut();
            if existing.domain == domain
                && in_company_scope(&existing.company_id, &scope)
                && supersedes_token(body.alias_id, existing.alias_id)
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
            company_id,
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
            company_id,
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

pub(crate) async fn list_tokens(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let who = caller(&req)?;
        require_permission(&ctx, &who.role, PERMISSION_READ)?;
        let scope = caller_company(&_tenant, &req)?;
        // Filter on the raw token before mapping to the public view:
        // the view strips the company, so the scope must be decided
        // here. A scoped caller sees only their own company's tokens
        // plus global ones; an unresolved caller sees global only.
        let mut items: Vec<VerificationTokenView> = tokens_store()
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|token| in_company_scope(&token.company_id, &scope))
            .map(|token| VerificationTokenView::from(&token))
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

pub(crate) async fn revoke_token(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationTokenView> = (|| {
        let _tenant = require_tenant(&req)?;
        let who = caller(&req)?;
        require_permission(&ctx, &who.role, PERMISSION_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let id = id.into_inner();
        let mut entry = tokens_store()
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Verification token not found"))?;
        require_record_writable_in_company_scope(&entry.company_id, &scope)?;
        if !token_is_revocable(&entry.status) {
            return Err(UnifiedError::conflict(format!(
                "A {} token cannot be revoked; only a pending token can be revoked",
                entry.status
            )));
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
pub(crate) async fn check_once(
    token: &VerificationToken,
    settings: &VerificationSettings,
) -> (bool, Option<String>, Option<String>) {
    let verifier = DnsVerifier::new();
    let timeout = Duration::from_secs(settings.verify_timeout_seconds);
    if token.record_type == "cname" {
        match tokio::time::timeout(timeout, verifier.lookup_cname(&token.host)).await {
            Ok(Ok(Some(target))) => {
                let matched = target
                    .trim_end_matches('.')
                    .eq_ignore_ascii_case(token.expected_value.trim_end_matches('.'));
                if matched {
                    (true, Some(target), None)
                } else {
                    (
                        false,
                        Some(target.clone()),
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
        match tokio::time::timeout(timeout, verifier.lookup_txt(&token.host)).await {
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

/// A new token supersedes a pending token only for the exact same
/// alias: alias ids must match exactly, including None == None
/// (a token with no alias supersedes only the no-alias token for
/// the domain, never another alias's token).
pub(crate) fn supersedes_token(
    new_alias_id: Option<Uuid>,
    existing_alias_id: Option<Uuid>,
) -> bool {
    new_alias_id == existing_alias_id
}

/// Only a pending token can be revoked. A verified token is
/// replaced, never revoked, and revoked / superseded / failed
/// tokens are already terminal, so revoking them again is a 409.
pub(crate) fn token_is_revocable(status: &str) -> bool {
    status == STATUS_PENDING
}

/// The rate-limit budget is exhausted once `max_attempts`
/// attempts are recorded inside the window.
pub(crate) fn rate_limit_exhausted(attempts_in_window: usize, max_attempts: u32) -> bool {
    attempts_in_window as u32 >= max_attempts
}

/// The 429 error for a domain whose verify budget is spent.
pub(crate) fn verify_rate_limit_error(
    domain: &str,
    settings: &VerificationSettings,
    recent: usize,
) -> UnifiedError {
    UnifiedError::new(
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
    )
}

pub(crate) async fn verify_domain_alias(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<VerifyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    // Stash the application context for the scheduler retry sweep:
    // the sweep's hook listener is synchronous and needs a bus to
    // emit the outcome events on (see `hooks.rs`). The first verify
    // call stashes it; the listener reports the sweep as skipped
    // until then.
    let _ = DOMAIN_VERIFICATION_CTX.set(stash_ctx(&**ctx));
    let result: UnifiedResult<VerifyOutcome> = (|| async {
        let who = caller(&req)?;
        require_permission(&ctx, &who.role, PERMISSION_VERIFY)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        require_body_tenant_match(&_tenant, &body.tenant_id)?;
        let domain = normalize_domain(&body.domain)?;
        let settings = current_settings();
        let mut token = current_token_for(&domain, body.alias_id)
            .ok_or_else(|| {
                UnifiedError::not_found(
                    "No verification token issued for this domain; issue a token first",
                )
            })?;
        require_record_writable_in_company_scope(&token.company_id, &scope)?;
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
            // The outcome takes `record_type`/`host` by value, so
            // clone them first for the `looked_up` payload below.
            let record_type = token.record_type.clone();
            let host = token.host.clone();
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
                    "host": host,
                    "record_type": record_type,
                    "expected_value_prefix": value_prefix(&settings.value_template, &host),
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
        if rate_limit_exhausted(recent, settings.max_attempts) {
            return Err(verify_rate_limit_error(&domain, &settings, recent));
        }
        let looked_up = json!({
            "host": token.host,
            "record_type": token.record_type,
            "expected_value_prefix": value_prefix(&settings.value_template, &token.host),
        });
        // One DNS check per request. The old retry loop performed up
        // to max_attempts checks with retry_delay between them inside
        // this request, blocking an Actix worker for up to
        // max_attempts * (verify_timeout + retry_delay) — about 90s
        // at the defaults. A verify call now performs exactly one
        // check and returns a pending outcome when the record is
        // not there yet; retries happen out of band through the
        // scheduler (kind `domain.verify.retry`, listener
        // `on_verify_retry_requested` in `hooks.rs`), or by calling
        // verify again after retry_delay_seconds.
        let recent_before = attempts_in_window(&domain, settings.rate_limit_window_seconds);
        // Re-check the budget immediately before the attempt, so a
        // concurrent verify call cannot push the window past
        // max_attempts: a call must not finish with more attempts
        // in the window than the documented budget allows.
        if rate_limit_exhausted(recent_before, settings.max_attempts) {
            return Err(verify_rate_limit_error(&domain, &settings, recent_before));
        }
        let attempt_number = recent_before as u32 + 1;
        let (matched, record_found, error) = check_once(&token, &settings).await;
        // Re-checked again after the DNS round-trip: another call
        // may have spent the budget while this one was looking up.
        // When the budget is spent, this attempt is not recorded
        // and the call fails with 429 instead.
        let recent_after_lookup = attempts_in_window(&domain, settings.rate_limit_window_seconds);
        if rate_limit_exhausted(recent_after_lookup, settings.max_attempts) {
            return Err(verify_rate_limit_error(
                &domain,
                &settings,
                recent_after_lookup,
            ));
        }
        // The budget is exhausted by this attempt exactly when the
        // window count reaches max_attempts once it is recorded:
        // that is the final attempt, and only it is recorded as
        // "failed". Intermediate retries stay "pending" and the
        // token stays pending so the next verify call can retry.
        let exhausted = rate_limit_exhausted(recent_after_lookup + 1, settings.max_attempts);
        let attempt_result = if matched {
            STATUS_VERIFIED.to_string()
        } else if exhausted {
            STATUS_FAILED.to_string()
        } else {
            STATUS_PENDING.to_string()
        };
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
        let now = Utc::now();
        if matched {
            let record_found = record_found.expect("a matched check carries its record");
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
                attempts_made: 1,
                verified_at: Some(now),
                error: None,
            })
        } else if exhausted {
            // Final failure: the budget is spent, so mark the token
            // failed and emit the failure event so the SaaS Domains
            // module can react.
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
                    "error": error,
                    "attempts": 1,
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
                attempts_made: 1,
                verified_at: None,
                error,
            })
        } else {
            // Intermediate retry: the token stays pending, so the
            // next verify call can retry within the budget.
            Ok(VerifyOutcome {
                domain,
                alias_id: token.alias_id,
                tenant_id: token.tenant_id,
                company_id: token.company_id,
                status: STATUS_PENDING.to_string(),
                record_type: token.record_type,
                host: token.host,
                record_found: None,
                looked_up,
                attempts_made: 1,
                verified_at: None,
                error,
            })
        }
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn list_attempts(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let who = caller(&req)?;
        require_permission(&ctx, &who.role, PERMISSION_READ)?;
        let scope = caller_company(&_tenant, &req)?;
        let mut items: Vec<VerificationAttempt> = attempts_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        // Attempts carry no company of their own: they are scoped
        // through their parent token. An attempt whose token is
        // missing is hidden (fail closed).
        items.retain(|row| {
            tokens_store()
                .get(&row.token_id)
                .map(|token| in_company_scope(&token.company_id, &scope))
                .unwrap_or(false)
        });
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

pub(crate) async fn get_status(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<StatusQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let who = caller(&req)?;
        require_permission(&ctx, &who.role, PERMISSION_READ)?;
        let scope = caller_company(&_tenant, &req)?;
        let domain = normalize_domain(&query.domain)?;
        // The status is derived from the domain's current token, so
        // an out-of-scope token must not leak its status: a scoped
        // caller asking about another company's domain sees the
        // token as missing, not its status.
        if let Some(token) = current_token_for(&domain, None) {
            require_record_in_company_scope(&token.company_id, &scope)?;
        }
        let (status, verified_at) = domain_status(&domain);
        Ok(json!({
            "domain": domain,
            "status": status,
            "verified_at": verified_at,
        }))
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supersede_requires_an_exact_alias_match() {
        let alias_a = Uuid::now_v7();
        let alias_b = Uuid::now_v7();
        assert!(supersedes_token(Some(alias_a), Some(alias_a)));
        assert!(supersedes_token(None, None));
        assert!(!supersedes_token(Some(alias_a), Some(alias_b)));
        // The old bug: issuing a token with no alias superseded
        // every pending token for the domain, whatever its alias.
        assert!(!supersedes_token(None, Some(alias_a)));
        assert!(!supersedes_token(Some(alias_a), None));
    }

    #[test]
    fn only_pending_tokens_are_revocable() {
        assert!(token_is_revocable(STATUS_PENDING));
        assert!(!token_is_revocable(STATUS_VERIFIED));
        assert!(!token_is_revocable("revoked"));
        assert!(!token_is_revocable("superseded"));
        assert!(!token_is_revocable(STATUS_FAILED));
    }

    #[test]
    fn rate_limit_budget_is_exhausted_at_max_attempts() {
        assert!(!rate_limit_exhausted(0, 5));
        assert!(!rate_limit_exhausted(4, 5));
        assert!(rate_limit_exhausted(5, 5));
        assert!(rate_limit_exhausted(6, 5));
    }

    #[test]
    fn rate_limit_error_is_429_and_names_the_domain() {
        let settings = default_settings();
        let error = verify_rate_limit_error("example.com", &settings, 5);
        assert_eq!(error.code, 429);
        assert!(error.message.contains("example.com"));
        assert_eq!(
            error.metadata.get("attempts_in_window").and_then(Value::as_u64),
            Some(5)
        );
    }
}
