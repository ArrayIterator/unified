//! HTTP handlers for engineering change orders: CRUD, the
//! status machine, and data-driven approval steps.

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
    caller, caller_company, created, decisions_for, eco_status_from_str, eco_view, err, load_eco,
    next_eco_code, ok, paginate, require_permission, require_tenant, require_text, resolve_tenant,
    validate_approval_steps, validate_id_list, validate_impact_level, validate_uuid_list,
};
use super::state::PlmState;
use super::types::{
    eco_approvals_complete, eco_can_transition, ApprovalDecision, CreateApprovalRequest,
    CreateEcoRequest, EcoStatus, EcoTransitionRequest, PlmApproval, PlmEco, UpdateEcoRequest,
    EVENT_ECO_APPROVED, EVENT_ECO_CREATED, EVENT_ECO_IMPLEMENTED, MAX_DESCRIPTION_LEN,
    MAX_SUMMARY_LEN, PERM_PLM_MANAGE,
};

/// Path carrying an ECO id.
#[derive(Debug, Deserialize)]
struct EcoPath {
    eco_id: String,
}

fn validate_eco_type(state: &PlmState, eco_type: &str) -> UnifiedResult<String> {
    let eco_type = eco_type.trim().to_lowercase();
    if eco_type.is_empty() {
        return Err(UnifiedError::bad_request("eco_type must not be empty"));
    }
    let settings = state.current_settings();
    if !settings.eco_types.iter().any(|item| item == &eco_type) {
        return Err(UnifiedError::bad_request(format!(
            "eco_type must be one of the tenant's configured types: {}",
            settings.eco_types.join(", ")
        )));
    }
    Ok(eco_type)
}

fn validate_affected_versions(
    state: &PlmState,
    tenant_id: &str,
    version_ids: &[Uuid],
) -> UnifiedResult<()> {
    for version_id in version_ids {
        match state.versions.get(version_id) {
            Some(entry) if entry.value().tenant_id == tenant_id => {}
            _ => {
                return Err(UnifiedError::not_found(format!(
                    "Version not found: {version_id}"
                )));
            }
        }
    }
    Ok(())
}

// ECO handlers (authenticated)

