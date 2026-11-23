//! Compliance Center module — GDPR / ISO support: data export — Consent handlers (append-only).
//!
//! Split from `src/modules/compliance.rs`; behaviour unchanged.

use super::breaches::*;
use super::checklists::*;
use super::constants::*;
use super::deletion_listeners::*;
use super::deletions::*;
use super::exports::*;
use super::helpers::*;
use super::module::*;
use super::records::*;
use super::requests::*;
use super::retention::*;
use super::state::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn list_consents(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        let purpose_filter = params.filters.get("purpose").cloned();
        let granted_filter = params
            .filters
            .get("granted")
            .and_then(|raw| raw.parse::<bool>().ok());
        let mut items: Vec<ConsentLog> = state
            .consents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|consent| {
                // A requester sees only their own consent history
                // unless they hold the manage capability.
                if !is_manager && consent.user_id != caller.actor {
                    return false;
                }
                if let Some(user) = &user_filter {
                    if &consent.user_id != user {
                        return false;
                    }
                }
                if let Some(purpose) = &purpose_filter {
                    if &consent.purpose != purpose {
                        return false;
                    }
                }
                if let Some(granted) = granted_filter {
                    if consent.granted != granted {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|consent| std::cmp::Reverse(consent.recorded_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn record_consent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    body: web::Json<RecordConsentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ConsentLog> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let input = body.into_inner();
        let user_id = match &input.user_id {
            Some(raw) => {
                if !is_manager && raw != &caller.actor {
                    return Err(UnifiedError::forbidden(
                        "Recording consent for another user requires the manage capability",
                    ));
                }
                require_text(raw, "user_id", MAX_NAME_LEN)?
            }
            None => caller.actor.clone(),
        };
        let purpose = require_text(&input.purpose, "purpose", MAX_PURPOSE_LEN)?;
        let version = require_text(
            &input.consent_text_version,
            "consent_text_version",
            MAX_NAME_LEN,
        )?;
        let record = ConsentLog {
            id: Uuid::now_v7(),
            user_id: user_id.clone(),
            purpose: purpose.clone(),
            consent_text_version: version.clone(),
            granted: input.granted,
            recorded_at: Utc::now(),
        };
        state.consents.insert(record.id, record.clone());
        // Withdrawal is recorded the same way, as a new row with
        // granted=false; the old row is never overwritten.
        let event_name = if input.granted {
            "compliance.consent.recorded"
        } else {
            "compliance.consent.withdrawn"
        };
        state.emit(
            event_name,
            json!({
                "consent_id": record.id.to_string(),
                "user_id": user_id,
                "purpose": purpose,
                "consent_text_version": version,
                "granted": input.granted,
            }),
        );
        Ok(record)
    })();
    match result {
        Ok(record) => created(record),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn get_consent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ConsentLog> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let consent = load_consent(&state, &path.into_inner())?;
        check_own_or_manage(&ctx, &caller.role, &consent.user_id, &caller.actor)?;
        Ok(consent)
    })();
    match result {
        Ok(consent) => ok(consent),
        Err(error) => err(error, debug),
    }
}
