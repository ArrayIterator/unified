// Generic Approvals — Business logic shared by handlers and hooks.

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
use super::handlers_definitions::*;
use super::handlers_requests::*;
use super::handlers_dashboards::*;
use super::hooks::*;
use super::module::*;

/// Resolve the tenant context for a request. THE single
/// convergence point for tenant-scoped data access in this file
/// (see `unified_core::tenancy`): this function returns the tenancy
/// `TenantContext` (tenant id as Uuid + schema name derived only
/// by `tenancy::schema_name()`), and the Diesel persistence wiring
/// — when it lands — takes the Uuid from here into
/// `tenancy::with_tenant_connection`. Fail closed:
/// `from_http_request` answers 401 when the request did not
/// resolve to a tenant and 403 when the resolved tenant id is not
/// a valid UUID; no resolved tenant means the request is refused
/// here, never served from `public`.
///
/// OPEN (not closed by this guard): the in-memory DashMap stores
/// in this module carry no `tenant_id` on their records yet, so
/// there is no per-record tenant filtering to apply once the
/// context is resolved — that needs a store-shape change (tenant
/// id on every record + a migration column) and is integrator
/// work. What this guard does close is the context requirement
/// itself: every tenant-data endpoint resolves the tenant first
/// through `caller()`, and a request with no tenant context is
/// rejected before any business data is read.
pub(crate) fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<TenancyContext> {
    TenancyContext::from_http_request(req)
}

// ---------------------------------------------------------------------------
// Lookups
// ---------------------------------------------------------------------------

pub(crate) fn load_definition(state: &ApprovalsState, raw_id: &str) -> UnifiedResult<ApprovalDefinition> {
    let id = parse_uuid(raw_id, "definition")?;
    state
        .definitions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Approval definition not found: {id}")))
}

pub(crate) fn load_request(state: &ApprovalsState, raw_id: &str) -> UnifiedResult<ApprovalRequest> {
    let id = parse_uuid(raw_id, "request")?;
    state
        .requests
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Approval request not found: {id}")))
}

pub(crate) fn active_definition_for(
    state: &ApprovalsState,
    module: &str,
    record_type: &str,
) -> Option<ApprovalDefinition> {
    let mut matches: Vec<ApprovalDefinition> = state
        .definitions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|definition| {
            definition.is_active && definition.module == module && definition.record_type == record_type
        })
        .collect();
    // Newest wins when more than one is active (creation refuses a
    // second active definition, so this is only a safety net).
    matches.sort_by_key(|definition| definition.created_at);
    matches.pop()
}

pub(crate) fn steps_for_definition(state: &ApprovalsState, definition_id: Uuid) -> Vec<ApprovalStep> {
    let mut items: Vec<ApprovalStep> = state
        .steps
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|step| step.definition_id == definition_id && step.is_active)
        .collect();
    items.sort_by_key(|step| step.position);
    items
}

pub(crate) fn decisions_for_request(state: &ApprovalsState, request_id: Uuid) -> Vec<ApprovalDecision> {
    let mut items: Vec<ApprovalDecision> = state
        .decisions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|decision| decision.request_id == request_id)
        .collect();
    items.sort_by_key(|decision| decision.created_at);
    items
}

pub(crate) fn escalations_for_request(state: &ApprovalsState, request_id: Uuid) -> Vec<ApprovalEscalation> {
    let mut items: Vec<ApprovalEscalation> = state
        .escalations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|escalation| escalation.request_id == request_id)
        .collect();
    items.sort_by_key(|escalation| escalation.created_at);
    items
}

/// The approvers that apply to one step of one request: the
/// reassignment override wins over the definition step.
pub(crate) fn approvers_for_step(
    state: &ApprovalsState,
    request_id: Uuid,
    step_index: i32,
    step: &ApprovalStep,
) -> Vec<StepApprover> {
    state
        .step_overrides
        .get(&(request_id, step_index))
        .map(|entry| entry.value().clone())
        .unwrap_or_else(|| step.approvers.clone())
}

// ---------------------------------------------------------------------------
// Approver resolution
// ---------------------------------------------------------------------------

