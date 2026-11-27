// Generic Approvals - Approval request handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::TenantContext as TenancyContext;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_definitions::*;
use super::handlers_dashboards::*;
use super::hooks::*;
use super::module::*;

// Requests - creation

/// Shared request creation used by the HTTP endpoint and by the
/// `approval.requested` hook handler. A definition with no active
/// steps is rejected: a request created against it would sit in the
/// `requested` status forever, since deciding and expiry only accept
/// `in_progress` requests and nothing ever advances it.
pub(crate) fn create_request_record(
    state: &ApprovalsState,
    definition: &ApprovalDefinition,
    module: String,
    record_type: String,
    record_id: String,
    requester_id: String,
    summary: String,
    amount_minor: Option<i64>,
    currency: Option<String>,
    company_id: Option<String>,
) -> UnifiedResult<ApprovalRequest> {
    let settings = state.settings();
    let now = Utc::now();
    let steps = steps_for_definition(state, definition.id);
    if steps.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "Approval definition {} has no active steps; add a step before requesting approval",
            definition.id
        )));
    }
    let request = ApprovalRequest {
        id: Uuid::new_v4(),
        definition_id: definition.id,
        module,
        record_type,
        record_id,
        requester_id,
        summary,
        amount_minor,
        currency,
        status: STATUS_IN_PROGRESS.to_string(),
        current_step_index: 0,
        round: 1,
        escalation_count: 0,
        last_escalated_at: None,
        expires_at: deadline_for(definition, &settings, now),
        decided_at: None,
        decided_by: None,
        company_id,
        created_at: now,
        updated_at: now,
    };
    state.requests.insert(request.id, request.clone());
    state.emit(
        APPROVAL_CREATED,
        json!({
            "request_id": request.id.to_string(),
            "module": request.module,
            "record_type": request.record_type,
            "record_id": request.record_id,
            "requester_id": request.requester_id,
            "definition_id": request.definition_id.to_string(),
        }),
    );
    if let Some(step) = steps.first() {
        notify_step(state, &request, step, "Approval requested");
    }
    Ok(request)
}

