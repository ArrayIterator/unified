//! HTTP handlers: payroll runs and items.

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

// Handlers: payroll

pub(crate) async fn list_payroll_runs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let params = query.into_inner();
        let payroll_capable = can_payroll(&ctx, &caller.role);
        let read_all = check_capability(&ctx, &caller.role, PERM_HR_READ_ALL);
        let mut items: Vec<Value> = state
            .payroll_runs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|run| {
                params
                    .filters
                    .get("status")
                    .is_none_or(|status| run.status == *status)
                    && params
                        .filters
                        .get("period")
                        .is_none_or(|period| run.period == *period)
            })
            .map(|run| {
                if payroll_capable || read_all {
                    serde_json::to_value(&run).unwrap_or(Value::Null)
                } else {
                    // Employees without payroll access see that a run
                    // exists, but never its money totals. The skipped
                    // list carries no money, so it stays visible.
                    json!({
                        "id": run.id,
                        "period": run.period,
                        "currency": run.currency,
                        "status": run.status,
                        "total_employees": run.total_employees,
                        "skipped_employees": run.skipped_employees,
                        "finalized_at": run.finalized_at,
                        "created_at": run.created_at,
                        "updated_at": run.updated_at,
                    })
                }
            })
            .collect();
        items.sort_by(|a, b| {
            b.get("period")
                .and_then(|v| v.as_str())
                .cmp(&a.get("period").and_then(|v| v.as_str()))
        });
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

pub(crate) async fn create_payroll_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    body: web::Json<CreatePayrollRun>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PayrollRun> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        if !can_payroll(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(format!(
                "Role \"{}\" does not hold permission \"{}\"",
                caller.role, PERM_HR_PAYROLL
            )));
        }
        let input = body.into_inner();
        let period = valid_period(&input.period)?;
        let currency = match input.currency {
            Some(raw) => valid_currency(&raw)?,
            None => "USD".to_string(),
        };
        if state
            .payroll_runs
            .iter()
            .any(|entry| entry.value().period == period)
        {
            return Err(UnifiedError::conflict(format!(
                "A payroll run already exists for period {period}"
            )));
        }
        let run = PayrollRun {
            id: new_id(),
            period,
            currency,
            status: "draft".to_string(),
            total_gross: 0,
            total_net: 0,
            total_employees: 0,
            skipped_employees: Vec::new(),
            finalized_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.payroll_runs.insert(run.id.clone(), run.clone());
        Ok(run)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_payroll_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let run = state
            .payroll_runs
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Payroll run not found"))?;
        if can_payroll(&ctx, &caller.role)
            || check_capability(&ctx, &caller.role, PERM_HR_READ_ALL)
        {
            Ok(serde_json::to_value(&run).unwrap_or(Value::Null))
        } else {
            Ok(json!({
                "id": run.id,
                "period": run.period,
                "currency": run.currency,
                "status": run.status,
                "total_employees": run.total_employees,
                "skipped_employees": run.skipped_employees,
                "finalized_at": run.finalized_at,
                "created_at": run.created_at,
                "updated_at": run.updated_at,
            }))
        }
    })();
    json::respond(result, debug)
}

