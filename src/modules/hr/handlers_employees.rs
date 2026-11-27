//! HTTP handlers: employees and org chart.

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


// Handlers: employees

pub(crate) async fn list_employees(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Employee>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_HR_READ_ALL);
        let mut items: Vec<Employee> = state
            .employees
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|employee| {
                (read_all || employee.user_id == caller.actor)
                    && matches_search(
                        &format!("{} {}", employee.employee_code, employee.user_id),
                        &params,
                    )
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| employee.status == *status)
                    && params
                        .filters
                        .get("department_id")
                        .is_none_or(|department| {
                            employee.department_id.as_deref() == Some(department.as_str())
                        })
                    && params
                        .filters
                        .get("position_id")
                        .is_none_or(|position| {
                            employee.position_id.as_deref() == Some(position.as_str())
                        })
            })
            .collect();
        items.sort_by(|a, b| a.employee_code.cmp(&b.employee_code));
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

pub(crate) async fn create_employee(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    body: web::Json<CreateEmployee>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Employee> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", 255)?;
        if state
            .employees
            .iter()
            .any(|entry| entry.value().user_id == user_id)
        {
            return Err(UnifiedError::conflict(format!(
                "An employee record already exists for user {user_id}"
            )));
        }
        let employee_code = match input.employee_code {
            Some(code) if !code.trim().is_empty() => require_text(&code, "employee_code", 64)?,
            _ => format!("EMP-{}", &new_id()[..8].to_uppercase()),
        };
        if state
            .employees
            .iter()
            .any(|entry| entry.value().employee_code == employee_code)
        {
            return Err(UnifiedError::conflict(format!(
                "An employee with code \"{employee_code}\" already exists"
            )));
        }
        if let Some(position_id) = &input.position_id {
            if !position_id.trim().is_empty()
                && !state.positions.contains_key(position_id.trim())
            {
                return Err(UnifiedError::not_found(format!(
                    "Position {} not found",
                    position_id.trim()
                )));
            }
        }
        let department_id = input
            .department_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let department_verified = match &department_id {
            Some(department_id) => state.department_verified(&user_id, department_id),
            None => false,
        };
        let manager_id = input
            .manager_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(manager_id) = &manager_id {
            if !state.employees.contains_key(manager_id.as_str()) {
                return Err(UnifiedError::not_found(format!(
                    "Manager employee {manager_id} not found"
                )));
            }
        }
        let hire_date = parse_date(&input.hire_date, "hire_date")?;
        let employee = Employee {
            id: new_id(),
            user_id: user_id.clone(),
            employee_code,
            position_id: input
                .position_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            department_id,
            department_verified,
            manager_id,
            status: "active".to_string(),
            hire_date,
            created_at: now(),
            updated_at: now(),
        };
        // A fresh employee cannot manage themself, so no cycle check
        // is needed here; updates check the chain instead.
        state.employees.insert(employee.id.clone(), employee.clone());
        state.emit(
            "hr.employee_created",
            json!({
                "employee_id": employee.id,
                "user_id": employee.user_id,
                "department_id": employee.department_id,
            }),
        );
        Ok(employee)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_employee(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Employee> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        load_employee_for_read(&state, &ctx, &caller, path.as_str())
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_employee(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateEmployee>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Employee> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let employee_id = path.as_str().to_string();
        if let Some(position_id) = &input.position_id {
            if !position_id.trim().is_empty()
                && !state.positions.contains_key(position_id.trim())
            {
                return Err(UnifiedError::not_found(format!(
                    "Position {} not found",
                    position_id.trim()
                )));
            }
        }
        if let Some(Some(manager_id)) = &input.manager_id {
            if !manager_id.trim().is_empty() {
                if !state.employees.contains_key(manager_id.trim()) {
                    return Err(UnifiedError::not_found(format!(
                        "Manager employee {} not found",
                        manager_id.trim()
                    )));
                }
                if state.manager_would_cycle(&employee_id, manager_id.trim()) {
                    return Err(UnifiedError::conflict(
                        "manager_id would create a cycle in the reporting chain",
                    ));
                }
            }
        }
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "on_leave" | "terminated") {
                return Err(UnifiedError::bad_request(
                    "status must be one of: active, on_leave, terminated",
                ));
            }
        }
        let hire_date = match &input.hire_date {
            Some(raw) => Some(parse_date(raw, "hire_date")?),
            None => None,
        };
        let department_verified = match &input.department_id {
            Some(department_id) if !department_id.trim().is_empty() => {
                let user_id = state
                    .employees
                    .get(employee_id.as_str())
                    .map(|entry| entry.value().user_id.clone())
                    .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
                Some(state.department_verified(&user_id, department_id.trim()))
            }
            _ => None,
        };
        // Terminating an employee closes their live HR records
        // first: active contracts are terminated and pending or
        // approved leave is cancelled (approved days refunded). A
        // failed refund aborts the update, so the employee is never
        // left terminated with live contracts or leave behind.
        if matches!(input.status.as_deref(), Some("terminated")) {
            let current_status = state
                .employees
                .get(employee_id.as_str())
                .map(|entry| entry.value().status.clone());
            if current_status
                .as_deref()
                .is_some_and(|status| is_termination_transition(status, "terminated"))
            {
                state.terminate_employee_relationships(&employee_id, today())?;
            }
        }
        let mut employee = state
            .employees
            .get_mut(employee_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        if let Some(position_id) = input.position_id {
            employee.position_id = if position_id.trim().is_empty() {
                None
            } else {
                Some(position_id.trim().to_string())
            };
        }
        if let Some(department_id) = input.department_id {
            employee.department_id = if department_id.trim().is_empty() {
                None
            } else {
                Some(department_id.trim().to_string())
            };
            if let Some(verified) = department_verified {
                employee.department_verified = verified;
            } else if employee.department_id.is_none() {
                employee.department_verified = false;
            }
        }
        if let Some(manager_id) = input.manager_id {
            employee.manager_id = manager_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        if let Some(status) = input.status {
            employee.status = status;
        }
        if let Some(hire_date) = hire_date {
            employee.hire_date = hire_date;
        }
        employee.updated_at = now();
        Ok(employee.clone())
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_employee(
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
        let employee_id = path.as_str().to_string();
        if state
            .contracts
            .iter()
            .any(|entry| entry.value().employee_id == employee_id && entry.value().status == "active")
        {
            return Err(UnifiedError::conflict(
                "Employee still has an active contract; terminate it first",
            ));
        }
        if state.leave_requests.iter().any(|entry| {
            let request = entry.value();
            request.employee_id == employee_id
                && matches!(request.status.as_str(), "pending" | "approved")
        }) {
            return Err(UnifiedError::conflict(
                "Employee still has pending or approved leave requests; cancel them first",
            ));
        }
        if state
            .employees
            .iter()
            .any(|entry| entry.value().manager_id.as_deref() == Some(employee_id.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Employee still manages other employees; reassign their reports first",
            ));
        }
        state
            .employees
            .remove(employee_id.as_str())
            .map(|_| json!({ "deleted": true, "id": employee_id }))
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))
    })();
    json::respond(result, debug)
}

