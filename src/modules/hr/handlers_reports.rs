//! HTTP handlers: HR reports.

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
use super::helpers::{minor_to_major};

// Handlers: HR reports

pub(crate) async fn report_headcount(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ_ALL)?;
        let employees: Vec<Employee> = state
            .employees
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        let mut by_department: HashMap<String, u64> = HashMap::new();
        let mut by_status: HashMap<String, u64> = HashMap::new();
        let mut by_position: HashMap<String, u64> = HashMap::new();
        for employee in &employees {
            *by_status.entry(employee.status.clone()).or_insert(0) += 1;
            *by_department
                .entry(
                    employee
                        .department_id
                        .clone()
                        .unwrap_or_else(|| "unassigned".to_string()),
                )
                .or_insert(0) += 1;
            *by_position
                .entry(
                    employee
                        .position_id
                        .clone()
                        .unwrap_or_else(|| "unassigned".to_string()),
                )
                .or_insert(0) += 1;
        }
        let active = by_status.get("active").copied().unwrap_or(0);
        Ok(json!({
            "total_employees": employees.len(),
            "active_employees": active,
            "by_department": by_department,
            "by_status": by_status,
            "by_position": by_position,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn report_leave_usage(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ_ALL)?;
        let params = query.into_inner();
        let year = params
            .filters
            .get("year")
            .and_then(|raw| raw.parse::<i32>().ok())
            .unwrap_or_else(|| today().format("%Y").to_string().parse::<i32>().unwrap_or(2000));
        let mut by_type: HashMap<String, Value> = HashMap::new();
        for entry in state.leave_types.iter() {
            let leave_type = entry.value();
            by_type.insert(
                leave_type.id.clone(),
                json!({
                    "leave_type_id": leave_type.id,
                    "name": leave_type.name,
                    "code": leave_type.code,
                    "approved_days": 0.0,
                    "pending_days": 0.0,
                    "rejected_days": 0.0,
                    "cancelled_days": 0.0,
                    "total_entitled_days": 0.0,
                    "total_used_days": 0.0,
                }),
            );
        }
        for entry in state.leave_requests.iter() {
            let request = entry.value();
            if request.start_date.format("%Y").to_string().parse::<i32>().unwrap_or(0) != year {
                continue;
            }
            if let Some(summary) = by_type.get_mut(&request.leave_type_id) {
                let key = match request.status.as_str() {
                    "approved" => "approved_days",
                    "pending" => "pending_days",
                    "rejected" => "rejected_days",
                    "cancelled" => "cancelled_days",
                    _ => continue,
                };
                let current = summary.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0);
                summary[key] = json!(round4(current + request.days));
            }
        }
        for entry in state.leave_balances.iter() {
            let balance = entry.value();
            if balance.year != year {
                continue;
            }
            if let Some(summary) = by_type.get_mut(&balance.leave_type_id) {
                let entitled = summary
                    .get("total_entitled_days")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                summary["total_entitled_days"] =
                    json!(round4(entitled + balance.entitled_days + balance.carried_over_days));
                let used = summary
                    .get("total_used_days")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                summary["total_used_days"] = json!(round4(used + balance.used_days));
            }
        }
        let mut by_type: Vec<Value> = by_type.into_values().collect();
        by_type.sort_by(|a, b| {
            a.get("name")
                .and_then(|v| v.as_str())
                .cmp(&b.get("name").and_then(|v| v.as_str()))
        });
        let pending_requests = state
            .leave_requests
            .iter()
            .filter(|entry| entry.value().status == "pending")
            .count() as u64;
        Ok(json!({
            "year": year,
            "pending_requests": pending_requests,
            "by_leave_type": by_type,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn report_payroll_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        if !can_payroll(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(format!(
                "Role \"{}\" does not hold permission \"{}\"",
                caller.role, PERM_HR_PAYROLL
            )));
        }
        let params = query.into_inner();
        let runs: Vec<PayrollRun> = state
            .payroll_runs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|run| {
                params
                    .filters
                    .get("period")
                    .is_none_or(|period| run.period == *period)
            })
            .collect();
        let mut by_status: HashMap<String, u64> = HashMap::new();
        let mut total_gross = 0i64;
        let mut total_net = 0i64;
        let mut total_employees = 0u32;
        for run in &runs {
            *by_status.entry(run.status.clone()).or_insert(0) += 1;
            total_gross += run.total_gross;
            total_net += run.total_net;
            total_employees += run.total_employees;
        }
        let mut by_period: Vec<Value> = runs
            .iter()
            .map(|run| {
                json!({
                    "period": run.period,
                    "status": run.status,
                    "currency": run.currency,
                    "total_gross": minor_to_major(run.total_gross, &run.currency),
                    "total_net": minor_to_major(run.total_net, &run.currency),
                    "total_employees": run.total_employees,
                })
            })
            .collect();
        by_period.sort_by(|a, b| {
            b.get("period")
                .and_then(|v| v.as_str())
                .cmp(&a.get("period").and_then(|v| v.as_str()))
        });
        Ok(json!({
            "total_runs": runs.len(),
            // Runs may use different currencies; the totals sum minor
            // units as before (major units were summed the same way)
            // and are emitted as major units on a 2-decimal basis.
            "total_gross": minor_to_major(total_gross, "USD"),
            "total_net": minor_to_major(total_net, "USD"),
            "total_employees": total_employees,
            "by_status": by_status,
            "by_period": by_period,
        }))
    })();
    json::respond(result, debug)
}
