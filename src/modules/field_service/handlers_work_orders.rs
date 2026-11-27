// field_service module - Work order handlers.
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

use super::constants::{MAX_CHECKLIST_ITEMS, MAX_DESCRIPTION_LEN, MAX_NAME_LEN, MAX_NOTES_LEN, MAX_PARTS_PER_ORDER, MAX_TITLE_LEN, MAX_UNIT_LEN, PERM_FIELD_SERVICE_MANAGE, PERM_FIELD_SERVICE_VIEW};
use super::types::{DispatchAssignment, WorkOrder, WorkOrderItem, WorkOrderPart, WorkOrderStatus};
use super::requests::{AssignRequest, CancelRequest, ChangeStatusRequest, CreateWorkOrderRequest, UpdateWorkOrderRequest};
use super::state::FieldServiceState;
use super::helpers::{
    Caller, caller, caller_company, can_act_on_order, compute_next_due, created, err, items_of,
    load_equipment, load_plan, load_work_order, ok, optional_text, paginate, parts_of,
    priority_rank, require_permission, require_tenant, require_text,
};

// Work order handlers

/// Shared work-order creation used by the create endpoint and by
/// failure reports: validates references, seeds checklist items
/// from the plan template when present, and stores required parts.
pub(crate) fn insert_work_order(
    state: &FieldServiceState,
    caller: &Caller,
    input: CreateWorkOrderRequest,
) -> UnifiedResult<WorkOrder> {
    let equipment = load_equipment(state, &input.equipment_id.to_string())?;
    let plan = match input.plan_id {
        Some(id) => Some(load_plan(state, &id.to_string())?),
        None => None,
    };
    if let Some(plan) = &plan {
        if plan.equipment_id != equipment.id {
            return Err(UnifiedError::bad_request(
                "Maintenance plan does not belong to this equipment",
            ));
        }
    }
    let order_type = match &input.order_type {
        Some(raw) => {
            let order_type = raw.trim().to_string();
            if !state.definition_exists("work_order_type", &order_type) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown work order type: {order_type}"
                )));
            }
            order_type
        }
        None => "preventive".to_string(),
    };
    let priority = match &input.priority {
        Some(raw) => {
            let priority = raw.trim().to_string();
            if !state.definition_exists("priority", &priority) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown priority: {priority}"
                )));
            }
            priority
        }
        None => state.settings().default_priority,
    };
    let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
    let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
    let project_id = match &input.project_id {
        Some(raw) => Some(require_text(raw, "project_id", MAX_NAME_LEN)?),
        None => None,
    };
    let assignee_id = match &input.assignee_id {
        Some(raw) => {
            let assignee = raw.trim().to_string();
            if assignee.is_empty() {
                return Err(UnifiedError::bad_request("assignee_id must not be empty"));
            }
            Some(assignee)
        }
        None => None,
    };
    if let (Some(scheduled), Some(due)) = (input.scheduled_at, input.due_at) {
        if due < scheduled {
            return Err(UnifiedError::bad_request(
                "due_at must not be earlier than scheduled_at",
            ));
        }
    }
    // Validate the seed data before inserting anything, so a bad
    // checklist title or part line cannot leave an orphan order.
    let mut checklist: Vec<String> = Vec::new();
    if let Some(items) = &input.checklist_items {
        if items.len() > MAX_CHECKLIST_ITEMS {
            return Err(UnifiedError::bad_request(format!(
                "checklist_items must contain at most {MAX_CHECKLIST_ITEMS} items"
            )));
        }
        for item in items {
            checklist.push(require_text(item, "checklist item", MAX_TITLE_LEN)?);
        }
    } else if let Some(plan) = &plan {
        if let Some(template_id) = plan.template_id {
            if let Some(template) = state
                .templates
                .get(&template_id)
                .map(|entry| entry.value().clone())
            {
                checklist = template.checklist_template.clone();
            }
        }
    }
    let mut part_lines: Vec<(Option<Uuid>, String, i64, String)> = Vec::new();
    if let Some(parts) = &input.required_parts {
        if parts.len() > MAX_PARTS_PER_ORDER {
            return Err(UnifiedError::bad_request(format!(
                "required_parts must contain at most {MAX_PARTS_PER_ORDER} entries"
            )));
        }
        for part in parts {
            let part_name = require_text(&part.part_name, "part_name", MAX_NAME_LEN)?;
            let quantity = part.quantity.unwrap_or(1);
            if quantity < 1 || quantity > 100_000 {
                return Err(UnifiedError::bad_request(
                    "quantity must be between 1 and 100000",
                ));
            }
            let unit = match &part.unit {
                Some(raw) => require_text(raw, "unit", MAX_UNIT_LEN)?,
                None => "unit".to_string(),
            };
            part_lines.push((part.stock_id, part_name, quantity, unit));
        }
    }
    let now = Utc::now();
    let work_order = WorkOrder {
        id: Uuid::now_v7(),
        equipment_id: equipment.id,
        plan_id: input.plan_id,
        project_id,
        title,
        description,
        order_type,
        priority,
        status: if assignee_id.is_some() {
            WorkOrderStatus::Assigned
        } else {
            WorkOrderStatus::Open
        },
        assignee_id: assignee_id.clone(),
        scheduled_at: input.scheduled_at,
        due_at: input.due_at,
        completed_at: None,
        verified_at: None,
        closed_at: None,
        cancelled_at: None,
        created_by: caller.actor.clone(),
        created_at: now,
        updated_at: now,
    };
    state.work_orders.insert(work_order.id, work_order.clone());
    // Seed the checklist: explicit items win; otherwise the plan
    // template's checklist seeds the order.
    for (index, title) in checklist.into_iter().enumerate() {
        let item = WorkOrderItem {
            id: Uuid::now_v7(),
            work_order_id: work_order.id,
            title,
            position: index as i64,
            is_done: false,
            done_by: None,
            done_at: None,
            notes: None,
            created_at: now,
            updated_at: now,
        };
        state.items.insert(item.id, item);
    }
    for (stock_id, part_name, quantity, unit) in part_lines {
        let record = WorkOrderPart {
            id: Uuid::now_v7(),
            work_order_id: work_order.id,
            stock_id,
            part_name,
            quantity,
            unit,
            status: "required".to_string(),
            consumed_at: None,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.parts.insert(record.id, record);
    }
    state.emit(
        "field_service.work_order.created",
        json!({
            "work_order_id": work_order.id.to_string(),
            "equipment_id": equipment.id.to_string(),
            "plan_id": work_order.plan_id.map(|id| id.to_string()),
            "project_id": work_order.project_id,
            "order_type": work_order.order_type,
            "created_by": caller.actor,
        }),
    );
    if let Some(technician) = &assignee_id {
        create_dispatch_assignment(state, &work_order, technician, None, &caller.actor);
    }
    Ok(work_order)
}

