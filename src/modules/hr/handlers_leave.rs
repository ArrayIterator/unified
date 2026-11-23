//! HTTP handlers: leave types, balances and requests.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Handlers: leave types
// ---------------------------------------------------------------------------

pub(crate) async fn list_leave_types(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<LeaveType>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let params = query.into_inner();
        let mut items: Vec<LeaveType> = state
            .leave_types
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|leave_type| {
                matches_search(&format!("{} {}", leave_type.name, leave_type.code), &params)
                    && params
                        .filters
                        .get("paid")
                        .is_none_or(|flag| leave_type.paid == (flag == "true"))
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn create_leave_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    body: web::Json<CreateLeaveType>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveType> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 255)?;
        let code = match input.code {
            Some(code) if !code.trim().is_empty() => slugify(&code),
            _ => slugify(&name),
        };
        if code.is_empty() {
            return Err(UnifiedError::bad_request("code must not be empty"));
        }
        if state.leave_types.iter().any(|entry| entry.value().code == code) {
            return Err(UnifiedError::conflict(format!(
                "A leave type with code \"{code}\" already exists"
            )));
        }
        let default_days = match input.default_days_per_year {
            Some(days) => parse_days(days, "default_days_per_year")?,
            None => 0.0,
        };
        let leave_type = LeaveType {
            id: new_id(),
            name,
            code,
            default_days_per_year: default_days,
            paid: input.paid.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state.leave_types.insert(leave_type.id.clone(), leave_type.clone());
        Ok(leave_type)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_leave_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveType> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        state
            .leave_types
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Leave type not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_leave_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateLeaveType>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveType> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let default_days = match input.default_days_per_year {
            Some(days) => Some(parse_days(days, "default_days_per_year")?),
            None => None,
        };
        let mut leave_type = state
            .leave_types
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Leave type not found"))?;
        if let Some(name) = input.name {
            leave_type.name = require_text(&name, "name", 255)?;
        }
        if let Some(days) = default_days {
            leave_type.default_days_per_year = days;
        }
        if let Some(paid) = input.paid {
            leave_type.paid = paid;
        }
        leave_type.updated_at = now();
        Ok(leave_type.clone())
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_leave_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let leave_type_id = path.as_str().to_string();
        // Balances reference the type too: deleting it while any
        // balance row exists would orphan balances that can still
        // be charged.
        if state.leave_type_in_use(leave_type_id.as_str()) {
            return Err(UnifiedError::conflict(
                "Leave type still has leave requests or balances; keep it for history",
            ));
        }
        state
            .leave_types
            .remove(leave_type_id.as_str())
            .map(|_| json!({ "deleted": true, "id": leave_type_id }))
            .ok_or_else(|| UnifiedError::not_found("Leave type not found"))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: leave balances
// ---------------------------------------------------------------------------

pub(crate) async fn list_leave_balances(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let employee = load_employee_for_read(&state, &ctx, &caller, path.as_str())?;
        let params = query.into_inner();
        let year_filter = params
            .filters
            .get("year")
            .and_then(|raw| raw.parse::<i32>().ok());
        let mut items: Vec<Value> = state
            .leave_balances
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|balance| {
                balance.employee_id == employee.id
                    && year_filter.is_none_or(|year| balance.year == year)
                    && params
                        .filters
                        .get("leave_type_id")
                        .is_none_or(|leave_type| balance.leave_type_id == *leave_type)
            })
            .map(|balance| {
                json!({
                    "id": balance.id,
                    "employee_id": balance.employee_id,
                    "leave_type_id": balance.leave_type_id,
                    "year": balance.year,
                    "entitled_days": balance.entitled_days,
                    "used_days": balance.used_days,
                    "carried_over_days": balance.carried_over_days,
                    "remaining_days": balance.remaining(),
                    "created_at": balance.created_at,
                    "updated_at": balance.updated_at,
                })
            })
            .collect();
        items.sort_by(|a, b| {
            a.get("year")
                .and_then(|v| v.as_i64())
                .cmp(&b.get("year").and_then(|v| v.as_i64()))
        });
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn create_leave_balance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateLeaveBalance>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveBalance> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let employee_id = path.as_str().to_string();
        if !state.employees.contains_key(employee_id.as_str()) {
            return Err(UnifiedError::not_found("Employee not found"));
        }
        // Hold the employee's leave lock across the duplicate check
        // and the insert: without it, two concurrent creates for the
        // same employee, type, and year can both pass the check and
        // insert duplicate balance rows (the same race
        // `ensure_balance` already closes with this lock).
        let lock = state.leave_lock(&employee_id);
        let _guard = lock
            .lock()
            .map_err(|_| UnifiedError::internal("Leave lock was poisoned"))?;
        let input = body.into_inner();
        if !state.leave_types.contains_key(input.leave_type_id.as_str()) {
            return Err(UnifiedError::not_found("Leave type not found"));
        }
        if !(2000..=2100).contains(&input.year) {
            return Err(UnifiedError::bad_request("year must be between 2000 and 2100"));
        }
        if state.leave_balances.iter().any(|entry| {
            let balance = entry.value();
            balance.employee_id == employee_id
                && balance.leave_type_id == input.leave_type_id
                && balance.year == input.year
        }) {
            return Err(UnifiedError::conflict(
                "A balance already exists for this employee, leave type, and year",
            ));
        }
        let leave_type = state
            .leave_types
            .get(input.leave_type_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Leave type not found"))?;
        let entitled_days = match input.entitled_days {
            Some(days) => parse_days(days, "entitled_days")?,
            None => leave_type.default_days_per_year,
        };
        let carried_over_days = match input.carried_over_days {
            Some(days) => parse_days(days, "carried_over_days")?,
            None => 0.0,
        };
        let balance = LeaveBalance {
            id: new_id(),
            employee_id,
            leave_type_id: input.leave_type_id,
            year: input.year,
            entitled_days,
            used_days: 0.0,
            carried_over_days,
            created_at: now(),
            updated_at: now(),
        };
        state.leave_balances.insert(balance.id.clone(), balance.clone());
        Ok(balance)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn update_leave_balance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateLeaveBalance>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveBalance> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let entitled_days = match input.entitled_days {
            Some(days) => Some(parse_days(days, "entitled_days")?),
            None => None,
        };
        let carried_over_days = match input.carried_over_days {
            Some(days) => Some(parse_days(days, "carried_over_days")?),
            None => None,
        };
        let mut balance = state
            .leave_balances
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Leave balance not found"))?;
        // Validate before writing: the entry is a live reference, so
        // a failed check after assignment would leave bad values stored.
        let new_entitled = entitled_days.unwrap_or(balance.entitled_days);
        let new_carried = carried_over_days.unwrap_or(balance.carried_over_days);
        if balance.used_days > new_entitled + new_carried {
            return Err(UnifiedError::unprocessable(
                "used_days already exceeds the new entitlement plus carried-over days",
            ));
        }
        balance.entitled_days = new_entitled;
        balance.carried_over_days = new_carried;
        balance.updated_at = now();
        Ok(balance.clone())
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: leave requests
// ---------------------------------------------------------------------------

pub(crate) async fn list_leave_requests(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<LeaveRequest>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_HR_READ_ALL);
        let own_employee_id = state
            .employee_by_user(&caller.actor)
            .map(|employee| employee.id);
        let mut items: Vec<LeaveRequest> = state
            .leave_requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| {
                (read_all || Some(request.employee_id.as_str()) == own_employee_id.as_deref())
                    && params
                        .filters
                        .get("employee_id")
                        .is_none_or(|employee| request.employee_id == *employee)
                    && params
                        .filters
                        .get("leave_type_id")
                        .is_none_or(|leave_type| request.leave_type_id == *leave_type)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| request.status == *status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn create_leave_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    body: web::Json<CreateLeaveRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let input = body.into_inner();
        // An employee files for themself; HR (`hr:manage`) may file on
        // behalf of any employee by passing `employee_id`.
        let employee_id = match &input.employee_id {
            Some(employee_id) if !employee_id.trim().is_empty() => {
                if !check_capability(&ctx, &caller.role, PERM_HR_MANAGE) {
                    return Err(UnifiedError::forbidden(
                        "Only HR managers may file leave for another employee",
                    ));
                }
                employee_id.trim().to_string()
            }
            _ => state
                .employee_by_user(&caller.actor)
                .map(|employee| employee.id)
                .ok_or_else(|| {
                    UnifiedError::not_found("No employee record exists for the caller")
                })?,
        };
        let employee = state
            .employees
            .get(employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        if !state.leave_types.contains_key(input.leave_type_id.as_str()) {
            return Err(UnifiedError::not_found("Leave type not found"));
        }
        let start_date = parse_date(&input.start_date, "start_date")?;
        let end_date = parse_date(&input.end_date, "end_date")?;
        if end_date < start_date {
            return Err(UnifiedError::bad_request(
                "end_date must not be before start_date",
            ));
        }
        // Employees may not backdate leave; HR may, for corrections.
        if start_date < today() && !check_capability(&ctx, &caller.role, PERM_HR_MANAGE) {
            return Err(UnifiedError::bad_request(
                "start_date must not be in the past",
            ));
        }
        if state.leave_overlaps(&employee.id, start_date, end_date, None) {
            return Err(UnifiedError::conflict(
                "The requested dates overlap an existing pending or approved leave request",
            ));
        }
        let days = (end_date - start_date).num_days() as f64 + 1.0;
        validate_leave_span(days)?;
        let reason = optional_text(&input.reason, "reason", MAX_TEXT)?.unwrap_or_default();
        let request = LeaveRequest {
            id: new_id(),
            employee_id: employee.id.clone(),
            leave_type_id: input.leave_type_id,
            start_date,
            end_date,
            days,
            status: "pending".to_string(),
            reason,
            reviewed_by: None,
            review_note: None,
            created_at: now(),
            updated_at: now(),
        };
        state.leave_requests.insert(request.id.clone(), request.clone());
        state.emit(
            "hr.leave_requested",
            json!({
                "leave_request_id": request.id,
                "employee_id": request.employee_id,
                "leave_type_id": request.leave_type_id,
                "start_date": request.start_date.to_string(),
                "end_date": request.end_date.to_string(),
                "days": request.days,
            }),
        );
        Ok(request)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_leave_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let request = state
            .leave_requests
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Leave request not found"))?;
        let employee = state
            .employees
            .get(request.employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        require_read_employee(&ctx, &caller, &employee)?;
        Ok(request)
    })();
    json::respond(result, debug)
}

pub(crate) async fn approve_leave_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewLeaveRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let review_note = optional_text(&input.reason, "reason", MAX_TEXT)?;
        let request_id = path.as_str().to_string();
        let snapshot = state
            .leave_requests
            .get(request_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Leave request not found"))?;
        if snapshot.status != "pending" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending leave request can be approved (current status: {})",
                snapshot.status
            )));
        }
        let employee_id = snapshot.employee_id.clone();
        let leave_type_id = snapshot.leave_type_id.clone();
        let start_date = snapshot.start_date;
        let end_date = snapshot.end_date;
        // Serialize approvals for this employee: the pending check,
        // the overlap check, and the balance check-and-deduct all
        // happen under one lock, so two concurrent approvals can
        // neither both approve overlapping leaves nor overdraw the
        // same balance.
        let approved = {
            let lock = state.leave_lock(&employee_id);
            let _guard = lock
                .lock()
                .map_err(|_| UnifiedError::internal("Leave lock was poisoned"))?;
            let current = state
                .leave_requests
                .get(request_id.as_str())
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found("Leave request not found"))?;
            if current.status != "pending" {
                return Err(UnifiedError::conflict(format!(
                    "Only a pending leave request can be approved (current status: {})",
                    current.status
                )));
            }
            if state.leave_overlaps(&employee_id, start_date, end_date, Some(request_id.as_str()))
            {
                return Err(UnifiedError::conflict(
                    "The requested dates overlap another approved leave request",
                ));
            }
            // Days are charged per calendar year, so a request
            // crossing 1 January draws from each year's balance.
            state.charge_leave_balances_locked(
                &employee_id,
                &leave_type_id,
                start_date,
                end_date,
            )?;
            let mut request = state
                .leave_requests
                .get_mut(request_id.as_str())
                .ok_or_else(|| UnifiedError::not_found("Leave request not found"))?;
            request.status = "approved".to_string();
            request.reviewed_by = Some(caller.actor.clone());
            request.review_note = review_note;
            request.updated_at = now();
            request.clone()
        };
        state.emit(
            "hr.leave_approved",
            json!({
                "leave_request_id": approved.id,
                "employee_id": approved.employee_id,
                "days": approved.days,
            }),
        );
        let employee_user_id = state
            .employees
            .get(approved.employee_id.as_str())
            .map(|entry| entry.value().user_id.clone());
        if let Some(user_id) = employee_user_id {
            state.emit(
                "notification.requested",
                json!({
                    "user_id": user_id,
                    "title": "Leave request approved",
                    "body": format!(
                        "Your leave request for {} day(s) was approved.",
                        approved.days
                    ),
                    "kind": "hr",
                }),
            );
        }
        Ok(approved)
    })();
    json::respond(result, debug)
}