/// Ask the HR / users module for the requester's manager. Returns
/// `None` when nobody answers or the answer carries no manager id —
/// the slot stays unresolved instead of failing the whole request.
pub(crate) fn resolve_manager(state: &ApprovalsState, requester_id: &str) -> Option<String> {
    let bus = state.bus()?;
    let context = bus.emit(
        USER_MANAGER_QUERY,
        json!({ "user_id": requester_id }),
    );
    context
        .get("manager_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// An active delegation that lets `delegate_id` act for
/// `delegator_id` right now, if any.
pub(crate) fn active_delegation_for(
    state: &ApprovalsState,
    delegator_id: &str,
    delegate_id: &str,
    now: DateTime<Utc>,
) -> Option<ApprovalDelegation> {
    state
        .delegations
        .iter()
        .map(|entry| entry.value().clone())
        .find(|delegation| {
            delegation.is_active
                && delegation.delegator_id == delegator_id
                && delegation.delegate_id == delegate_id
                && delegation.starts_at <= now
                && now <= delegation.ends_at
        })
}

/// One slot of the current step, resolved against a caller: does the
/// caller satisfy this slot, directly or as a delegate? `acting_for`
/// is `Some(original approver)` when the caller decides as a
/// delegate, `None` when they decide as themselves.
pub(crate) struct SlotMatch {
    pub(crate) slot: StepApprover,
    pub(crate) acting_for: Option<String>,
}

pub(crate) fn match_slot(
    state: &ApprovalsState,
    request: &ApprovalRequest,
    slot: &StepApprover,
    actor: &str,
    role: &str,
    now: DateTime<Utc>,
) -> Option<SlotMatch> {
    match slot.kind.as_str() {
        APPROVER_KIND_USER => {
            if actor == slot.ref_value {
                Some(SlotMatch { slot: slot.clone(), acting_for: None })
            } else if active_delegation_for(state, &slot.ref_value, actor, now).is_some() {
                Some(SlotMatch {
                    slot: slot.clone(),
                    acting_for: Some(slot.ref_value.clone()),
                })
            } else {
                None
            }
        }
        APPROVER_KIND_ROLE => {
            if role == slot.ref_value {
                Some(SlotMatch { slot: slot.clone(), acting_for: None })
            } else {
                None
            }
        }
        APPROVER_KIND_MANAGER => {
            let manager = resolve_manager(state, &request.requester_id)?;
            if actor == manager {
                Some(SlotMatch { slot: slot.clone(), acting_for: None })
            } else if active_delegation_for(state, &manager, actor, now).is_some() {
                Some(SlotMatch {
                    slot: slot.clone(),
                    acting_for: Some(manager),
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Every slot of the current step the caller may decide for, plus
/// whether the step has any slot that cannot be resolved right now
/// (an unanswered manager query).
pub(crate) fn matchable_slots(
    state: &ApprovalsState,
    request: &ApprovalRequest,
    slots: &[StepApprover],
    actor: &str,
    role: &str,
    now: DateTime<Utc>,
) -> (Vec<SlotMatch>, bool) {
    let mut matched = Vec::new();
    let mut unresolved = false;
    for slot in slots {
        if slot.kind == APPROVER_KIND_MANAGER && resolve_manager(state, &request.requester_id).is_none() {
            unresolved = true;
        }
        if let Some(found) = match_slot(state, request, slot, actor, role, now) {
            matched.push(found);
        }
    }
    (matched, unresolved)
}

/// Is the caller waiting on this request — an approver of its current
/// step, or a delegate of one? Used by the `my-pending` dashboard.
pub(crate) fn caller_is_pending_approver(
    state: &ApprovalsState,
    request: &ApprovalRequest,
    actor: &str,
    role: &str,
    now: DateTime<Utc>,
) -> bool {
    if request.status != STATUS_IN_PROGRESS {
        return false;
    }
    let Some(step) = current_step(state, request) else {
        return false;
    };
    let slots = approvers_for_step(state, request.id, request.current_step_index, &step);
    let (matched, _) = matchable_slots(state, request, &slots, actor, role, now);
    !matched.is_empty()
}

pub(crate) fn current_step(state: &ApprovalsState, request: &ApprovalRequest) -> Option<ApprovalStep> {
    let steps = steps_for_definition(state, request.definition_id);
    steps
        .into_iter()
        .nth(request.current_step_index as usize)
}

/// Step completion for the current round: which slots of the step
/// already hold an `approve` decision, and whether the step is done.
/// A slot is satisfied by a decision whose `slot_kind` / `slot_ref`
/// match it; a manager slot is satisfied by a decision recorded
/// against the resolved manager id.
pub(crate) fn satisfied_slots(
    state: &ApprovalsState,
    request: &ApprovalRequest,
    step: &ApprovalStep,
) -> Vec<String> {
    let slots = approvers_for_step(state, request.id, request.current_step_index, step);
    let decisions = decisions_for_request(state, request.id);
    let mut satisfied: Vec<String> = Vec::new();
    for slot in &slots {
        let key = format!("{}:{}", slot.kind, slot.ref_value);
        let done = decisions.iter().any(|decision| {
            decision.step_index == request.current_step_index
                && decision.round == request.round
                && decision.decision == DECISION_APPROVE
                && slot_matches_decision(state, request, slot, decision)
        });
        if done {
            satisfied.push(key);
        }
    }
    satisfied
}

pub(crate) fn slot_matches_decision(
    state: &ApprovalsState,
    request: &ApprovalRequest,
    slot: &StepApprover,
    decision: &ApprovalDecision,
) -> bool {
    if decision.slot_kind != slot.kind {
        return false;
    }
    match slot.kind.as_str() {
        APPROVER_KIND_USER => {
            decision.slot_ref == slot.ref_value
                || decision
                    .acting_for
                    .as_deref()
                    .map(|acting| acting == slot.ref_value)
                    .unwrap_or(false)
        }
        APPROVER_KIND_ROLE => decision.slot_ref == slot.ref_value,
        APPROVER_KIND_MANAGER => {
            // The decision records the resolved manager id in
            // `slot_ref`; it matches when it is the requester's
            // current manager. When the manager cannot be
            // resolved, only an exact match against the slot's
            // stored value counts: any non-empty `slot_ref` would
            // let an unrelated decision satisfy the manager slot.
            match resolve_manager(state, &request.requester_id) {
                Some(manager) => decision.slot_ref == manager,
                None => decision.slot_ref == slot.ref_value,
            }
        }
        _ => false,
    }
}

pub(crate) fn step_is_complete(
    state: &ApprovalsState,
    request: &ApprovalRequest,
    step: &ApprovalStep,
) -> bool {
    let slots = approvers_for_step(state, request.id, request.current_step_index, step);
    let satisfied = satisfied_slots(state, request, step);
    match step.mode.as_str() {
        STEP_MODE_ALL => satisfied.len() == slots.len() && !slots.is_empty(),
        _ => !satisfied.is_empty(),
    }
}

/// Build the detail view of a request: steps with live status plus
/// the full decision history.
pub(crate) fn request_detail(state: &ApprovalsState, request: &ApprovalRequest) -> ApprovalRequestDetail {
    let steps = steps_for_definition(state, request.definition_id);
    let decisions = decisions_for_request(state, request.id);
    let mut views: Vec<ApprovalStepView> = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        let index = index as i32;
        let slots = approvers_for_step(state, request.id, index, step);
        let satisfied = satisfied_slots(state, request, step);
        let (_, unresolved) = {
            // Resolution is only meaningful for the current step;
            // earlier / later steps report their stored shape.
            let mut unres = false;
            if index == request.current_step_index {
                for slot in &slots {
                    if slot.kind == APPROVER_KIND_MANAGER
                        && resolve_manager(state, &request.requester_id).is_none()
                    {
                        unres = true;
                    }
                }
            }
            (Vec::<SlotMatch>::new(), unres)
        };
        let status = if index < request.current_step_index {
            "approved".to_string()
        } else if index > request.current_step_index {
            "pending".to_string()
        } else if request.status == STATUS_APPROVED
            || request.status == STATUS_REJECTED
            || request.status == STATUS_CHANGES_REQUESTED
            || request.status == STATUS_EXPIRED
            || request.status == STATUS_CANCELLED
        {
            request.status.clone()
        } else if decisions.iter().any(|decision| {
            decision.step_index == index
                && decision.round == request.round
                && decision.decision == DECISION_REJECT
        }) {
            STATUS_REJECTED.to_string()
        } else if decisions.iter().any(|decision| {
            decision.step_index == index
                && decision.round == request.round
                && decision.decision == DECISION_REQUEST_CHANGES
        }) {
            STATUS_CHANGES_REQUESTED.to_string()
        } else {
            STATUS_IN_PROGRESS.to_string()
        };
        views.push(ApprovalStepView {
            step: step.clone(),
            status,
            satisfied_slots: satisfied,
            resolution: if unresolved {
                "unresolved".to_string()
            } else {
                "resolved".to_string()
            },
        });
    }
    ApprovalRequestDetail {
        request: request.clone(),
        steps: views,
        decisions,
    }
}

/// Notify one user through the bus (`notification.requested`).
/// Delivery belongs to the notifications module; a missing consumer
/// only means nobody delivers, never an error here.
pub(crate) fn notify_user(state: &ApprovalsState, user_id: &str, title: &str, body: &str) {
    state.emit(
        NOTIFICATION_REQUESTED,
        json!({ "to_user_id": user_id, "title": title, "body": body }),
    );
}

/// Notify every approver slot of a step that can be addressed
/// directly (user slots by id; role and manager slots are named in
/// the body because they have no single address).
pub(crate) fn notify_step(state: &ApprovalsState, request: &ApprovalRequest, step: &ApprovalStep, title: &str) {
    let slots = approvers_for_step(state, request.id, request.current_step_index, step);
    let mut named: Vec<String> = Vec::new();
    for slot in &slots {
        match slot.kind.as_str() {
            APPROVER_KIND_USER => {
                notify_user(
                    state,
                    &slot.ref_value,
                    title,
                    &format!(
                        "Approval requested: {} (record {} {}).",
                        request.summary, request.record_type, request.record_id
                    ),
                );
            }
            APPROVER_KIND_ROLE => named.push(format!("role {}", slot.ref_value)),
            APPROVER_KIND_MANAGER => named.push("the requester's manager".to_string()),
            _ => {}
        }
    }
    if !named.is_empty() {
        notify_user(
            state,
            &request.requester_id,
            title,
            &format!(
                "Approval requested: {} — waiting on {}.",
                request.summary,
                named.join(", ")
            ),
        );
    }
}

/// The deadline for a new request under a definition.
pub(crate) fn deadline_for(definition: &ApprovalDefinition, settings: &ApprovalSettings, now: DateTime<Utc>) -> DateTime<Utc> {
    let hours = definition
        .timeout_hours
        .unwrap_or(settings.default_timeout_hours);
    now + Duration::hours(hours)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn definition(timeout_hours: Option<i64>) -> ApprovalDefinition {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        ApprovalDefinition {
            id: Uuid::nil(),
            module: "invoices".to_string(),
            record_type: "invoice".to_string(),
            name: "Invoice approval".to_string(),
            description: None,
            is_active: true,
            timeout_hours,
            escalation_action: "notify".to_string(),
            escalate_to: None,
            escalation_extension_hours: None,
            company_id: None,
            created_by: "admin".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn deadline_for_prefers_the_definition_timeout() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let settings = ApprovalSettings::default();
        assert_eq!(
            deadline_for(&definition(Some(48)), &settings, now),
            now + Duration::hours(48)
        );
    }

    #[test]
    fn deadline_for_falls_back_to_the_settings_default() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let settings = ApprovalSettings::default();
        assert_eq!(settings.default_timeout_hours, 72);
        assert_eq!(
            deadline_for(&definition(None), &settings, now),
            now + Duration::hours(72)
        );
    }

    fn manager_request() -> ApprovalRequest {
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
            status: STATUS_IN_PROGRESS.to_string(),
            current_step_index: 0,
            round: 1,
            escalation_count: 0,
            last_escalated_at: None,
            expires_at: now,
            decided_at: None,
            decided_by: None,
            company_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn manager_decision(slot_ref: &str) -> ApprovalDecision {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        ApprovalDecision {
            id: Uuid::nil(),
            request_id: Uuid::nil(),
            step_index: 0,
            round: 1,
            approver_id: slot_ref.to_string(),
            acting_for: None,
            slot_kind: APPROVER_KIND_MANAGER.to_string(),
            slot_ref: slot_ref.to_string(),
            decision: DECISION_APPROVE.to_string(),
            comment: None,
            created_at: now,
        }
    }

    #[test]
    fn manager_slot_needs_an_exact_match_when_the_manager_is_unresolvable() {
        // No hook bus attached, so resolve_manager returns None.
        let state = ApprovalsState::default();
        let request = manager_request();
        let slot = StepApprover {
            kind: APPROVER_KIND_MANAGER.to_string(),
            ref_value: "manager-1".to_string(),
        };
        assert!(!slot_matches_decision(
            &state,
            &request,
            &slot,
            &manager_decision("someone-else")
        ));
        assert!(slot_matches_decision(
            &state,
            &request,
            &slot,
            &manager_decision("manager-1")
        ));
    }
}