/// Recompute one run from active contracts: gross is the contract
/// salary, net is gross plus adjustments minus deductions. Existing
/// adjustments and deductions are kept, so HR can tune a line and
/// re-run the computation without losing its edits.
pub(crate) async fn compute_payroll_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PayrollRun> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        if !can_payroll(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(format!(
                "Role \"{}\" does not hold permission \"{}\"",
                caller.role, PERM_HR_PAYROLL
            )));
        }
        let run_id = path.as_str().to_string();
        let mut run = state
            .payroll_runs
            .get_mut(run_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Payroll run not found"))?;
        if !matches!(run.status.as_str(), "draft" | "computed") {
            return Err(UnifiedError::conflict(format!(
                "Only a draft or computed payroll run can be computed (current status: {})",
                run.status
            )));
        }
        let today = today();
        let currency = run.currency.clone();
        drop(run);
        let mut total_gross = 0i64;
        let mut total_net = 0i64;
        let mut employee_count = 0u32;
        let mut skipped: Vec<SkippedPayrollEmployee> = Vec::new();
        let mut included: Vec<String> = Vec::new();
        let employees: Vec<Employee> = state
            .employees
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|employee| employee.status == "active")
            .collect();
        for employee in employees {
            let Some(contract) = state.active_contract(&employee.id, today) else {
                // No active contract: the employee is left out, and
                // any item from an earlier compute is removed below
                // so it cannot disagree with the totals. They are
                // listed as skipped either way - not only when a
                // stale item happened to exist - so the skipped list
                // accounts for every employee left out of the run.
                skipped.push(no_active_contract_skip(&employee.id));
                continue;
            };
            // Payroll is computed in the run currency; a contract in
            // another currency is skipped and listed in the run's
            // skipped list instead of being silently converted at a
            // made-up rate.
            if let Some(skip) =
                foreign_currency_skip(&employee.id, &contract.currency, &currency)
            {
                skipped.push(skip);
                continue;
            }
            included.push(employee.id.clone());
            employee_count += 1;
            let gross = contract.salary_amount;
            let existing = state.payroll_items.iter().find(|entry| {
                let item = entry.value();
                item.payroll_run_id == run_id && item.employee_id == employee.id
            });
            let (adjustments, deductions, notes) = match existing.as_ref() {
                Some(entry) => (
                    entry.value().adjustments,
                    entry.value().deductions,
                    entry.value().notes.clone(),
                ),
                None => (0, 0, String::new()),
            };
            let net = (gross + adjustments - deductions).max(0);
            total_gross += gross;
            total_net += net;
            match existing {
                Some(entry) => {
                    let item_id = entry.key().clone();
                    drop(entry);
                    if let Some(mut item) = state.payroll_items.get_mut(item_id.as_str()) {
                        item.gross = gross;
                        item.net = net;
                        item.updated_at = now();
                    }
                }
                None => {
                    let item = PayrollItem {
                        id: new_id(),
                        payroll_run_id: run_id.clone(),
                        employee_id: employee.id.clone(),
                        gross,
                        adjustments,
                        deductions,
                        net,
                        notes,
                        created_at: now(),
                        updated_at: now(),
                    };
                    state.payroll_items.insert(item.id.clone(), item);
                }
            }
        }
        // Drop stale items for employees left out of this compute,
        // so the listed items always add up to the run totals. An
        // employee whose stale item was removed but who is not
        // already listed (no active contract) is recorded too.
        for employee_id in state.remove_stale_payroll_items(&run_id, &included) {
            if !skipped.iter().any(|skip| skip.employee_id == employee_id) {
                skipped.push(no_active_contract_skip(&employee_id));
            }
        }
        skipped.sort_by(|a, b| a.employee_id.cmp(&b.employee_id));
        let mut run = state
            .payroll_runs
            .get_mut(run_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Payroll run not found"))?;
        run.total_gross = total_gross;
        run.total_net = total_net;
        run.total_employees = employee_count;
        run.skipped_employees = skipped;
        run.status = "computed".to_string();
        run.updated_at = now();
        Ok(run.clone())
    })();
    json::respond(result, debug)
}

/// The skip entry for an employee with no active contract for the
/// run period. Used both when the compute loop finds no contract
/// and when a stale payroll item is removed for such an employee,
/// so the two paths always report the same reason.
pub(crate) fn no_active_contract_skip(employee_id: &str) -> SkippedPayrollEmployee {
    SkippedPayrollEmployee {
        employee_id: employee_id.to_string(),
        reason: "employee has no active contract for this period".to_string(),
    }
}

/// The skip entry for a contract whose currency differs from the
/// run currency, or `None` when the currencies match. A mismatched
/// contract is never converted at a made-up rate; it is reported
/// in the run's skipped list instead.
pub(crate) fn foreign_currency_skip(
    employee_id: &str,
    contract_currency: &str,
    run_currency: &str,
) -> Option<SkippedPayrollEmployee> {
    if contract_currency == run_currency {
        None
    } else {
        Some(SkippedPayrollEmployee {
            employee_id: employee_id.to_string(),
            reason: format!(
                "contract currency {contract_currency} differs from run currency {run_currency}"
            ),
        })
    }
}