pub(crate) async fn reject_leave_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewLeaveRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let reason = require_text(
            input.reason.as_deref().unwrap_or(""),
            "reason",
            MAX_TEXT,
        )?;
        let mut request = state
            .leave_requests
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Leave request not found"))?;
        if request.status != "pending" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending leave request can be rejected (current status: {})",
                request.status
            )));
        }
        request.status = "rejected".to_string();
        request.reviewed_by = Some(caller.actor.clone());
        request.review_note = Some(reason.clone());
        request.updated_at = now();
        let rejected = request.clone();
        drop(request);
        state.emit(
            "hr.leave_rejected",
            json!({
                "leave_request_id": rejected.id,
                "employee_id": rejected.employee_id,
                "reason": reason,
            }),
        );
        let employee_user_id = state
            .employees
            .get(rejected.employee_id.as_str())
            .map(|entry| entry.value().user_id.clone());
        if let Some(user_id) = employee_user_id {
            state.emit(
                "notification.requested",
                json!({
                    "user_id": user_id,
                    "title": "Leave request rejected",
                    "body": format!("Your leave request was rejected: {reason}"),
                    "kind": "hr",
                }),
            );
        }
        Ok(rejected)
    })();
    json::respond(result, debug)
}

pub(crate) async fn cancel_leave_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let request_id = path.as_str().to_string();
        let request = state
            .leave_requests
            .get(request_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Leave request not found"))?;
        let employee = state
            .employees
            .get(request.employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        // The employee themself, or HR with `hr:manage`.
        if employee.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        }
        if !matches!(request.status.as_str(), "pending" | "approved") {
            return Err(UnifiedError::conflict(format!(
                "Only a pending or approved leave request can be cancelled (current status: {})",
                request.status
            )));
        }
        // Refund under the employee's leave lock, re-checking the
        // status there: a concurrent approval may have approved a
        // pending request meanwhile. A failed refund fails the
        // cancellation outright — the days must never be silently
        // lost while the request is still cancelled.
        let cancelled = {
            let lock = state.leave_lock(&request.employee_id);
            let _guard = lock
                .lock()
                .map_err(|_| UnifiedError::internal("Leave lock was poisoned"))?;
            let current = state
                .leave_requests
                .get(request_id.as_str())
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found("Leave request not found"))?;
            if !matches!(current.status.as_str(), "pending" | "approved") {
                return Err(UnifiedError::conflict(format!(
                    "Only a pending or approved leave request can be cancelled (current status: {})",
                    current.status
                )));
            }
            // An approved cancellation returns the days to the
            // balances, split across years the way they were charged.
            if current.status == "approved" {
                state.refund_leave_balances_locked(
                    &current.employee_id,
                    &current.leave_type_id,
                    current.start_date,
                    current.end_date,
                )?;
            }
            let mut stored = state
                .leave_requests
                .get_mut(request_id.as_str())
                .ok_or_else(|| UnifiedError::not_found("Leave request not found"))?;
            stored.status = "cancelled".to_string();
            stored.updated_at = now();
            stored.clone()
        };
        state.emit(
            "hr.leave_cancelled",
            json!({
                "leave_request_id": cancelled.id,
                "employee_id": cancelled.employee_id,
                "days": cancelled.days,
            }),
        );
        let employee_user_id = state
            .employees
            .get(cancelled.employee_id.as_str())
            .map(|entry| entry.value().user_id.clone());
        if let Some(user_id) = employee_user_id {
            state.emit(
                "notification.requested",
                json!({
                    "user_id": user_id,
                    "title": "Leave request cancelled",
                    "body": format!(
                        "Your leave request for {} day(s) was cancelled.",
                        cancelled.days
                    ),
                    "kind": "hr",
                }),
            );
        }
        Ok(cancelled)
    })();
    json::respond(result, debug)
}

/// Reject a leave request longer than one year of days. Without a
/// cap, a multi-year pending request can be filed and blocks every
/// overlapping request until approval finally refuses it.
pub(crate) fn validate_leave_span(days: f64) -> UnifiedResult<()> {
    if days > MAX_LEAVE_REQUEST_DAYS {
        return Err(UnifiedError::bad_request(format!(
            "Leave request must not span more than {MAX_LEAVE_REQUEST_DAYS} days"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leave_span_up_to_one_year_is_allowed() {
        assert!(validate_leave_span(1.0).is_ok());
        assert!(validate_leave_span(366.0).is_ok());
    }

    #[test]
    fn leave_span_over_one_year_is_rejected() {
        assert!(validate_leave_span(367.0).is_err());
        assert!(validate_leave_span(730.0).is_err());
    }
}