pub(crate) async fn list_requests(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let params = query.into_inner();
        let module_filter = params.filters.get("module").cloned();
        let record_type_filter = params.filters.get("record_type").cloned();
        let record_id_filter = params.filters.get("record_id").cloned();
        let requester_filter = params.filters.get("requester_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let definition_filter = params.filters.get("definition_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<ApprovalRequest> = state
            .requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| {
                if let Some(module) = &module_filter {
                    if request.module != *module {
                        return false;
                    }
                }
                if let Some(record_type) = &record_type_filter {
                    if request.record_type != *record_type {
                        return false;
                    }
                }
                if let Some(record_id) = &record_id_filter {
                    if request.record_id != *record_id {
                        return false;
                    }
                }
                if let Some(requester) = &requester_filter {
                    if request.requester_id != *requester {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if request.status != *status {
                        return false;
                    }
                }
                if let Some(definition_id) = &definition_filter {
                    if request.definition_id.to_string() != *definition_id {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if request.company_id.as_deref() != Some(company_id.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {} {}", request.summary, request.record_id, request.requester_id)
                        .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|request| request.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    body: web::Json<CreateRequestInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let input = body.into_inner();
        let module = require_text(&input.module, "module", MAX_MODULE_LEN)?;
        if !valid_record_key(&module, MAX_MODULE_LEN) {
            return Err(UnifiedError::bad_request(
                "module must be lowercase letters, digits, dots, underscores, or hyphens",
            ));
        }
        let record_type = require_text(&input.record_type, "record_type", MAX_RECORD_TYPE_LEN)?;
        if !valid_record_key(&record_type, MAX_RECORD_TYPE_LEN) {
            return Err(UnifiedError::bad_request(
                "record_type must be lowercase letters, digits, dots, underscores, or hyphens",
            ));
        }
        let record_id = require_text(&input.record_id, "record_id", MAX_RECORD_ID_LEN)?;
        let requester_id = match &input.requester_id {
            Some(raw) => require_text(raw, "requester_id", MAX_USER_ID_LEN)?,
            None => caller.actor.clone(),
        };
        let summary = require_text(&input.summary, "summary", MAX_SUMMARY_LEN)?;
        if let Some(amount) = input.amount_minor {
            if amount < 0 {
                return Err(UnifiedError::bad_request("amount_minor must not be negative"));
            }
        }
        let currency = optional_text(&input.currency, "currency", MAX_CURRENCY_LEN)?;
        let company_id = optional_text(&input.company_id, "company_id", MAX_COMPANY_ID_LEN)?;
        let definition = match &input.definition_id {
            Some(raw) => {
                let definition = load_definition(&state, raw)?;
                if definition.module != module || definition.record_type != record_type {
                    return Err(UnifiedError::bad_request(
                        "definition_id does not match the given module / record_type",
                    ));
                }
                if !definition.is_active {
                    return Err(UnifiedError::bad_request(
                        "The given approval definition is not active",
                    ));
                }
                definition
            }
            None => active_definition_for(&state, &module, &record_type).ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "No active approval definition exists for {module} / {record_type}"
                ))
            })?,
        };
        create_request_record(
            &state,
            &definition,
            module,
            record_type,
            record_id,
            requester_id,
            summary,
            input.amount_minor,
            currency,
            company_id,
        )
    })();
    match result {
        Ok(request) => created(request),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalRequestDetail> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let request = load_request(&state, &path.into_inner())?;
        Ok(request_detail(&state, &request))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn request_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let request = load_request(&state, &path.into_inner())?;
        let params = query.into_inner();
        let decisions = decisions_for_request(&state, request.id);
        let escalations = escalations_for_request(&state, request.id);
        Ok(json!({
            "request_id": request.id.to_string(),
            "decisions": paginate(decisions, &params).to_json(),
            "escalations": escalations,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Requests - decisions and lifecycle

/// Apply a terminal decision to a request: persist the new status,
/// stamp who decided, and answer the requesting module on the bus.
/// `approval.decided` is emitted exactly once per request.
pub(crate) fn finish_request(
    state: &ApprovalsState,
    request: &mut ApprovalRequest,
    status: &str,
    decision: &str,
    decided_by: &str,
) {
    let now = Utc::now();
    request.status = status.to_string();
    request.decided_at = Some(now);
    request.decided_by = Some(decided_by.to_string());
    request.updated_at = now;
    state.requests.insert(request.id, request.clone());
    state.emit(
        APPROVAL_DECIDED,
        json!({
            "request_id": request.id.to_string(),
            "decision": decision,
            "decided_by": decided_by,
            "module": request.module,
            "record_type": request.record_type,
            "record_id": request.record_id,
            "requester_id": request.requester_id,
        }),
    );
}

pub(crate) async fn decide_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecideInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalRequestDetail> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_DECIDE)?;
        let input = body.into_inner();
        if !DECISIONS.contains(&input.decision.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown decision: {} (expected one of {})",
                input.decision,
                DECISIONS.join(", ")
            )));
        }
        let comment = optional_text(&input.comment, "comment", MAX_COMMENT_LEN)?;
        if (input.decision == DECISION_REJECT || input.decision == DECISION_REQUEST_CHANGES)
            && comment.as_deref().unwrap_or("").is_empty()
        {
            return Err(UnifiedError::bad_request(
                "A comment is required when rejecting or requesting changes",
            ));
        }
        let settings = state.settings();
        if input.decision == DECISION_APPROVE
            && settings.require_comment_on_approve
            && comment.as_deref().unwrap_or("").is_empty()
        {
            return Err(UnifiedError::bad_request(
                "A comment is required when approving (see the module settings)",
            ));
        }
        let mut request = load_request(&state, &path.into_inner())?;
        if request.status != STATUS_IN_PROGRESS {
            return Err(UnifiedError::conflict(format!(
                "Only an in_progress request can be decided (current status: {})",
                request.status
            )));
        }
        if !settings.allow_self_approval && caller.actor == request.requester_id {
            return Err(UnifiedError::forbidden(
                "The requester cannot approve their own request",
            ));
        }
        let now = Utc::now();
        let Some(step) = current_step(&state, &request) else {
            return Err(UnifiedError::conflict(
                "The request has no active step to decide",
            ));
        };
        let slots = approvers_for_step(&state, request.id, request.current_step_index, &step);
        let (matched, unresolved) =
            matchable_slots(&state, &request, &slots, &caller.actor, &caller.role, now);
        let Some(found) = matched.first() else {
            if unresolved {
                return Err(UnifiedError::forbidden(
                    "The current step waits on the requester's manager, which could not be resolved; ask an administrator to reassign the step",
                ));
            }
            return Err(UnifiedError::forbidden(
                "The caller is not an approver of the current step",
            ));
        };
        // A slot that is already satisfied cannot be decided twice in
        // the same round: the decision would be meaningless.
        let slot_key = format!("{}:{}", found.slot.kind, found.slot.ref_value);
        let already_satisfied = satisfied_slots(&state, &request, &step).contains(&slot_key);
        if input.decision == DECISION_APPROVE && already_satisfied {
            return Err(UnifiedError::conflict(
                "This approver slot already approved the current step in this round",
            ));
        }
        let slot_ref = if found.slot.kind == APPROVER_KIND_MANAGER {
            resolve_manager(&state, &request.requester_id)
                .unwrap_or_else(|| found.slot.ref_value.clone())
        } else {
            found.slot.ref_value.clone()
        };
        let decision = ApprovalDecision {
            id: Uuid::new_v4(),
            request_id: request.id,
            step_index: request.current_step_index,
            round: request.round,
            approver_id: caller.actor.clone(),
            acting_for: found.acting_for.clone(),
            slot_kind: found.slot.kind.clone(),
            slot_ref,
            decision: input.decision.clone(),
            comment,
            created_at: now,
        };
        state.decisions.insert(decision.id, decision.clone());
        match input.decision.as_str() {
            DECISION_REJECT => {
                finish_request(
                    &state,
                    &mut request,
                    STATUS_REJECTED,
                    "rejected",
                    &caller.actor,
                );
            }
            DECISION_REQUEST_CHANGES => {
                finish_request(
                    &state,
                    &mut request,
                    STATUS_CHANGES_REQUESTED,
                    "changes_requested",
                    &caller.actor,
                );
            }
            _ => {
                if step_is_complete(&state, &request, &step) {
                    let steps = steps_for_definition(&state, request.definition_id);
                    if (request.current_step_index as usize) + 1 >= steps.len() {
                        finish_request(
                            &state,
                            &mut request,
                            STATUS_APPROVED,
                            "approved",
                            &caller.actor,
                        );
                    } else {
                        request.current_step_index += 1;
                        request.updated_at = now;
                        state.requests.insert(request.id, request.clone());
                        if let Some(next) = current_step(&state, &request) {
                            notify_step(&state, &request, &next, "Approval requested");
                        }
                    }
                } else {
                    request.updated_at = now;
                    state.requests.insert(request.id, request.clone());
                }
            }
        }
        let fresh = load_request(&state, &request.id.to_string())?;
        Ok(request_detail(&state, &fresh))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Reset a changes-requested / expired request for a fresh round:
/// back to `in_progress` at step 0, so every approver re-approves
/// the revised record instead of resuming mid-flow, with a fresh
/// deadline and no carried-over escalation or decision stamps.
fn reset_for_resubmission(
    request: &mut ApprovalRequest,
    definition: &ApprovalDefinition,
    settings: &ApprovalSettings,
    now: DateTime<Utc>,
) {
    request.status = STATUS_IN_PROGRESS.to_string();
    request.current_step_index = 0;
    request.round += 1;
    request.escalation_count = 0;
    request.last_escalated_at = None;
    request.expires_at = deadline_for(definition, settings, now);
    request.decided_at = None;
    request.decided_by = None;
    request.updated_at = now;
}

pub(crate) async fn resubmit_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalRequestDetail> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let mut request = load_request(&state, &path.into_inner())?;
        if !RESUBMITTABLE_STATUSES.contains(&request.status.as_str()) {
            return Err(UnifiedError::conflict(format!(
                "Only a changes_requested or expired request can be resubmitted (current status: {})",
                request.status
            )));
        }
        let definition = load_definition(&state, &request.definition_id.to_string())?;
        let settings = state.settings();
        let now = Utc::now();
        reset_for_resubmission(&mut request, &definition, &settings, now);
        state.requests.insert(request.id, request.clone());
        state.emit(
            APPROVAL_RESUBMITTED,
            json!({ "request_id": request.id.to_string(), "round": request.round }),
        );
        if let Some(step) = current_step(&state, &request) {
            notify_step(&state, &request, &step, "Approval resubmitted");
        }
        Ok(request_detail(&state, &request))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn cancel_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalRequestDetail> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let mut request = load_request(&state, &path.into_inner())?;
        if !OPEN_STATUSES.contains(&request.status.as_str()) {
            return Err(UnifiedError::conflict(format!(
                "Only an open request can be cancelled (current status: {})",
                request.status
            )));
        }
        request.status = STATUS_CANCELLED.to_string();
        request.decided_at = Some(Utc::now());
        request.decided_by = Some(caller.actor.clone());
        request.updated_at = Utc::now();
        state.requests.insert(request.id, request.clone());
        state.emit(
            APPROVAL_CANCELLED,
            json!({
                "request_id": request.id.to_string(),
                "cancelled_by": caller.actor,
            }),
        );
        Ok(request_detail(&state, &request))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn reassign_request_step(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReassignInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalRequestDetail> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings();
        validate_approvers(&input.approvers, settings.max_approvers_per_step)?;
        let request = load_request(&state, &path.into_inner())?;
        if request.status != STATUS_IN_PROGRESS {
            return Err(UnifiedError::conflict(format!(
                "Only an in_progress request can be reassigned (current status: {})",
                request.status
            )));
        }
        let steps = steps_for_definition(&state, request.definition_id);
        if input.step_index < 0 || (input.step_index as usize) >= steps.len() {
            return Err(UnifiedError::bad_request(format!(
                "step_index {} is out of range for this request",
                input.step_index
            )));
        }
        state
            .step_overrides
            .insert((request.id, input.step_index), input.approvers);
        let fresh = load_request(&state, &request.id.to_string())?;
        Ok(request_detail(&state, &fresh))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Expire overdue requests and escalate them. Expiry never silently
/// auto-approves: each overdue request is notified + escalated, and
/// only expires once its definition's rule gives up (action `none`,
/// or the escalation cap is reached).
///
/// Shared by the HTTP endpoint and the
/// `approvals.sweep_expiry_requested` hook the scheduler dispatches,
/// so the scheduled sweep runs the same code as the manual one.
/// The sweep is synchronous (no gateway calls), so it runs inline
/// in the hook.
pub(crate) fn sweep_expired_requests(state: &ApprovalsState) -> UnifiedResult<Value> {
    let settings = state.settings();
    let now = Utc::now();
    let mut overdue: Vec<ApprovalRequest> = state
        .requests
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|request| request.status == STATUS_IN_PROGRESS && request.expires_at < now)
        .collect();
    overdue.sort_by_key(|request| request.expires_at);
    let mut escalated: Vec<String> = Vec::new();
    let mut expired: Vec<String> = Vec::new();
    for mut request in overdue {
        let definition = match state
            .definitions
            .get(&request.definition_id)
            .map(|entry| entry.value().clone())
        {
            Some(definition) => definition,
            None => continue,
        };
        let give_up = definition.escalation_action == ESCALATION_NONE
            || request.escalation_count >= settings.max_escalations;
        if give_up {
            request.status = STATUS_EXPIRED.to_string();
            request.updated_at = now;
            state.requests.insert(request.id, request.clone());
            state.escalations.insert(
                Uuid::new_v4(),
                ApprovalEscalation {
                    id: Uuid::new_v4(),
                    request_id: request.id,
                    step_index: request.current_step_index,
                    reason: "deadline reached and the escalation rule gave up".to_string(),
                    action: ESCALATION_NONE.to_string(),
                    created_at: now,
                },
            );
            state.emit(
                APPROVAL_EXPIRED,
                json!({
                    "request_id": request.id.to_string(),
                    "step_index": request.current_step_index,
                }),
            );
            notify_user(
                state,
                &request.requester_id,
                "Approval request expired",
                &format!(
                    "Approval expired: {} (record {} {}).",
                    request.summary, request.record_type, request.record_id
                ),
            );
            expired.push(request.id.to_string());
            continue;
        }
        request.escalation_count += 1;
        request.last_escalated_at = Some(now);
        let extension = definition
            .escalation_extension_hours
            .unwrap_or(settings.default_escalation_extension_hours);
        request.expires_at = now + Duration::hours(extension);
        request.updated_at = now;
        state.requests.insert(request.id, request.clone());
        state.escalations.insert(
            Uuid::new_v4(),
            ApprovalEscalation {
                id: Uuid::new_v4(),
                request_id: request.id,
                step_index: request.current_step_index,
                reason: "deadline reached".to_string(),
                action: definition.escalation_action.clone(),
                created_at: now,
            },
        );
        state.emit(
            APPROVAL_ESCALATED,
            json!({
                "request_id": request.id.to_string(),
                "step_index": request.current_step_index,
                "escalation_count": request.escalation_count,
                "action": definition.escalation_action,
            }),
        );
        if let Some(target) = &definition.escalate_to {
            match target.kind.as_str() {
                APPROVER_KIND_USER => {
                    notify_user(
                        state,
                        &target.ref_value,
                        "Approval request escalated",
                        &format!(
                            "Escalated approval: {} (record {} {}), deadline extended by {}h.",
                            request.summary, request.record_type, request.record_id, extension
                        ),
                    );
                }
                APPROVER_KIND_MANAGER => {
                    if let Some(manager) = resolve_manager(state, &request.requester_id) {
                        notify_user(
                            state,
                            &manager,
                            "Approval request escalated",
                            &format!(
                                "Escalated approval: {} (record {} {}), deadline extended by {}h.",
                                request.summary, request.record_type, request.record_id, extension
                            ),
                        );
                    }
                }
                _ => {}
            }
        }
        if let Some(step) = current_step(state, &request) {
            notify_step(state, &request, &step, "Approval request escalated");
        }
        escalated.push(request.id.to_string());
    }
    Ok(json!({
        "swept_at": now.to_rfc3339(),
        "escalated": escalated,
        "expired": expired,
    }))
}

pub(crate) async fn sweep_expiry(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        sweep_expired_requests(&state)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Run the expiry sweep for `approvals.sweep_expiry_requested`,
/// the event the scheduler dispatches for the
/// `approvals.sweep_expiry` job kind.
pub(crate) fn on_sweep_expiry_requested(state: Arc<ApprovalsState>, ctx: &mut HookContext) {
    match sweep_expired_requests(&state) {
        Ok(summary) => ctx.add_result(json!({
            "approvals": "sweep_expiry",
            "summary": summary,
        })),
        Err(error) => ctx.add_result(json!({
            "approvals": "sweep_expiry",
            "error": error.message,
        })),
    }
}

pub(crate) async fn my_requests(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<ApprovalRequest> = state
            .requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| {
                if request.requester_id != caller.actor {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if request.status != *status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|request| request.created_at);
        items.reverse();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn overdue_requests(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let params = query.into_inner();
        let now = Utc::now();
        let mut items: Vec<ApprovalRequest> = state
            .requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| request.status == STATUS_IN_PROGRESS && request.expires_at < now)
            .collect();
        items.sort_by_key(|request| request.expires_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> ApprovalDefinition {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        ApprovalDefinition {
            id: Uuid::nil(),
            module: "invoices".to_string(),
            record_type: "invoice".to_string(),
            name: "Invoice approval".to_string(),
            description: None,
            is_active: true,
            timeout_hours: Some(48),
            escalation_action: "notify".to_string(),
            escalate_to: None,
            escalation_extension_hours: None,
            company_id: None,
            created_by: "admin".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    fn request_at_step(step_index: i32) -> ApprovalRequest {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        ApprovalRequest {
            id: Uuid::nil(),
            definition_id: Uuid::nil(),
            module: "invoices".to_string(),
            record_type: "invoice".to_string(),
            record_id: "rec-1".to_string(),
            requester_id: "user-42".to_string(),
            summary: "Invoice #1042".to_string(),
            amount_minor: None,
            currency: None,
            status: STATUS_CHANGES_REQUESTED.to_string(),
            current_step_index: step_index,
            round: 1,
            escalation_count: 2,
            last_escalated_at: Some(now),
            expires_at: now,
            decided_at: Some(now),
            decided_by: Some("approver-1".to_string()),
            company_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn active_step(definition_id: Uuid) -> ApprovalStep {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        ApprovalStep {
            id: Uuid::new_v4(),
            definition_id,
            position: 0,
            name: "Finance".to_string(),
            mode: "any".to_string(),
            approvers: vec![StepApprover {
                kind: APPROVER_KIND_USER.to_string(),
                ref_value: "approver-1".to_string(),
            }],
            is_active: true,
            created_at: now,
        }
    }

    #[test]
    fn resubmission_restarts_from_the_first_step() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut request = request_at_step(2);
        reset_for_resubmission(&mut request, &definition(), &ApprovalSettings::default(), now);
        assert_eq!(request.status, STATUS_IN_PROGRESS);
        assert_eq!(request.current_step_index, 0);
        assert_eq!(request.round, 2);
        assert_eq!(request.escalation_count, 0);
        assert!(request.last_escalated_at.is_none());
        assert!(request.decided_at.is_none());
        assert!(request.decided_by.is_none());
        assert_eq!(request.expires_at, now + Duration::hours(48));
    }

    #[test]
    fn create_request_record_rejects_a_definition_without_active_steps() {
        let state = ApprovalsState::default();
        let result = create_request_record(
            &state,
            &definition(),
            "invoices".to_string(),
            "invoice".to_string(),
            "rec-1".to_string(),
            "user-42".to_string(),
            "Invoice #1042".to_string(),
            None,
            None,
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no active steps"));
        assert!(state.requests.is_empty());
    }

    #[test]
    fn create_request_record_starts_in_progress_when_steps_exist() {
        let state = ApprovalsState::default();
        let definition = definition();
        let step = active_step(definition.id);
        state.steps.insert(step.id, step);
        let request = create_request_record(
            &state,
            &definition,
            "invoices".to_string(),
            "invoice".to_string(),
            "rec-1".to_string(),
            "user-42".to_string(),
            "Invoice #1042".to_string(),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(request.status, STATUS_IN_PROGRESS);
        assert_eq!(request.current_step_index, 0);
        assert_eq!(request.round, 1);
    }
}
