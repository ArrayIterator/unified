//! HTTP handlers for KYC cases: creation, submission,
//! status reads, updates, deletion, and the audit trail.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    append_audit, caller, can_manage, case_event, case_view, created, err, load_case,
    load_subject_type_by_code, load_visible_case, ok, paginate, require_permission,
    require_subject_id, resolve_tenant, verify_audit_chain,
};
use super::state::KycState;
use super::types::{
    can_transition, CreateCaseRequest, KycCase, KycCaseStatus, UpdateCaseRequest,
    EVENT_CASE_SUBMITTED, PERM_KYC_MANAGE,
};

/// Path carrying a case id.
#[derive(Debug, Deserialize)]
struct CasePath {
    case_id: String,
}

/// Every required document-type code of the case's subject
/// type must have at least one linked document before the
/// case can be submitted.
fn check_required_documents(state: &KycState, case: &KycCase) -> UnifiedResult<()> {
    let subject_type = load_subject_type_by_code(state, &case.tenant_id, &case.subject_type)?;
    let documents = state.case_documents(case.id);
    let missing: Vec<&str> = subject_type
        .required_document_type_codes
        .iter()
        .map(String::as_str)
        .filter(|code| {
            !documents
                .iter()
                .any(|document| document.document_type_code == *code)
        })
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "Case cannot be submitted: missing required documents: {}",
            missing.join(", ")
        )))
    }
}

