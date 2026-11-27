//! HTTP handlers for the admin review queue: listing cases
//! for review with counts, and the human decisions - approve,
//! reject, request more information, escalate, and expire.
//!
//! No decision here is taken by the AI: the AI verdict feeds
//! this queue, and a human admin decides. The one exception
//! is the admin-enabled auto-approve policy, which runs in
//! the verify handler, not here.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;

use super::handlers_cases::load_case_for_manager;
use super::helpers::{
    caller, case_event, case_view, err, ok, paginate, require_reason, resolve_tenant,
};
use super::state::KycState;
use super::types::{
    can_transition, DecisionRequest, KycCaseStatus, RequestMoreInfoRequest, EVENT_CASE_APPROVED,
    EVENT_CASE_ESCALATED, EVENT_CASE_EXPIRED, EVENT_CASE_MORE_INFO_REQUESTED, EVENT_CASE_REJECTED,
};

/// Path carrying a case id.
#[derive(Debug, Deserialize)]
struct CasePath {
    case_id: String,
}

/// Shared decision runner: loads the case as a manager,
/// checks the status machine, applies the decision, audits
/// it, emits the event, and notifies the subject.
fn decide(
    state: &KycState,
    case: &mut super::types::KycCase,
    actor: &str,
    to: KycCaseStatus,
    event: &str,
    action: &str,
    reason: Option<String>,
) -> UnifiedResult<()> {
    if !can_transition(case.status, to) {
        return Err(UnifiedError::bad_request(format!(
            "Case cannot move from {} to {}",
            case.status.as_str(),
            to.as_str()
        )));
    }
    case.status = to;
    case.reviewed_by = Some(actor.to_string());
    case.decision_reason = reason.clone();
    case.updated_at = Utc::now();
    state.cases.insert(case.id, case.clone());
    case_event(state, event, case, actor, action, reason.clone());
    Ok(())
}

fn notify_subject(state: &KycState, case: &super::types::KycCase, title: &str, body: &str) {
    state.notify(
        "kyc.case.decision",
        &case.subject_id,
        title,
        body,
        Some(format!("/kyc/cases/{}", case.id)),
    );
}

// Admin review queue (manager)