/// Create (or replace) the active dispatch assignment for a work
/// order. A previous active assignment is released first, so at
/// most one active assignment exists per work order.
pub(crate) fn create_dispatch_assignment(
    state: &FieldServiceState,
    order: &WorkOrder,
    technician_id: &str,
    sequence: Option<i64>,
    assigned_by: &str,
) -> DispatchAssignment {
    if let Some(previous) = state.active_assignment_of_order(order.id) {
        if let Some(mut entry) = state.dispatch_assignments.get_mut(&previous.id) {
            entry.value_mut().status = "released".to_string();
            entry.value_mut().released_at = Some(Utc::now());
            entry.value_mut().updated_at = Utc::now();
        }
    }
    let sequence = match sequence {
        Some(value) if value >= 0 => value,
        _ => state.active_assignments_of(technician_id).len() as i64 + 1,
    };
    let now = Utc::now();
    let assignment = DispatchAssignment {
        id: Uuid::now_v7(),
        work_order_id: order.id,
        technician_id: technician_id.to_string(),
        sequence,
        status: "active".to_string(),
        assigned_by: assigned_by.to_string(),
        assigned_at: now,
        started_at: None,
        completed_at: None,
        released_at: None,
        notes: None,
        created_at: now,
        updated_at: now,
    };
    state.dispatch_assignments.insert(assignment.id, assignment.clone());
    state.notify(
        "field_service.dispatch.assigned",
        technician_id,
        &format!("New work order assigned: {}", order.title),
        &format!(
            "Work order \"{}\" ({}) was assigned to you. Priority: {}.",
            order.title, order.id, order.priority
        ),
        Some(format!("/api/v1/field-service/work-orders/{}", order.id)),
        if order.priority == "urgent" { "high" } else { "normal" },
    );
    state.emit(
        "field_service.dispatch.assigned",
        json!({
            "dispatch_assignment_id": assignment.id.to_string(),
            "work_order_id": order.id.to_string(),
            "technician_id": technician_id,
            "sequence": sequence,
            "assigned_by": assigned_by,
        }),
    );
    assignment
}