fn list_visible_cases(
    state: &KycState,
    ctx: &AppContext,
    tenant_id: &str,
    caller: &super::helpers::Caller,
    params: &PaginationParams,
) -> Vec<Value> {
    let is_manager = can_manage(ctx, &caller.role);
    let status_filter = params
        .filters
        .get("status")
        .and_then(|raw| KycCaseStatus::from_str(raw));
    let risk_filter = params.filters.get("risk_level").cloned();
    let subject_type_filter = params.filters.get("subject_type").cloned();
    let subject_filter = params.filters.get("subject").cloned();
    let search = params.search.clone().map(|value| value.to_lowercase());
    let mut items: Vec<KycCase> = state
        .cases
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|case| {
            if case.tenant_id != tenant_id {
                return false;
            }
            if !is_manager && case.subject_id != caller.actor && case.submitted_by != caller.actor {
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
    items.iter().map(|case| case_view(state, case)).collect()
}

// ---------------------------------------------------------------------------
// Case handlers (authenticated)
// ---------------------------------------------------------------------------

pub(crate) async fn list_cases(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let params = query.into_inner();
        let views = list_visible_cases(&state, &ctx, &tenant_id, &caller, &params);
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_case(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateCaseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let input = body.into_inner();
        let subject_type = load_subject_type_by_code(&state, &tenant_id, &input.subject_type)?;
        if !subject_type.active {
            return Err(UnifiedError::bad_request(format!(
                "Subject type \"{}\" is not active",
                subject_type.code
            )));
        }
        let subject_id = match &input.subject_id {
            Some(raw) if !raw.trim().is_empty() => require_subject_id(raw)?,
            _ => caller.actor.clone(),
        };
        // A manager may open a case for another subject; a plain
        // caller may only open one for themselves.
        if subject_id != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(
                "Only a manager may create a case for another subject",
            ));
        }
        let settings = state.current_settings(&tenant_id);
        let now = Utc::now();
        let case = KycCase {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            subject_id: subject_id.clone(),
            subject_type: subject_type.code.clone(),
            status: KycCaseStatus::Draft,
            risk_score: None,
            risk_level_id: None,
            risk_level_code: None,
            ai_verdict: None,
            submitted_by: caller.actor.clone(),
            reviewed_by: None,
            decision_reason: None,
            escalated: false,
            expires_at: Some(now + chrono::Duration::days(settings.verification_expiry_days)),
            created_at: now,
            updated_at: now,
        };
        state.cases.insert(case.id, case.clone());
        case_event(
            &state,
            "kyc.case.created",
            &case,
            &caller.actor,
            "created",
            Some(format!(
                "Case opened for subject type {}",
                case.subject_type
            )),
        );
        Ok(case_view(&state, &case))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_case(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CasePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let case = load_visible_case(&state, &ctx, &tenant_id, &caller, &path.case_id)?;
        Ok(case_view(&state, &case))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Lightweight status read for the subject: status, risk,
/// verdict summary, and expiry — the full case view is
/// `GET /cases/{id}`.
pub(crate) async fn get_case_status(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CasePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let case = load_visible_case(&state, &ctx, &tenant_id, &caller, &path.case_id)?;
        Ok(json!({
            "case_id": case.id.to_string(),
            "subject_id": case.subject_id,
            "subject_type": case.subject_type,
            "status": case.status.as_str(),
            "risk_score": case.risk_score,
            "risk_level_code": case.risk_level_code,
            "ai_verdict": case.ai_verdict,
            "escalated": case.escalated,
            "expires_at": case.expires_at,
            "updated_at": case.updated_at,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_case(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CasePath>,
    body: web::Json<UpdateCaseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let mut case = load_visible_case(&state, &ctx, &tenant_id, &caller, &path.case_id)?;
        if case.status != KycCaseStatus::Draft {
            return Err(UnifiedError::bad_request(
                "Only a draft case can be updated",
            ));
        }
        let input = body.into_inner();
        if let Some(subject_id) = &input.subject_id {
            let subject_id = require_subject_id(subject_id)?;
            if subject_id != caller.actor && !can_manage(&ctx, &caller.role) {
                return Err(UnifiedError::forbidden(
                    "Only a manager may move a case to another subject",
                ));
            }
            case.subject_id = subject_id;
        }
        if let Some(subject_type) = &input.subject_type {
            let row = load_subject_type_by_code(&state, &tenant_id, subject_type)?;
            if !row.active {
                return Err(UnifiedError::bad_request(format!(
                    "Subject type \"{}\" is not active",
                    row.code
                )));
            }
            case.subject_type = row.code;
        }
        case.updated_at = Utc::now();
        state.cases.insert(case.id, case.clone());
        append_audit(&state, &tenant_id, case.id, "updated", &caller.actor, None);
        Ok(case_view(&state, &case))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_case(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CasePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let case = load_visible_case(&state, &ctx, &tenant_id, &caller, &path.case_id)?;
        if case.status != KycCaseStatus::Draft {
            return Err(UnifiedError::bad_request(
                "Only a draft case can be deleted; a submitted case is decided or expired instead",
            ));
        }
        for document in state.case_documents(case.id) {
            state.documents.remove(&document.id);
        }
        for entry in state
            .audit_entries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.case_id == case.id)
        {
            state.audit_entries.remove(&entry.id);
        }
        state.cases.remove(&case.id);
        state.emit(
            "kyc.case.deleted",
            json!({
                "tenant_id": tenant_id,
                "case_id": case.id.to_string(),
                "subject_id": case.subject_id,
                "actor_id": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "case_id": case.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn submit_case(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CasePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let mut case = load_visible_case(&state, &ctx, &tenant_id, &caller, &path.case_id)?;
        if !can_transition(case.status, KycCaseStatus::Submitted) {
            return Err(UnifiedError::bad_request(format!(
                "Case cannot move from {} to submitted",
                case.status.as_str()
            )));
        }
        // A re-submission after more information was requested
        // must satisfy the required documents again.
        check_required_documents(&state, &case)?;
        if let Some(expires_at) = case.expires_at {
            if expires_at <= Utc::now() {
                return Err(UnifiedError::bad_request(
                    "Case has expired; open a new case instead",
                ));
            }
        }
        case.status = KycCaseStatus::Submitted;
        case.updated_at = Utc::now();
        state.cases.insert(case.id, case.clone());
        case_event(
            &state,
            EVENT_CASE_SUBMITTED,
            &case,
            &caller.actor,
            "submitted",
            None,
        );
        state.notify(
            "kyc.case.submitted",
            &case.subject_id,
            "Identity verification submitted",
            "Your identity verification case was submitted and is waiting for review.",
            Some(format!("/kyc/cases/{}", case.id)),
        );
        Ok(case_view(&state, &case))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// List the hash-chained audit trail of a case (paginated,
/// filter by action) with the chain verification result.
pub(crate) async fn list_case_audit(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CasePath>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let case = load_visible_case(&state, &ctx, &tenant_id, &caller, &path.case_id)?;
        let params = query.into_inner();
        let action_filter = params.filters.get("action").cloned();
        let mut entries: Vec<_> = state
            .audit_entries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                entry.case_id == case.id
                    && action_filter
                        .as_deref()
                        .map(|action| entry.action == action)
                        .unwrap_or(true)
            })
            .collect();
        entries.sort_by_key(|entry| entry.created_at);
        let chain_valid = verify_audit_chain(&entries);
        let page = paginate(
            entries
                .iter()
                .map(|entry| serde_json::to_value(entry).unwrap_or(Value::Null))
                .collect(),
            &params,
        );
        let mut value = serde_json::to_value(page).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut value {
            map.insert("audit_chain_valid".to_string(), Value::Bool(chain_valid));
        }
        Ok(value)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Load a case for manager-only writes. Kept separate from
/// `load_visible_case` so review handlers fail closed on the
/// manage capability before touching the row.
pub(crate) fn load_case_for_manager(
    state: &KycState,
    ctx: &AppContext,
    tenant_id: &str,
    caller: &super::helpers::Caller,
    case_id: &str,
) -> UnifiedResult<KycCase> {
    require_permission(ctx, &caller.role, PERM_KYC_MANAGE)?;
    load_case(state, tenant_id, case_id)
}
