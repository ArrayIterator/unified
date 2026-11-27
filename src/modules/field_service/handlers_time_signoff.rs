// field_service module - Time logging, incident raising, sign-off, report.
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

use super::constants::{MAX_DESCRIPTION_LEN, MAX_NAME_LEN, MAX_NOTES_LEN, MAX_TITLE_LEN, PERM_FIELD_SERVICE_VIEW};
use super::types::{DispatchAssignment, Signoff, WorkOrderStatus};
use super::requests::{RaiseIncidentRequest, SignOffRequest, TimeLogRequest};
use super::state::FieldServiceState;
use super::helpers::{
    caller, caller_company, can_act_on_order, created, err, items_of, load_work_order, ok,
    optional_text, parts_of, require_permission, require_tenant, require_text,
};
use super::handlers_work_orders::apply_work_order_transition;

// Time logging, incident raising, sign-off, report

/// Log time on a work order through the timetracker module. The
/// timetracker module exposes no log-request listener yet, so the
/// request is an event (`timetracker.time.log_requested`) and the
/// answer is reported honestly as `handler_answered`.
pub(crate) async fn log_work_order_time(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TimeLogRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let order = load_work_order(&state, &path.into_inner())?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "Time cannot be logged on a closed or cancelled work order",
            ));
        }
        let input = body.into_inner();
        let technician_id = input.technician_id.trim().to_string();
        if technician_id.is_empty() {
            return Err(UnifiedError::bad_request("technician_id must not be empty"));
        }
        if !input.hours.is_finite() || input.hours <= 0.0 || input.hours > 24.0 {
            return Err(UnifiedError::bad_request(
                "hours must be a finite number between 0 and 24",
            ));
        }
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let answered = state.emit_request(
            "timetracker.time.log_requested",
            json!({
                "work_order_id": order.id.to_string(),
                "equipment_id": order.equipment_id.to_string(),
                "project_id": order.project_id,
                "user_id": technician_id,
                "technician_id": technician_id,
                "hours": input.hours,
                "description": description,
                "logged_at": input.logged_at.map(|at| at.to_rfc3339()),
                "source": "field_service",
                "logged_by": caller.actor,
            }),
        );
        state.emit(
            "field_service.work_order.time_logged",
            json!({
                "work_order_id": order.id.to_string(),
                "technician_id": technician_id,
                "hours": input.hours,
                "handler_answered": answered,
                "logged_by": caller.actor,
            }),
        );
        Ok(json!({
            "work_order_id": order.id.to_string(),
            "technician_id": technician_id,
            "hours": input.hours,
            "status": "requested",
            "handler_answered": answered,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Request an incident for a failure found on the job. The
/// incidents module listens to no create-request event yet, so the
/// request is an event (`incidents.create_requested`) and the
/// answer is reported honestly as `handler_answered`.
pub(crate) async fn raise_incident(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RaiseIncidentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let order = load_work_order(&state, &path.into_inner())?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "An incident cannot be raised from a closed or cancelled work order",
            ));
        }
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let severity = match &input.severity {
            Some(raw) => {
                let severity = raw.trim().to_lowercase();
                match severity.as_str() {
                    "critical" | "high" | "medium" | "low" => severity,
                    _ => {
                        return Err(UnifiedError::bad_request(
                            "severity must be \"critical\", \"high\", \"medium\", or \"low\"",
                        ));
                    }
                }
            }
            None => "high".to_string(),
        };
        let answered = state.emit_request(
            "incidents.create_requested",
            json!({
                "title": title,
                "description": description.clone().unwrap_or_else(|| format!(
                    "Failure found during field service work order {}",
                    order.id
                )),
                "severity": severity,
                "source": "field_service",
                "work_order_id": order.id.to_string(),
                "equipment_id": order.equipment_id.to_string(),
                "created_by": caller.actor,
            }),
        );
        Ok(json!({
            "work_order_id": order.id.to_string(),
            "title": title,
            "severity": severity,
            "requested": true,
            "handler_answered": answered,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Follow-up status for a sign-off: `requested` only when a
/// survey distribution was supplied *and* the surveys module
/// answered the send request. When nothing was sent - no
/// distribution, or no handler answered - the follow-up stays
/// `pending_no_distribution` instead of claiming a request went
/// out.
pub(crate) fn signoff_follow_up_status(
    distribution_id: Option<Uuid>,
    answered: bool,
) -> &'static str {
    if distribution_id.is_some() && answered {
        "requested"
    } else {
        "pending_no_distribution"
    }
}

/// Customer sign-off: records the customer name, the storage
/// reference of the captured signature, and the timestamp, then
/// moves the work order from `completed` to `verified`. When a
/// surveys distribution id is supplied, a satisfaction survey
/// send is requested through the surveys module; without one the
/// follow-up is recorded as pending.
pub(crate) async fn sign_off_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SignOffRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Signoff> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let mut order = load_work_order(&state, &path.into_inner())?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status != WorkOrderStatus::Completed {
            return Err(UnifiedError::conflict(format!(
                "Only a completed work order can be signed off (current status: \"{}\")",
                order.status.as_str()
            )));
        }
        if state
            .signoffs
            .iter()
            .any(|entry| entry.value().work_order_id == order.id)
        {
            return Err(UnifiedError::conflict(
                "This work order already has a sign-off",
            ));
        }
        let input = body.into_inner();
        let customer_name = require_text(&input.customer_name, "customer_name", MAX_NAME_LEN)?;
        let signature_ref = require_text(&input.signature_ref, "signature_ref", MAX_NAME_LEN)?;
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        let now = Utc::now();
        let follow_up_status = match input.survey_distribution_id {
            Some(distribution_id) => {
                let answered = state.emit_request(
                    "surveys.send_requested",
                    json!({
                        "distribution_id": distribution_id.to_string(),
                        "work_order_id": order.id.to_string(),
                        "source": "field_service",
                    }),
                );
                signoff_follow_up_status(Some(distribution_id), answered).to_string()
            }
            None => {
                state.emit(
                    "field_service.survey.requested",
                    json!({
                        "work_order_id": order.id.to_string(),
                        "customer_name": customer_name,
                        "reason": "no survey distribution supplied",
                    }),
                );
                signoff_follow_up_status(None, false).to_string()
            }
        };
        let signoff = Signoff {
            id: Uuid::now_v7(),
            work_order_id: order.id,
            customer_name,
            signature_ref,
            signed_at: now,
            notes,
            survey_distribution_id: input.survey_distribution_id,
            follow_up_status,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.signoffs.insert(signoff.id, signoff.clone());
        apply_work_order_transition(&state, &mut order, WorkOrderStatus::Verified, &caller.actor)?;
        state.emit(
            "field_service.signoff.created",
            json!({
                "signoff_id": signoff.id.to_string(),
                "work_order_id": order.id.to_string(),
                "customer_name": signoff.customer_name,
                "follow_up_status": signoff.follow_up_status,
                "created_by": caller.actor,
            }),
        );
        Ok(signoff)
    })();
    match result {
        Ok(signoff) => created(signoff),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_work_order_signoff(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Signoff> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        let order = load_work_order(&state, &path.into_inner())?;
        state
            .signoffs
            .iter()
            .map(|entry| entry.value().clone())
            .find(|signoff| signoff.work_order_id == order.id)
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No sign-off exists for work order: {}",
                    order.id
                ))
            })
    })();
    match result {
        Ok(signoff) => ok(signoff),
        Err(error) => err(error, debug),
    }
}

