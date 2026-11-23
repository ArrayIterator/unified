// Handlers: consent types and consent records.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;

use super::*;

// ---------------------------------------------------------------------------
// Handlers: consent types and consent records
// ---------------------------------------------------------------------------

pub(crate) async fn list_consent_types(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ConsentTypeDef>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let params = query.into_inner();
        let mut items: Vec<ConsentTypeDef> = state
            .consent_types
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| matches_search(&format!("{} {}", record.slug, record.name), &params))
            .collect();
        items.sort_by(|a, b| a.slug.cmp(&b.slug));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_consent_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<CreateConsentTypeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ConsentTypeDef> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_ADMIN)?;
        let input = body.into_inner();
        if !valid_slug(&input.slug) {
            return Err(UnifiedError::bad_request(
                "slug must be a lowercase slug",
            ));
        }
        let name = require_text(&input.name, "name", 128)?;
        let description = input.description.trim().to_string();
        if description.len() > 500 {
            return Err(UnifiedError::bad_request(
                "description must be at most 500 characters",
            ));
        }
        if state.consent_types.contains_key(&input.slug) {
            return Err(UnifiedError::conflict(format!(
                "Consent type \"{}\" already exists",
                input.slug
            )));
        }
        let record = ConsentTypeDef {
            slug: input.slug.clone(),
            name,
            description,
            created_at: now(),
        };
        state
            .consent_types
            .insert(input.slug.clone(), record.clone());
        state.audit(
            EVENT_PRIVACY_CONSENT_CHANGED,
            &caller.actor,
            "privacy.consent_type.created",
            &format!("consent-type:{}", record.slug),
            json!({ "consent_type": record.slug }),
        );
        Ok(record)
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) fn record_consent_change(
    state: &PrivacyState,
    record: &ConsentRecord,
    action: &str,
    actor: &str,
) {
    let entry = ConsentHistoryEntry {
        id: new_id(),
        consent_id: record.id.clone(),
        user_id: record.user_id.clone(),
        consent_type: record.consent_type.clone(),
        action: action.to_string(),
        actor: actor.to_string(),
        created_at: now(),
    };
    state
        .consent_history
        .insert(entry.id.clone(), entry);
    let event = if action == "granted" {
        EVENT_COMPLIANCE_CONSENT_GRANTED
    } else {
        EVENT_COMPLIANCE_CONSENT_WITHDRAWN
    };
    state.emit(
        event,
        json!({
            "user_id": record.user_id,
            "consent_type": record.consent_type,
            "consent_id": record.id,
            "actor_id": actor,
            "expires_at": record.expires_at,
        }),
    );
    state.audit(
        EVENT_PRIVACY_CONSENT_CHANGED,
        actor,
        format!("privacy.consent.{action}").as_str(),
        &format!("user:{}", record.user_id),
        json!({
            "user_id": record.user_id,
            "consent_type": record.consent_type,
            "consent_id": record.id,
            "granted": record.granted,
            "expires_at": record.expires_at,
        }),
    );
}

pub(crate) async fn grant_consent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<GrantConsentInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<(ConsentRecord, bool)> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", 256)?;
        if !valid_slug(&input.consent_type) {
            return Err(UnifiedError::bad_request(
                "consent_type must be a lowercase slug",
            ));
        }
        if !state.consent_types.contains_key(&input.consent_type) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown consent type \"{}\"",
                input.consent_type
            )));
        }
        // Self-service: the subject user may grant or withdraw
        // their own consent without a write capability; anyone
        // else needs privacy:write.
        if caller.actor != user_id {
            require_permission(&ctx, &caller.role, PERM_PRIVACY_WRITE)?;
        }
        if let Some(expires_at) = input.expires_at {
            if expires_at <= now() {
                return Err(UnifiedError::bad_request(
                    "expires_at must be in the future",
                ));
            }
        }
        let source = input
            .source
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("api")
            .to_string();
        if source.len() > 128 {
            return Err(UnifiedError::bad_request(
                "source must be at most 128 characters",
            ));
        }
        // The find-or-create runs under the consent write lock
        // in `PrivacyState::apply_consent_grant`, so concurrent
        // grants for the same (user_id, consent_type) pair can
        // never create duplicate records.
        let (record, is_new) = state.apply_consent_grant(
            &user_id,
            &input.consent_type,
            input.granted,
            input.expires_at,
            &source,
            &caller.actor,
        );
        Ok((record, is_new))
    })();
    match result {
        Ok((record, is_new)) => json::success_response(record, if is_new { 201 } else { 200 }),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_consents(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ConsentRecord>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let params = query.into_inner();
        let current = now();
        let mut items: Vec<ConsentRecord> = state
            .consents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                matches_search(&format!("{} {}", record.user_id, record.consent_type), &params)
                    && params
                        .filters
                        .get("user_id")
                        .is_none_or(|user_id| record.user_id == *user_id)
                    && params
                        .filters
                        .get("consent_type")
                        .is_none_or(|consent_type| record.consent_type == *consent_type)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| record.status(current) == status)
            })
            .collect();
        sort_desc_by_created(&mut items, |record| record.created_at);
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_current_consent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<CurrentConsentQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let input = query.into_inner();
        let record = state
            .consents
            .iter()
            .find(|entry| {
                entry.value().user_id == input.user_id
                    && entry.value().consent_type == input.consent_type
            })
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No consent record for user \"{}\" and type \"{}\"",
                    input.user_id, input.consent_type
                ))
            })?;
        let current = now();
        Ok(json!({
            "user_id": record.user_id,
            "consent_type": record.consent_type,
            "consent_id": record.id,
            "status": record.status(current),
            "granted": record.is_currently_granted(current),
            "granted_at": record.granted_at,
            "withdrawn_at": record.withdrawn_at,
            "expires_at": record.expires_at,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_consent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ConsentRecord> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        state
            .consents
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Consent record not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn withdraw_consent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ConsentRecord> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        // Load first so the permission check can compare the
        // caller against the subject user; the record itself is
        // re-loaded under the consent write lock inside
        // `withdraw_consent_record` (user_id never changes after
        // creation, so the check stays valid).
        let record = state
            .consents
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Consent record not found"))?;
        // Self-service: the subject user may withdraw their own
        // consent; anyone else needs privacy:write.
        if caller.actor != record.user_id {
            require_permission(&ctx, &caller.role, PERM_PRIVACY_WRITE)?;
        }
        // Idempotent no-op: an already-withdrawn record is
        // returned unchanged (HTTP 200) with no new history
        // entry; see `PrivacyState::withdraw_consent_record`.
        state.withdraw_consent_record(path.as_str(), &caller.actor)
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_consent_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ConsentHistoryEntry>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let consent_id = path.into_inner();
        if !state.consents.contains_key(&consent_id) {
            return Err(UnifiedError::not_found("Consent record not found"));
        }
        let params = query.into_inner();
        let mut items: Vec<ConsentHistoryEntry> = state
            .consent_history
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                entry.consent_id == consent_id
                    && params
                        .filters
                        .get("action")
                        .is_none_or(|action| entry.action == *action)
            })
            .collect();
        sort_desc_by_created(&mut items, |entry| entry.created_at);
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}