/// Org-chart data: manager links for every employee, no salary data.
/// Any authenticated caller may read it; salary never appears here.
pub(crate) async fn get_org_chart(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<OrgChartNode>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _caller = caller(&req)?;
        let mut nodes: Vec<OrgChartNode> = state
            .employees
            .iter()
            .map(|entry| {
                let employee = entry.value();
                OrgChartNode {
                    employee_id: employee.id.clone(),
                    user_id: employee.user_id.clone(),
                    position_id: employee.position_id.clone(),
                    department_id: employee.department_id.clone(),
                    manager_id: employee.manager_id.clone(),
                    status: employee.status.clone(),
                    hire_date: employee.hire_date,
                }
            })
            .collect();
        nodes.sort_by(|a, b| a.employee_id.cmp(&b.employee_id));
        Ok(nodes)
    })();
    json::respond(result, debug)
}

/// Whether a status update from `from` to `to` is a termination
/// transition (live contracts and leave must be closed first).
/// Re-terminating an already terminated employee is not one.
pub(crate) fn is_termination_transition(from: &str, to: &str) -> bool {
    to == "terminated" && from != "terminated"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn termination_transition_detected_once() {
        assert!(is_termination_transition("active", "terminated"));
        assert!(is_termination_transition("on_leave", "terminated"));
        assert!(!is_termination_transition("terminated", "terminated"));
        assert!(!is_termination_transition("active", "on_leave"));
    }
}