/// Completion report data for a work order: the order itself, its
/// equipment, checklist completion, parts, dispatch history, and
/// sign-off. Time totals are not summed here because time entries
/// live in the timetracker module.
pub(crate) async fn work_order_report(
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
        let equipment = state
            .equipment
            .get(&order.equipment_id)
            .map(|entry| entry.value().clone());
        let items = items_of(&state, order.id);
        let done_items = items.iter().filter(|item| item.is_done).count();
        let parts = parts_of(&state, order.id);
        let mut assignments: Vec<DispatchAssignment> = state
            .dispatch_assignments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.work_order_id == order.id)
            .collect();
        assignments.sort_by_key(|row| row.assigned_at);
        let signoff = state
            .signoffs
            .iter()
            .map(|entry| entry.value().clone())
            .find(|signoff| signoff.work_order_id == order.id);
        Ok(json!({
            "work_order": order,
            "equipment": equipment,
            "checklist": {
                "total": items.len(),
                "done": done_items,
                "items": items,
            },
            "parts": parts,
            "dispatch_assignments": assignments,
            "signoff": signoff,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signoff_follow_up_status_needs_an_answered_request() {
        assert_eq!(
            signoff_follow_up_status(Some(Uuid::new_v4()), true),
            "requested"
        );
        assert_eq!(
            signoff_follow_up_status(Some(Uuid::new_v4()), false),
            "pending_no_distribution",
            "an unanswered send request must not be recorded as requested"
        );
        assert_eq!(
            signoff_follow_up_status(None, true),
            "pending_no_distribution"
        );
        assert_eq!(
            signoff_follow_up_status(None, false),
            "pending_no_distribution"
        );
    }
}