/// The review queue: every non-terminal case the tenant has,
/// filterable like the case list (status, risk level,
/// subject, subject type), plus counts by status so the queue
/// can render its tabs without a second call.
pub(crate) async fn list_queue(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, super::types::PERM_KYC_MANAGE)?;
        let params = query.into_inner();
        let status_filter = params
            .filters
            .get("status")
            .and_then(|raw| KycCaseStatus::from_str(raw));
        let risk_filter = params.filters.get("risk_level").cloned();
        let subject_type_filter = params.filters.get("subject_type").cloned();
        let subject_filter = params.filters.get("subject").cloned();
        let search = params.search.clone().map(|value| value.to_lowercase());
        let mut items: Vec<super::types::KycCase> = state
            .cases
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|case| {
                if case.tenant_id != tenant_id {
                    return false;
                }
                if let Some(status) = status_filter {
                    if case.status != status {
                        return false;
                    }
                }
                if let Some(risk) = &risk_filter {
                    if case.risk_level_code.as_deref() != Some(risk.as_str()) {
                        return false;
                    }
                }
                if let Some(subject_type) = &subject_type_filter {
                    if case.subject_type != *subject_type {
                        return false;
                    }
                }
                if let Some(subject) = &subject_filter {
                    if case.subject_id != *subject {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    if !case.subject_id.to_lowercase().contains(needle.as_str()) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let mut by_status = serde_json::Map::new();
        for case in state
            .cases
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|case| case.tenant_id == tenant_id)
        {
            let count = by_status
                .get(case.status.as_str())
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            by_status.insert(case.status.as_str().to_string(), json!(count + 1));
        }
        let views: Vec<Value> = items.iter().map(|case| case_view(&state, case)).collect();
        let mut page = paginate(views, &params).to_json();
        if let Value::Object(map) = &mut page {
            map.insert("counts_by_status".to_string(), Value::Object(by_status));
        }
        Ok(page)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn approve_case(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CasePath>,
    body: web::Json<DecisionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let mut case = load_case_for_manager(&state, &ctx, &tenant_id, &caller, &path.case_id)?;
        let reason = require_reason(&body.reason)?;
        decide(
            &state,
            &mut case,
            &caller.actor,
            KycCaseStatus::Approved,
            EVENT_CASE_APPROVED,
            "approved",
            reason,
        )?;
        notify_subject(
            &state,
            &case,
            "Identity verification approved",
            "Your identity verification was approved.",
        );
        Ok(case_view(&state, &case))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn reject_case(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CasePath>,
    body: web::Json<DecisionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let mut case = load_case_for_manager(&state, &ctx, &tenant_id, &caller, &path.case_id)?;
        let reason = require_reason(&body.reason)?;
        if reason.as_deref().unwrap_or("").trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A reason is required to reject a case",
            ));
        }
        decide(
            &state,
            &mut case,
            &caller.actor,
            KycCaseStatus::Rejected,
            EVENT_CASE_REJECTED,
            "rejected",
            reason,
        )?;
        notify_subject(
            &state,
            &case,
            "Identity verification rejected",
            "Your identity verification was rejected. See the case for the reason.",
        );
        Ok(case_view(&state, &case))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn request_more_info(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CasePath>,
    body: web::Json<RequestMoreInfoRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let mut case = load_case_for_manager(&state, &ctx, &tenant_id, &caller, &path.case_id)?;
        let input = body.into_inner();
        let reason = require_reason(&Some(input.reason.clone()))?
            .ok_or_else(|| UnifiedError::bad_request("A reason is required"))?;
        if reason.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A reason is required to request more information",
            ));
        }
        // Requested document kinds must be known, active
        // document types of this tenant.
        for code in &input.requested_document_type_codes {
            let row = super::helpers::load_document_type_by_code(&state, &tenant_id, code)?;
            if !row.active {
                return Err(UnifiedError::bad_request(format!(
                    "Document type \"{code}\" is not active"
                )));
            }
        }
        let detail = if input.requested_document_type_codes.is_empty() {
            reason.clone()
        } else {
            format!(
                "{reason} (requested documents: {})",
                input.requested_document_type_codes.join(", ")
            )
        };
        decide(
            &state,
            &mut case,
            &caller.actor,
            KycCaseStatus::MoreInfoRequested,
            EVENT_CASE_MORE_INFO_REQUESTED,
            "more_info_requested",
            Some(detail.clone()),
        )?;
        notify_subject(
            &state,
            &case,
            "More information needed for verification",
            &format!("Your identity verification needs more information: {detail}"),
        );
        Ok(case_view(&state, &case))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn escalate_case(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CasePath>,
    body: web::Json<DecisionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let mut case = load_case_for_manager(&state, &ctx, &tenant_id, &caller, &path.case_id)?;
        if case.status.is_terminal() {
            return Err(UnifiedError::bad_request(format!(
                "A {} case cannot be escalated",
                case.status.as_str()
            )));
        }
        if case.escalated {
            return Err(UnifiedError::bad_request("Case is already escalated"));
        }
        let reason = require_reason(&body.reason)?;
        case.escalated = true;
        case.updated_at = Utc::now();
        state.cases.insert(case.id, case.clone());
        case_event(
            &state,
            EVENT_CASE_ESCALATED,
            &case,
            &caller.actor,
            "escalated",
            reason.clone(),
        );
        Ok(case_view(&state, &case))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn expire_case(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CasePath>,
    body: web::Json<DecisionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let mut case = load_case_for_manager(&state, &ctx, &tenant_id, &caller, &path.case_id)?;
        let reason = require_reason(&body.reason)?;
        decide(
            &state,
            &mut case,
            &caller.actor,
            KycCaseStatus::Expired,
            EVENT_CASE_EXPIRED,
            "expired",
            reason,
        )?;
        notify_subject(
            &state,
            &case,
            "Identity verification expired",
            "Your identity verification case expired. Please open a new case.",
        );
        Ok(case_view(&state, &case))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