pub(crate) async fn list_work_orders(
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
        let equipment_filter = params.filters.get("equipment_id").cloned();
        let plan_filter = params.filters.get("plan_id").cloned();
        let project_filter = params.filters.get("project_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let type_filter = params.filters.get("order_type").cloned();
        let priority_filter = params.filters.get("priority").cloned();
        let assignee_filter = params.filters.get("assignee_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<WorkOrder> = state
            .work_orders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|order| {
                if let Some(equipment_id) = &equipment_filter {
                    if order.equipment_id.to_string() != *equipment_id {
                        return false;
                    }
                }
                if let Some(plan_id) = &plan_filter {
                    if order.plan_id.map(|id| id.to_string()) != Some(plan_id.clone()) {
                        return false;
                    }
                }
                if let Some(project_id) = &project_filter {
                    if order.project_id.as_deref() != Some(project_id.as_str()) {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if order.status.as_str() != status {
                        return false;
                    }
                }
                if let Some(order_type) = &type_filter {
                    if &order.order_type != order_type {
                        return false;
                    }
                }
                if let Some(priority) = &priority_filter {
                    if &order.priority != priority {
                        return false;
                    }
                }
                if let Some(assignee) = &assignee_filter {
                    if order.assignee_id.as_deref() != Some(assignee.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        order.title,
                        order.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "due_at", "priority", "status"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "due_at" => a.due_at.cmp(&b.due_at),
                "priority" => priority_rank(&a.priority).cmp(&priority_rank(&b.priority)),
                "status" => a.status.as_str().cmp(b.status.as_str()),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    body: web::Json<CreateWorkOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrder> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        insert_work_order(&state, &caller, body.into_inner())
    })();
    match result {
        Ok(order) => created(order),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        let order = load_work_order(&state, &path.into_inner())?;
        Ok(json!({
            "work_order": order,
            "items": items_of(&state, order.id),
            "parts": parts_of(&state, order.id),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateWorkOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrder> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let mut order = load_work_order(&state, &path.into_inner())?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot be updated",
            ));
        }
        let input = body.into_inner();
        if let Some(title) = input.title {
            order.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = input.description {
            order.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(priority) = input.priority {
            let priority = priority.trim().to_string();
            if !state.definition_exists("priority", &priority) {
                return Err(UnifiedError::bad_request(format!("Unknown priority: {priority}")));
            }
            order.priority = priority;
        }
        if let Some(project_id) = input.project_id {
            order.project_id = match project_id {
                Some(raw) => Some(require_text(&raw, "project_id", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(plan_id) = input.plan_id {
            match plan_id {
                Some(id) => {
                    let plan = load_plan(&state, &id.to_string())?;
                    if plan.equipment_id != order.equipment_id {
                        return Err(UnifiedError::bad_request(
                            "Maintenance plan does not belong to this equipment",
                        ));
                    }
                    order.plan_id = Some(id);
                }
                None => order.plan_id = None,
            }
        }
        if let Some(scheduled_at) = input.scheduled_at {
            order.scheduled_at = scheduled_at;
        }
        if let Some(due_at) = input.due_at {
            order.due_at = due_at;
        }
        if let (Some(scheduled), Some(due)) = (order.scheduled_at, order.due_at) {
            if due < scheduled {
                return Err(UnifiedError::bad_request(
                    "due_at must not be earlier than scheduled_at",
                ));
            }
        }
        order.updated_at = Utc::now();
        state.work_orders.insert(order.id, order.clone());
        state.emit(
            "field_service.work_order.updated",
            json!({
                "work_order_id": order.id.to_string(),
                "updated_by": caller.actor,
            }),
        );
        Ok(order)
    })();
    match result {
        Ok(order) => ok(order),
        Err(error) => err(error, debug),
    }
}

/// Remove a work order and everything that belongs to it:
/// checklist items, parts, sign-offs, and the active dispatch
/// assignment (released, not deleted, so the dispatch history
/// stays intact). Emits `field_service.work_order.deleted` with
/// the same payload the old `.updated` event carried.
pub(crate) fn delete_work_order_records(
    state: &FieldServiceState,
    order: &WorkOrder,
    actor: &str,
) {
    for item in items_of(state, order.id) {
        state.items.remove(&item.id);
    }
    for part in parts_of(state, order.id) {
        state.parts.remove(&part.id);
    }
    let signoff_ids: Vec<Uuid> = state
        .signoffs
        .iter()
        .filter(|entry| entry.value().work_order_id == order.id)
        .map(|entry| *entry.key())
        .collect();
    for id in signoff_ids {
        state.signoffs.remove(&id);
    }
    if let Some(assignment) = state.active_assignment_of_order(order.id) {
        if let Some(mut entry) = state.dispatch_assignments.get_mut(&assignment.id) {
            entry.value_mut().status = "released".to_string();
            entry.value_mut().released_at = Some(Utc::now());
            entry.value_mut().updated_at = Utc::now();
        }
    }
    state.work_orders.remove(&order.id);
    state.emit(
        "field_service.work_order.deleted",
        json!({
            "work_order_id": order.id.to_string(),
            "deleted": true,
            "deleted_by": actor,
        }),
    );
}

pub(crate) async fn delete_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let order = load_work_order(&state, &path.into_inner())?;
        if !matches!(order.status, WorkOrderStatus::Open | WorkOrderStatus::Cancelled) {
            return Err(UnifiedError::conflict(
                "Only an open or cancelled work order can be deleted",
            ));
        }
        delete_work_order_records(&state, &order, &caller.actor);
        Ok(json!({ "deleted": true, "work_order_id": order.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Apply a validated status transition, stamping the lifecycle
/// timestamps. Shared by the status endpoint, the cancel endpoint,
/// and sign-off. Completing a preventive order linked to a plan
/// resets that plan's next-due date and emits the maintenance
/// completion events.
pub(crate) fn apply_work_order_transition(
    state: &FieldServiceState,
    order: &mut WorkOrder,
    next: WorkOrderStatus,
    actor: &str,
) -> UnifiedResult<()> {
    if !order.status.allowed_next().contains(&next) {
        return Err(UnifiedError::conflict(format!(
            "Invalid work order status transition from \"{}\" to \"{}\"",
            order.status.as_str(),
            next.as_str()
        )));
    }
    let now = Utc::now();
    order.status = next;
    order.updated_at = now;
    match next {
        WorkOrderStatus::Completed => {
            order.completed_at = Some(now);
        }
        WorkOrderStatus::Verified => {
            order.verified_at = Some(now);
        }
        WorkOrderStatus::Closed => {
            order.closed_at = Some(now);
        }
        WorkOrderStatus::Cancelled => {
            order.cancelled_at = Some(now);
        }
        WorkOrderStatus::Open => {
            order.assignee_id = None;
        }
        _ => {}
    }
    state.work_orders.insert(order.id, order.clone());
    state.emit(
        "field_service.work_order.status_changed",
        json!({
            "work_order_id": order.id.to_string(),
            "status": next.as_str(),
            "changed_by": actor,
        }),
    );
    if next == WorkOrderStatus::Completed {
        state.emit(
            "field_service.work_order.completed",
            json!({
                "work_order_id": order.id.to_string(),
                "equipment_id": order.equipment_id.to_string(),
                "plan_id": order.plan_id.map(|id| id.to_string()),
                "completed_by": actor,
            }),
        );
        // A completed preventive order resets its plan's next-due
        // date: the service is done, so the interval restarts now.
        if order.order_type == "preventive" {
            if let Some(plan_id) = order.plan_id {
                if let Some(mut plan) = state.plans.get(&plan_id).map(|entry| entry.value().clone()) {
                    let equipment_reading = state
                        .equipment
                        .get(&order.equipment_id)
                        .and_then(|entry| entry.value().usage_reading);
                    plan.last_service_at = Some(now);
                    plan.usage_at_last_service = equipment_reading;
                    if let Ok((next_due_at, next_due_usage)) = compute_next_due(state, &plan) {
                        plan.next_due_at = next_due_at;
                        plan.next_due_usage = next_due_usage;
                    }
                    plan.updated_at = now;
                    state.plans.insert(plan.id, plan.clone());
                    state.emit(
                        "field_service.plan.next_due_reset",
                        json!({
                            "plan_id": plan.id.to_string(),
                            "equipment_id": order.equipment_id.to_string(),
                            "work_order_id": order.id.to_string(),
                            "next_due_at": plan.next_due_at.map(|due| due.to_rfc3339()),
                            "next_due_usage": plan.next_due_usage,
                        }),
                    );
                    state.emit(
                        "field_service.maintenance.completed",
                        json!({
                            "plan_id": plan.id.to_string(),
                            "equipment_id": order.equipment_id.to_string(),
                            "work_order_id": order.id.to_string(),
                            "completed_at": now.to_rfc3339(),
                        }),
                    );
                    if let Some(equipment) = state
                        .equipment
                        .get(&order.equipment_id)
                        .map(|entry| entry.value().clone())
                    {
                        if let Some(asset_id) = equipment.asset_id {
                            state.emit(
                                "assets.maintenance.completed",
                                json!({
                                    "asset_id": asset_id.to_string(),
                                    "equipment_id": equipment.id.to_string(),
                                    "plan_id": plan.id.to_string(),
                                    "work_order_id": order.id.to_string(),
                                    "completed_at": now.to_rfc3339(),
                                }),
                            );
                        }
                    }
                }
            }
        }
    }
    if next == WorkOrderStatus::Verified {
        state.emit(
            "field_service.work_order.verified",
            json!({
                "work_order_id": order.id.to_string(),
                "verified_by": actor,
            }),
        );
    }
    Ok(())
}

pub(crate) async fn change_work_order_status(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ChangeStatusRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrder> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let mut order = load_work_order(&state, &path.into_inner())?;
        can_act_on_order(&ctx, &caller, &order)?;
        let input = body.into_inner();
        let next = WorkOrderStatus::from_str(input.status.trim()).ok_or_else(|| {
            UnifiedError::bad_request(format!("Unknown work order status: {}", input.status))
        })?;
        if let Some(note) = &input.note {
            let _ = require_text(note, "note", MAX_NOTES_LEN)?;
        }
        if next == WorkOrderStatus::Cancelled {
            return Err(UnifiedError::bad_request(
                "Use POST /cancel to cancel a work order",
            ));
        }
        if next == WorkOrderStatus::Verified {
            return Err(UnifiedError::bad_request(
                "Verification happens through customer sign-off (POST /sign-off)",
            ));
        }
        if next == WorkOrderStatus::Assigned {
            return Err(UnifiedError::bad_request(
                "Use POST /assign to assign a technician",
            ));
        }
        apply_work_order_transition(&state, &mut order, next, &caller.actor)?;
        Ok(order)
    })();
    match result {
        Ok(order) => ok(order),
        Err(error) => err(error, debug),
    }
}

/// Statuses an assignment may target: an open order takes its
/// first technician and an assigned order may be reassigned. An
/// order on hold must be resumed first; every other status cannot
/// be assigned at all.
pub(crate) fn check_assignable(order: &WorkOrder) -> UnifiedResult<()> {
    if order.status == WorkOrderStatus::OnHold {
        return Err(UnifiedError::conflict(
            "A work order on hold cannot be assigned; resume the order first",
        ));
    }
    if !matches!(
        order.status,
        WorkOrderStatus::Open | WorkOrderStatus::Assigned
    ) {
        return Err(UnifiedError::conflict(format!(
            "A work order in status \"{}\" cannot be assigned",
            order.status.as_str()
        )));
    }
    Ok(())
}

pub(crate) async fn assign_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssignRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrder> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let mut order = load_work_order(&state, &path.into_inner())?;
        check_assignable(&order)?;
        let input = body.into_inner();
        let technician_id = input.technician_id.trim().to_string();
        if technician_id.is_empty() {
            return Err(UnifiedError::bad_request("technician_id must not be empty"));
        }
        if let Some(sequence) = input.sequence {
            if sequence < 0 || sequence > 10_000 {
                return Err(UnifiedError::bad_request(
                    "sequence must be between 0 and 10000",
                ));
            }
        }
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        order.assignee_id = Some(technician_id.clone());
        if order.status == WorkOrderStatus::Open {
            order.status = WorkOrderStatus::Assigned;
        }
        order.updated_at = Utc::now();
        state.work_orders.insert(order.id, order.clone());
        let assignment =
            create_dispatch_assignment(&state, &order, &technician_id, input.sequence, &caller.actor);
        if let Some(notes) = notes {
            if let Some(mut entry) = state.dispatch_assignments.get_mut(&assignment.id) {
                entry.value_mut().notes = Some(notes);
                entry.value_mut().updated_at = Utc::now();
            }
        }
        state.emit(
            "field_service.work_order.assigned",
            json!({
                "work_order_id": order.id.to_string(),
                "technician_id": technician_id,
                "assigned_by": caller.actor,
            }),
        );
        Ok(order)
    })();
    match result {
        Ok(order) => ok(order),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn cancel_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CancelRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrder> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let mut order = load_work_order(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(reason) = &input.reason {
            let _ = require_text(reason, "reason", MAX_NOTES_LEN)?;
        }
        apply_work_order_transition(&state, &mut order, WorkOrderStatus::Cancelled, &caller.actor)?;
        if let Some(assignment) = state.active_assignment_of_order(order.id) {
            if let Some(mut entry) = state.dispatch_assignments.get_mut(&assignment.id) {
                entry.value_mut().status = "released".to_string();
                entry.value_mut().released_at = Some(Utc::now());
                entry.value_mut().updated_at = Utc::now();
            }
        }
        state.emit(
            "field_service.work_order.cancelled",
            json!({
                "work_order_id": order.id.to_string(),
                "reason": input.reason,
                "cancelled_by": caller.actor,
            }),
        );
        Ok(order)
    })();
    match result {
        Ok(order) => ok(order),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::field_service::Signoff;

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

    fn seed_order_with_children(state: &FieldServiceState) -> WorkOrder {
        let now = Utc::now();
        let order = work_order(WorkOrderStatus::Open);
        state.work_orders.insert(order.id, order.clone());
        let item_id = Uuid::new_v4();
        state.items.insert(
            item_id,
            WorkOrderItem {
                id: item_id,
                work_order_id: order.id,
                title: "Check seal".to_string(),
                position: 0,
                is_done: false,
                done_by: None,
                done_at: None,
                notes: None,
                created_at: now,
                updated_at: now,
            },
        );
        let part_id = Uuid::new_v4();
        state.parts.insert(
            part_id,
            WorkOrderPart {
                id: part_id,
                work_order_id: order.id,
                stock_id: None,
                part_name: "Seal".to_string(),
                quantity: 1,
                unit: "pcs".to_string(),
                status: "required".to_string(),
                consumed_at: None,
                created_by: "test".to_string(),
                created_at: now,
                updated_at: now,
            },
        );
        let signoff_id = Uuid::new_v4();
        state.signoffs.insert(
            signoff_id,
            Signoff {
                id: signoff_id,
                work_order_id: order.id,
                customer_name: "Customer".to_string(),
                signature_ref: "sig-1".to_string(),
                signed_at: now,
                notes: None,
                survey_distribution_id: None,
                follow_up_status: "pending_no_distribution".to_string(),
                created_by: "test".to_string(),
                created_at: now,
                updated_at: now,
            },
        );
        let assignment_id = Uuid::new_v4();
        state.dispatch_assignments.insert(
            assignment_id,
            DispatchAssignment {
                id: assignment_id,
                work_order_id: order.id,
                technician_id: "tech-1".to_string(),
                sequence: 0,
                status: "active".to_string(),
                assigned_by: "test".to_string(),
                assigned_at: now,
                started_at: None,
                completed_at: None,
                released_at: None,
                notes: None,
                created_at: now,
                updated_at: now,
            },
        );
        order
    }

    #[test]
    fn check_assignable_rejects_on_hold_orders() {
        let error = check_assignable(&work_order(WorkOrderStatus::OnHold)).unwrap_err();
        assert!(error.to_string().contains("resume the order first"));
        assert!(check_assignable(&work_order(WorkOrderStatus::Open)).is_ok());
        assert!(check_assignable(&work_order(WorkOrderStatus::Assigned)).is_ok());
        assert!(check_assignable(&work_order(WorkOrderStatus::Completed)).is_err());
    }

    #[test]
    fn delete_work_order_records_removes_signoffs_items_and_parts() {
        let state = FieldServiceState::seeded();
        let order = seed_order_with_children(&state);
        delete_work_order_records(&state, &order, "test");
        assert!(state.work_orders.get(&order.id).is_none());
        assert!(state.items.is_empty());
        assert!(state.parts.is_empty());
        assert!(
            state.signoffs.is_empty(),
            "sign-offs of a deleted order must not survive"
        );
        let assignment = state
            .dispatch_assignments
            .iter()
            .next()
            .expect("assignment still stored")
            .value()
            .clone();
        assert_eq!(assignment.status, "released");
    }

    #[test]
    fn delete_work_order_records_emits_deleted_not_updated() {
        use std::sync::Mutex;

        let state = FieldServiceState::seeded();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let bus = Arc::new(HookBus::new());
        for event in [
            "field_service.work_order.deleted",
            "field_service.work_order.updated",
        ] {
            let seen = Arc::clone(&seen);
            bus.register(
                event,
                "test",
                Arc::new(move |ctx: &mut HookContext| {
                    seen.lock()
                        .expect("seen lock poisoned")
                        .push(ctx.event.clone());
                }),
            );
        }
        *state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&bus));
        let order = seed_order_with_children(&state);
        delete_work_order_records(&state, &order, "test");
        let seen = seen.lock().expect("seen lock poisoned");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0], "field_service.work_order.deleted");
    }
}
