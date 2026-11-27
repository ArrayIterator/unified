// field_service module - Dispatch handlers.
//
// Split from the former single-file module `src/modules/field_service.rs`.
// The module root (`field_service.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{PERM_FIELD_SERVICE_MANAGE, PERM_FIELD_SERVICE_VIEW};
use super::helpers::{
    caller, caller_company, can_act_on_order, err, load_dispatch_assignment, load_work_order, ok,
    paginate, priority_rank, require_permission, require_tenant,
};
use super::state::FieldServiceState;
use super::types::{DispatchAssignment, WorkOrder, WorkOrderStatus};

// Dispatch handlers

/// One technician's board entry: their active jobs in dispatch
/// order, their availability, and the ordering rule. The order is
/// a heuristic - priority first, then due date, then creation
/// time - and is documented as such in the response: it is not a
/// route optimiser and carries no travel times or distances.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct TechnicianBoard {
    pub(crate) technician_id: String,
    pub(crate) active_jobs: usize,
    pub(crate) max_concurrent_jobs: i64,
    pub(crate) available: bool,
    pub(crate) jobs: Vec<Value>,
}

pub(crate) async fn dispatch_board(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        let params = query.into_inner();
        let technician_filter = params.filters.get("technician_id").cloned();
        let max_jobs = state.settings().max_concurrent_jobs_per_technician;
        // Technicians are the users holding active dispatch
        // assignments (plus an explicitly requested technician,
        // who may currently hold no jobs).
        let mut technician_ids: Vec<String> = state
            .dispatch_assignments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.status == "active")
            .map(|row| row.technician_id)
            .collect();
        technician_ids.sort();
        technician_ids.dedup();
        if let Some(requested) = &technician_filter {
            if !technician_ids.contains(requested) {
                technician_ids.push(requested.clone());
            }
        }
        let mut board: Vec<TechnicianBoard> = Vec::new();
        for technician_id in technician_ids {
            if let Some(filter) = &technician_filter {
                if &technician_id != filter {
                    continue;
                }
            }
            let assignments = state.active_assignments_of(&technician_id);
            let mut jobs: Vec<(i64, WorkOrder)> = Vec::new();
            for assignment in &assignments {
                if let Some(order) = state
                    .work_orders
                    .get(&assignment.work_order_id)
                    .map(|entry| entry.value().clone())
                {
                    jobs.push((assignment.sequence, order));
                }
            }
            // Dispatch order heuristic: assignment sequence first
            // (the dispatcher sets it), then priority, then due
            // date, then creation time. This is an ordering hint,
            // not a route optimiser.
            jobs.sort_by(|a, b| {
                a.0.cmp(&b.0)
                    .then(priority_rank(&a.1.priority).cmp(&priority_rank(&b.1.priority)))
                    .then(a.1.due_at.cmp(&b.1.due_at))
                    .then(a.1.created_at.cmp(&b.1.created_at))
            });
            let job_values: Vec<Value> = jobs
                .into_iter()
                .enumerate()
                .map(|(index, (sequence, order))| {
                    json!({
                        "sequence": sequence,
                        "board_position": index + 1,
                        "work_order": order,
                    })
                })
                .collect();
            let active_jobs = assignments.len();
            board.push(TechnicianBoard {
                technician_id,
                active_jobs,
                max_concurrent_jobs: max_jobs,
                available: (active_jobs as i64) < max_jobs,
                jobs: job_values,
            });
        }
        Ok(json!({
            "technicians": board,
            "max_concurrent_jobs_per_technician": max_jobs,
            "ordering": "assignment sequence, then priority (urgent > high > normal > low), then due date, then creation time. This is a dispatch ordering heuristic, not a route optimiser: no travel times or distances are computed.",
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_dispatch_assignments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        let params = query.into_inner();
        let technician_filter = params.filters.get("technician_id").cloned();
        let order_filter = params.filters.get("work_order_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<DispatchAssignment> = state
            .dispatch_assignments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if let Some(technician) = &technician_filter {
                    if &row.technician_id != technician {
                        return false;
                    }
                }
                if let Some(order_id) = &order_filter {
                    if row.work_order_id.to_string() != *order_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &row.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| {
            a.sequence
                .cmp(&b.sequence)
                .then(a.assigned_at.cmp(&b.assigned_at))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Detach the order from dispatch once its last active assignment
/// ends (completed or released). Semantics: an order that still
/// claims a technician but has no active assignment is
/// inconsistent, so `Assigned` falls back to `Open` and
/// `InProgress` is parked on `OnHold` (work started, but no
/// technician is currently dispatched), and `assignee_id` is
/// cleared in both cases. Orders in any other state, or orders
/// that still have another active assignment, are left alone.
pub(crate) fn detach_order_assignment(state: &FieldServiceState, order: &mut WorkOrder) {
    if !matches!(
        order.status,
        WorkOrderStatus::Assigned | WorkOrderStatus::InProgress
    ) {
        return;
    }
    if state.active_assignment_of_order(order.id).is_some() {
        return;
    }
    order.assignee_id = None;
    order.status = if order.status == WorkOrderStatus::Assigned {
        WorkOrderStatus::Open
    } else {
        WorkOrderStatus::OnHold
    };
    order.updated_at = Utc::now();
    state.work_orders.insert(order.id, order.clone());
}

pub(crate) async fn release_dispatch_assignment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DispatchAssignment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let mut assignment = load_dispatch_assignment(&state, &path.into_inner())?;
        if assignment.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Only an active dispatch assignment can be released (current status: \"{}\")",
                assignment.status
            )));
        }
        assignment.status = "released".to_string();
        assignment.released_at = Some(Utc::now());
        assignment.updated_at = Utc::now();
        state.dispatch_assignments.insert(assignment.id, assignment.clone());
        if let Some(mut order) = state
            .work_orders
            .get(&assignment.work_order_id)
            .map(|entry| entry.value().clone())
        {
            detach_order_assignment(&state, &mut order);
        }
        state.emit(
            "field_service.dispatch.released",
            json!({
                "dispatch_assignment_id": assignment.id.to_string(),
                "work_order_id": assignment.work_order_id.to_string(),
                "technician_id": assignment.technician_id,
                "released_by": caller.actor,
            }),
        );
        Ok(assignment)
    })();
    match result {
        Ok(assignment) => ok(assignment),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn complete_dispatch_assignment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DispatchAssignment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let mut assignment = load_dispatch_assignment(&state, &path.into_inner())?;
        let order = load_work_order(&state, &assignment.work_order_id.to_string())?;
        can_act_on_order(&ctx, &caller, &order)?;
        if assignment.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Only an active dispatch assignment can be completed (current status: \"{}\")",
                assignment.status
            )));
        }
        assignment.status = "completed".to_string();
        assignment.completed_at = Some(Utc::now());
        assignment.updated_at = Utc::now();
        state.dispatch_assignments.insert(assignment.id, assignment.clone());
        // Completing the last active assignment detaches the
        // technician from the order (see `detach_order_assignment`):
        // the order must not stay Assigned/InProgress with an
        // assignee but no active assignment.
        if let Some(mut order) = state
            .work_orders
            .get(&assignment.work_order_id)
            .map(|entry| entry.value().clone())
        {
            detach_order_assignment(&state, &mut order);
        }
        state.emit(
            "field_service.dispatch.completed",
            json!({
                "dispatch_assignment_id": assignment.id.to_string(),
                "work_order_id": assignment.work_order_id.to_string(),
                "technician_id": assignment.technician_id,
                "completed_by": caller.actor,
            }),
        );
        Ok(assignment)
    })();
    match result {
        Ok(assignment) => ok(assignment),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work_order(status: WorkOrderStatus) -> WorkOrder {
        let now = Utc::now();
        WorkOrder {
            id: Uuid::new_v4(),
            equipment_id: Uuid::new_v4(),
            plan_id: None,
            project_id: None,
            title: "Fix pump".to_string(),
            description: None,
            order_type: "corrective".to_string(),
            priority: "normal".to_string(),
            status,
            assignee_id: Some("tech-1".to_string()),
            scheduled_at: None,
            due_at: None,
            completed_at: None,
            verified_at: None,
            closed_at: None,
            cancelled_at: None,
            created_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    fn assignment(work_order_id: Uuid, technician_id: &str, status: &str) -> DispatchAssignment {
        let now = Utc::now();
        DispatchAssignment {
            id: Uuid::new_v4(),
            work_order_id,
            technician_id: technician_id.to_string(),
            sequence: 0,
            status: status.to_string(),
            assigned_by: "test".to_string(),
            assigned_at: now,
            started_at: None,
            completed_at: None,
            released_at: None,
            notes: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn detach_order_assignment_returns_assigned_order_to_open_when_last_assignment_ends() {
        let state = FieldServiceState::seeded();
        let mut order = work_order(WorkOrderStatus::Assigned);
        state.work_orders.insert(order.id, order.clone());
        let mut ended = assignment(order.id, "tech-1", "completed");
        state.dispatch_assignments.insert(ended.id, ended.clone());
        detach_order_assignment(&state, &mut order);
        let stored = state.work_orders.get(&order.id).expect("order still stored");
        assert_eq!(stored.value().status, WorkOrderStatus::Open);
        assert_eq!(stored.value().assignee_id, None);

        // With another active assignment remaining, the order keeps
        // its technician and state.
        let mut order = work_order(WorkOrderStatus::Assigned);
        state.work_orders.insert(order.id, order.clone());
        ended = assignment(order.id, "tech-1", "completed");
        state.dispatch_assignments.insert(ended.id, ended.clone());
        let active = assignment(order.id, "tech-2", "active");
        state.dispatch_assignments.insert(active.id, active.clone());
        detach_order_assignment(&state, &mut order);
        let stored = state.work_orders.get(&order.id).expect("order still stored");
        assert_eq!(stored.value().status, WorkOrderStatus::Assigned);
        assert_eq!(stored.value().assignee_id.as_deref(), Some("tech-1"));
    }

    #[test]
    fn detach_order_assignment_parks_in_progress_order_on_hold_when_released() {
        let state = FieldServiceState::seeded();
        let mut order = work_order(WorkOrderStatus::InProgress);
        state.work_orders.insert(order.id, order.clone());
        let released = assignment(order.id, "tech-1", "released");
        state.dispatch_assignments.insert(released.id, released.clone());
        detach_order_assignment(&state, &mut order);
        let stored = state.work_orders.get(&order.id).expect("order still stored");
        assert_eq!(stored.value().status, WorkOrderStatus::OnHold);
        assert_eq!(stored.value().assignee_id, None);
    }
}