pub(crate) async fn list_ecos(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let type_filter = params.filters.get("eco_type").cloned();
        let product_filter = params.filters.get("product_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<PlmEco> = state
            .ecos
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|eco| {
                if eco.tenant_id != tenant_id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if eco.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(eco_type) = &type_filter {
                    if eco.eco_type != *eco_type {
                        return false;
                    }
                }
                if let Some(product_id) = &product_filter {
                    if !eco.affected_product_ids.iter().any(|id| id == product_id) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        eco.code,
                        eco.title,
                        eco.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let views: Vec<Value> = items.iter().map(|eco| eco_view(&state, eco)).collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_eco(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateEcoRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PLM_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", 200)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let eco_type = validate_eco_type(&state, &input.eco_type)?;
        let affected_product_ids =
            validate_id_list(&input.affected_product_ids, "affected_product_ids")?;
        let affected_version_ids = validate_uuid_list(&input.affected_version_ids)?;
        validate_affected_versions(&state, &tenant_id, &affected_version_ids)?;
        let linked_document_ids =
            validate_id_list(&input.linked_document_ids, "linked_document_ids")?;
        let impact_level = validate_impact_level(&input.impact_level)?;
        let impact_assessment = match &input.impact_assessment {
            Some(raw) => Some(require_text(raw, "impact_assessment", MAX_SUMMARY_LEN)?),
            None => None,
        };
        let approval_steps = validate_approval_steps(&input.approval_steps)?;
        let now = Utc::now();
        let eco = PlmEco {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            code: next_eco_code(&state, &tenant_id),
            title,
            description,
            eco_type,
            status: EcoStatus::Draft,
            affected_product_ids,
            affected_version_ids,
            linked_document_ids,
            impact_level,
            impact_assessment,
            approval_steps,
            requested_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.ecos.insert(eco.id, eco.clone());
        state.record_audit(
            &tenant_id,
            "eco",
            eco.id,
            "created",
            &caller.actor,
            Some(eco.code.clone()),
            None,
            Some(eco.status.as_str().to_string()),
        );
        state.emit(
            EVENT_ECO_CREATED,
            json!({
                "tenant_id": tenant_id,
                "eco_id": eco.id.to_string(),
                "code": eco.code,
                "eco_type": eco.eco_type,
                "status": eco.status.as_str(),
                "requested_by": caller.actor,
            }),
        );
        Ok(eco_view(&state, &eco))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_eco(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<EcoPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let eco = load_eco(&state, &tenant_id, &path.eco_id)?;
        Ok(eco_view(&state, &eco))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_eco(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<EcoPath>,
    body: web::Json<UpdateEcoRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PLM_MANAGE)?;
        let mut eco = load_eco(&state, &tenant_id, &path.eco_id)?;
        // Only draft and rejected ECOs may be edited; once a
        // change is in review the record is frozen except
        // through the transition endpoint.
        if !matches!(eco.status, EcoStatus::Draft | EcoStatus::Rejected) {
            return Err(UnifiedError::bad_request(
                "only draft or rejected ECOs may be edited",
            ));
        }
        let input = body.into_inner();
        if let Some(raw) = &input.title {
            eco.title = require_text(raw, "title", 200)?;
        }
        if let Some(description) = &input.description {
            eco.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.eco_type {
            eco.eco_type = validate_eco_type(&state, raw)?;
        }
        if let Some(ids) = &input.affected_product_ids {
            eco.affected_product_ids = validate_id_list(ids, "affected_product_ids")?;
        }
        if let Some(ids) = &input.affected_version_ids {
            let ids = validate_uuid_list(ids)?;
            validate_affected_versions(&state, &tenant_id, &ids)?;
            eco.affected_version_ids = ids;
        }
        if let Some(ids) = &input.linked_document_ids {
            eco.linked_document_ids = validate_id_list(ids, "linked_document_ids")?;
        }
        if let Some(level) = &input.impact_level {
            eco.impact_level = validate_impact_level(level)?;
        }
        if let Some(assessment) = &input.impact_assessment {
            eco.impact_assessment = match assessment {
                Some(raw) => Some(require_text(raw, "impact_assessment", MAX_SUMMARY_LEN)?),
                None => None,
            };
        }
        if let Some(steps) = &input.approval_steps {
            eco.approval_steps = validate_approval_steps(steps)?;
        }
        eco.updated_at = Utc::now();
        state.ecos.insert(eco.id, eco.clone());
        state.record_audit(
            &tenant_id,
            "eco",
            eco.id,
            "updated",
            &caller.actor,
            Some(eco.code.clone()),
            None,
            Some(eco.status.as_str().to_string()),
        );
        Ok(eco_view(&state, &eco))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_eco(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<EcoPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PLM_MANAGE)?;
        let eco = load_eco(&state, &tenant_id, &path.eco_id)?;
        if !matches!(eco.status, EcoStatus::Draft | EcoStatus::Rejected) {
            return Err(UnifiedError::bad_request(
                "only draft or rejected ECOs may be deleted",
            ));
        }
        if state.bom_versions.iter().any(|entry| {
            entry.value().tenant_id == tenant_id && entry.value().eco_id == Some(eco.id)
        }) {
            return Err(UnifiedError::bad_request(
                "ECO is linked by BOM versions; unlink them first",
            ));
        }
        let approval_ids: Vec<Uuid> = state
            .approvals
            .iter()
            .filter(|entry| entry.value().entity_type == "eco" && entry.value().entity_id == eco.id)
            .map(|entry| *entry.key())
            .collect();
        for approval_id in approval_ids {
            state.approvals.remove(&approval_id);
        }
        state.ecos.remove(&eco.id);
        state.record_audit(
            &tenant_id,
            "eco",
            eco.id,
            "deleted",
            &caller.actor,
            Some(eco.code.clone()),
            Some(eco.status.as_str().to_string()),
            None,
        );
        Ok(json!({ "deleted": true, "eco_id": eco.id.to_string(), "code": eco.code }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Move an ECO through its status machine. Moving to
/// `approved` requires every required approval step to hold an
/// approved decision; moving to `implemented` notifies the
/// requester. Every transition writes an audit entry and emits
/// an event.
pub(crate) async fn transition_eco(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<EcoPath>,
    body: web::Json<EcoTransitionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PLM_MANAGE)?;
        let mut eco = load_eco(&state, &tenant_id, &path.eco_id)?;
        let input = body.into_inner();
        let target = eco_status_from_str(input.to_status.trim().to_lowercase().as_str())?;
        if !eco_can_transition(eco.status, target) {
            return Err(UnifiedError::bad_request(format!(
                "ECO in status {} cannot move to {}",
                eco.status.as_str(),
                target.as_str()
            )));
        }
        if target == EcoStatus::Approved {
            let decisions = decisions_for(&state, "eco", eco.id);
            if !eco_approvals_complete(&eco.approval_steps, &decisions) {
                return Err(UnifiedError::bad_request(
                    "not all required approval steps are approved",
                ));
            }
        }
        let from_status = eco.status;
        eco.status = target;
        eco.updated_at = Utc::now();
        state.ecos.insert(eco.id, eco.clone());
        state.record_audit(
            &tenant_id,
            "eco",
            eco.id,
            "status_changed",
            &caller.actor,
            input.comment.clone(),
            Some(from_status.as_str().to_string()),
            Some(target.as_str().to_string()),
        );
        let event = match target {
            EcoStatus::Approved => Some(EVENT_ECO_APPROVED),
            EcoStatus::Implemented => Some(EVENT_ECO_IMPLEMENTED),
            _ => None,
        };
        if let Some(event) = event {
            state.emit(
                event,
                json!({
                    "tenant_id": tenant_id,
                    "eco_id": eco.id.to_string(),
                    "code": eco.code,
                    "from_status": from_status.as_str(),
                    "to_status": target.as_str(),
                    "actor_id": caller.actor,
                    "affected_product_ids": eco.affected_product_ids,
                    "affected_version_ids": eco.affected_version_ids,
                }),
            );
        } else {
            state.emit(
                "plm.eco.status_changed",
                json!({
                    "tenant_id": tenant_id,
                    "eco_id": eco.id.to_string(),
                    "code": eco.code,
                    "from_status": from_status.as_str(),
                    "to_status": target.as_str(),
                    "actor_id": caller.actor,
                }),
            );
        }
        if target == EcoStatus::Implemented {
            state.notify(
                "eco_implemented",
                &eco.requested_by,
                &format!("ECO {} implemented", eco.code),
                &format!(
                    "Engineering change order {} has been implemented.",
                    eco.code
                ),
                None,
            );
        }
        Ok(eco_view(&state, &eco))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ECO approvals (authenticated)

pub(crate) async fn list_eco_approvals(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<EcoPath>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let eco = load_eco(&state, &tenant_id, &path.eco_id)?;
        let params = query.into_inner();
        let decision_filter = params.filters.get("decision").cloned();
        let mut items: Vec<PlmApproval> = state
            .approvals
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|approval| {
                if approval.tenant_id != tenant_id
                    || approval.entity_type != "eco"
                    || approval.entity_id != eco.id
                {
                    return false;
                }
                if let Some(decision) = &decision_filter {
                    if approval.decision.as_str() != decision.as_str() {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|approval| (approval.step_index, approval.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Decide one approval step on an ECO. The step must exist in
/// the ECO's data-driven `approval_steps`, must not have been
/// decided already, and the caller must hold the manage
/// capability. The decision is `approved` or `rejected`. A
/// rejection does not by itself move the ECO - the ECO moves
/// to `rejected` through the transition endpoint - but a
/// rejected required step blocks approval until the ECO is
/// reworked.
pub(crate) async fn create_eco_approval(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<EcoPath>,
    body: web::Json<CreateApprovalRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PlmApproval> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PLM_MANAGE)?;
        let eco = load_eco(&state, &tenant_id, &path.eco_id)?;
        let input = body.into_inner();
        let decision = ApprovalDecision::from_str(input.decision.trim().to_lowercase().as_str())
            .filter(|decision| *decision != ApprovalDecision::Pending)
            .ok_or_else(|| UnifiedError::bad_request("decision must be approved or rejected"))?;
        if input.step_index < 0 || input.step_index as usize >= eco.approval_steps.len() {
            return Err(UnifiedError::bad_request(format!(
                "step_index {} does not exist on this ECO ({} step(s))",
                input.step_index,
                eco.approval_steps.len()
            )));
        }
        let step = eco.approval_steps[input.step_index as usize].clone();
        if caller.role != step.approver_role {
            return Err(UnifiedError::forbidden(format!(
                "step \"{}\" requires role \"{}\", caller holds \"{}\"",
                step.name, step.approver_role, caller.role
            )));
        }
        if state.approvals.iter().any(|entry| {
            let approval = entry.value();
            approval.entity_type == "eco"
                && approval.entity_id == eco.id
                && approval.step_index == input.step_index
                && approval.decision != ApprovalDecision::Pending
        }) {
            return Err(UnifiedError::conflict(format!(
                "step {} has already been decided",
                input.step_index
            )));
        }
        let comment = match &input.comment {
            Some(raw) => Some(require_text(raw, "comment", 2000)?),
            None => None,
        };
        let now = Utc::now();
        let approval = PlmApproval {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            entity_type: "eco".to_string(),
            entity_id: eco.id,
            step_index: input.step_index,
            step_name: step.name.clone(),
            approver_role: step.approver_role.clone(),
            approver_id: Some(caller.actor.clone()),
            decision,
            comment: comment.clone(),
            decided_at: Some(now),
            created_at: now,
            updated_at: now,
        };
        state.approvals.insert(approval.id, approval.clone());
        state.record_audit(
            &tenant_id,
            "eco",
            eco.id,
            "approval_decided",
            &caller.actor,
            comment,
            Some(format!("step {}", input.step_index)),
            Some(decision.as_str().to_string()),
        );
        state.emit(
            "plm.approval.decided",
            json!({
                "tenant_id": tenant_id,
                "entity_type": "eco",
                "entity_id": eco.id.to_string(),
                "code": eco.code,
                "approval_id": approval.id.to_string(),
                "step_index": input.step_index,
                "step_name": step.name,
                "decision": decision.as_str(),
                "approver_id": caller.actor,
            }),
        );
        Ok(approval)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}
