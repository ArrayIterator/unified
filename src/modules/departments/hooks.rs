//! Hook handlers for ticket routing, SLAs and membership.

use unified_core::hooks::{HookContext};
use unified_core::json;
use serde_json::{json};
use std::sync::{Arc};
use super::state::DepartmentsState;
use super::validation::{now, within_working_hours};

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// Answer `ticket.route_requested` with the routed department, when
/// one matches. No result means "no route" and the ticket stays
/// unassigned to a department.
pub(crate) fn on_ticket_route_requested(state: Arc<DepartmentsState>, ctx: &mut HookContext) {
    let title = ctx
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let body = ctx
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let category = ctx
        .get("category")
        .and_then(|v| v.as_str())
        .unwrap_or("general")
        .to_string();
    let product = ctx
        .get("product")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let (department_id, _rule_id) = state.route_for(&title, &body, &category, product.as_deref());
    if let Some(department_id) = department_id {
        ctx.add_result(json!({ "department_id": department_id }));
    }
}

/// Answer `department.sla_requested` with the department's SLA
/// targets in minutes.
pub(crate) fn on_department_sla_requested(state: Arc<DepartmentsState>, ctx: &mut HookContext) {
    let Some(department_id) = ctx.get("department_id").and_then(|v| v.as_str()) else {
        return;
    };
    if let Some(department) = state.departments.get(department_id) {
        ctx.add_result(json!({
            "department_id": &department.id,
            "first_response_minutes": department.sla_first_response_minutes,
            "resolution_minutes": department.sla_resolution_minutes,
        }));
    }
}

/// Answer `department.membership_check` with the departments a user
/// agents for. Only active departments count.
pub(crate) fn on_department_membership_check(state: Arc<DepartmentsState>, ctx: &mut HookContext) {
    let Some(user_id) = ctx.get("user_id").and_then(|v| v.as_str()) else {
        return;
    };
    let department_ids: Vec<String> = state
        .departments
        .iter()
        .filter(|entry| {
            entry.value().status == "active"
                && entry.value().agents.iter().any(|agent| agent == user_id)
        })
        .map(|entry| entry.key().clone())
        .collect();
    ctx.add_result(json!({
        "user_id": user_id,
        "department_ids": department_ids,
    }));
}

/// Answer `department.agents_requested` with a department's agent
/// user ids.
pub(crate) fn on_department_agents_requested(state: Arc<DepartmentsState>, ctx: &mut HookContext) {
    let Some(department_id) = ctx.get("department_id").and_then(|v| v.as_str()) else {
        return;
    };
    if let Some(department) = state.departments.get(department_id) {
        let agent_ids: Vec<String> = department.agents.clone();
        ctx.add_result(json!({
            "department_id": &department.id,
            "agent_ids": agent_ids,
        }));
    }
}

/// On `ticket.created`, send the department auto-reply to the ticket
/// owner when the ticket arrived outside the department's working
/// hours. The reply goes through `notification.requested`, the event
/// the Notifications module already listens to.
pub(crate) fn on_ticket_created(state: Arc<DepartmentsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(department_id) = data.get("department_id").and_then(|v| v.as_str()) else {
        return;
    };
    let Some(user_id) = data.get("user_id").and_then(|v| v.as_str()) else {
        return;
    };
    let Some(department) = state.departments.get(department_id) else {
        return;
    };
    if !department.auto_reply.enabled {
        return;
    }
    if within_working_hours(&department.working_hours, now()) {
        return;
    }
    let subject = if department.auto_reply.subject.trim().is_empty() {
        "We received your ticket".to_string()
    } else {
        department.auto_reply.subject.clone()
    };
    let body = department.auto_reply.body.clone();
    let user_id = user_id.to_string();
    drop(department);
    state.emit(
        "notification.requested",
        json!({
            "user_id": user_id,
            "title": subject,
            "body": body,
            "kind": "ticket",
        }),
    );
}