pub(crate) async fn finalize_payroll_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PayrollRun> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        if !can_payroll(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(format!(
                "Role \"{}\" does not hold permission \"{}\"",
                caller.role, PERM_HR_PAYROLL
            )));
        }
        let run_id = path.as_str().to_string();
        let mut run = state
            .payroll_runs
            .get_mut(run_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Payroll run not found"))?;
        if run.status != "computed" {
            return Err(UnifiedError::conflict(format!(
                "Only a computed payroll run can be finalized (current status: {})",
                run.status
            )));
        }
        if run.total_employees == 0 {
            return Err(UnifiedError::conflict(
                "Cannot finalize a payroll run with no employees; compute it first",
            ));
        }
        run.status = "finalized".to_string();
        run.finalized_at = Some(now());
        run.updated_at = now();
        let finalized = run.clone();
        drop(run);
        // Finalizing never pays anyone: the billing module listens for
        // this event and owns any payout.
        state.emit(
            "hr.payroll_run_completed",
            json!({
                "payroll_run_id": finalized.id,
                "period": finalized.period,
                "total_gross": minor_to_major(finalized.total_gross, &finalized.currency),
                "total_net": minor_to_major(finalized.total_net, &finalized.currency),
                "employee_count": finalized.total_employees,
                "currency": finalized.currency,
            }),
        );
        Ok(finalized)
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_payroll_items(
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
        let run_id = path.as_str().to_string();
        if !state.payroll_runs.contains_key(run_id.as_str()) {
            return Err(UnifiedError::not_found("Payroll run not found"));
        }
        let params = query.into_inner();
        let own_employee_id = state
            .employee_by_user(&caller.actor)
            .map(|employee| employee.id);
        let payroll_capable = can_payroll(&ctx, &caller.role);
        let read_all = check_capability(&ctx, &caller.role, PERM_HR_READ_ALL);
        let mut items: Vec<Value> = state
            .payroll_items
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                item.payroll_run_id == run_id
                    && (payroll_capable
                        || read_all
                        || Some(item.employee_id.as_str()) == own_employee_id.as_deref())
            })
            .map(|item| {
                let employee_user_id = state
                    .employees
                    .get(item.employee_id.as_str())
                    .map(|entry| entry.value().user_id.clone())
                    .unwrap_or_default();
                payroll_item_view(&ctx, &caller, &item, &employee_user_id)
            })
            .collect();
        items.sort_by(|a, b| {
            a.get("employee_id")
                .and_then(|v| v.as_str())
                .cmp(&b.get("employee_id").and_then(|v| v.as_str()))
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

pub(crate) async fn update_payroll_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePayrollItem>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PayrollItem> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        if !can_payroll(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(format!(
                "Role \"{}\" does not hold permission \"{}\"",
                caller.role, PERM_HR_PAYROLL
            )));
        }
        let input = body.into_inner();
        let adjustments = match input.adjustments {
            Some(value) => Some(parse_money(value, "adjustments")?),
            None => None,
        };
        // Deductions are a positive amount subtracted from gross.
        let deductions = match input.deductions {
            Some(value) => Some(parse_money(value, "deductions")?),
            None => None,
        };
        let notes = match input.notes {
            Some(note) => Some(require_text(&note, "notes", MAX_TEXT)?),
            None => None,
        };
        let mut item = state
            .payroll_items
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Payroll item not found"))?;
        let run_status = state
            .payroll_runs
            .get(item.payroll_run_id.as_str())
            .map(|entry| entry.value().status.clone())
            .ok_or_else(|| UnifiedError::not_found("Payroll run not found"))?;
        if run_status == "finalized" {
            return Err(UnifiedError::conflict(
                "A finalized payroll run can no longer be changed",
            ));
        }
        if let Some(adjustments) = adjustments {
            item.adjustments = adjustments;
        }
        if let Some(deductions) = deductions {
            item.deductions = deductions;
        }
        if let Some(notes) = notes {
            item.notes = notes;
        }
        item.net = (item.gross + item.adjustments - item.deductions).max(0);
        item.updated_at = now();
        let updated = item.clone();
        let run_id = updated.payroll_run_id.clone();
        drop(item);
        // Keep the run totals in step with the edited line.
        let mut total_gross = 0i64;
        let mut total_net = 0i64;
        for entry in state.payroll_items.iter() {
            if entry.value().payroll_run_id == run_id {
                total_gross += entry.value().gross;
                total_net += entry.value().net;
            }
        }
        if let Some(mut run) = state.payroll_runs.get_mut(run_id.as_str()) {
            run.total_gross = total_gross;
            run.total_net = total_net;
            run.updated_at = now();
        }
        Ok(updated)
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_active_contract_skip_names_employee_and_reason() {
        let skip = no_active_contract_skip("emp-1");
        assert_eq!(skip.employee_id, "emp-1");
        assert_eq!(
            skip.reason,
            "employee has no active contract for this period"
        );
    }

    #[test]
    fn foreign_currency_skip_reports_mismatch_only() {
        assert!(foreign_currency_skip("emp-1", "USD", "USD").is_none());
        let skip = foreign_currency_skip("emp-1", "IDR", "USD").unwrap();
        assert_eq!(skip.employee_id, "emp-1");
        assert!(skip.reason.contains("IDR"));
        assert!(skip.reason.contains("USD"));
    }

    #[test]
    fn payroll_run_skipped_list_defaults_to_empty() {
        // Runs stored before the skipped list existed must still
        // deserialize, with no employees skipped.
        let run: PayrollRun = serde_json::from_value(json!({
            "id": "run-1",
            "period": "2026-11",
            "currency": "USD",
            "status": "draft",
            "total_gross": 0.0,
            "total_net": 0.0,
            "total_employees": 0,
            "finalized_at": null,
            "created_at": "2026-11-01T00:00:00Z",
            "updated_at": "2026-11-01T00:00:00Z",
        }))
        .unwrap();
        assert!(run.skipped_employees.is_empty());
    }
}
