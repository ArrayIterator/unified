//! HTTP handlers: contracts.

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

// Handlers: contracts

pub(crate) async fn list_contracts_for_employee(
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
        let mut items: Vec<Value> = state
            .contracts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|contract| {
                contract.employee_id == employee.id
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| contract.status == *status)
            })
            .map(|contract| contract_view(&ctx, &caller, &contract, &employee.user_id))
            .collect();
        items.sort_by(|a, b| {
            b.get("start_date")
                .and_then(|v| v.as_str())
                .cmp(&a.get("start_date").and_then(|v| v.as_str()))
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

pub(crate) async fn create_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateContract>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Contract> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let employee_id = path.as_str().to_string();
        if !state.employees.contains_key(employee_id.as_str()) {
            return Err(UnifiedError::not_found("Employee not found"));
        }
        let input = body.into_inner();
        if !matches!(
            input.contract_type.as_str(),
            "permanent" | "fixed_term" | "intern" | "part_time" | "contractor"
        ) {
            return Err(UnifiedError::bad_request(
                "contract_type must be one of: permanent, fixed_term, intern, part_time, contractor",
            ));
        }
        let start_date = parse_date(&input.start_date, "start_date")?;
        let end_date = match &input.end_date {
            Some(raw) if !raw.trim().is_empty() => Some(parse_date(raw, "end_date")?),
            _ => None,
        };
        if let Some(end) = end_date {
            if end < start_date {
                return Err(UnifiedError::bad_request(
                    "end_date must not be before start_date",
                ));
            }
        }
        if input.contract_type == "fixed_term" && end_date.is_none() {
            return Err(UnifiedError::bad_request(
                "fixed_term contracts require an end_date",
            ));
        }
        let salary_amount = parse_money(input.salary_amount, "salary_amount")?;
        let currency = match input.currency {
            Some(raw) => valid_currency(&raw)?,
            None => "USD".to_string(),
        };
        // One active contract at a time: creating a new contract
        // caps the previous active contract at the day before the
        // new start date (see `supersede_previous_contracts`).
        state.supersede_previous_contracts(&employee_id, start_date, today(), None);
        let contract = Contract {
            id: new_id(),
            employee_id,
            contract_type: input.contract_type,
            start_date,
            end_date,
            salary_amount,
            currency,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.contracts.insert(contract.id.clone(), contract.clone());
        Ok(contract)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_contract(
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
        let contract = state
            .contracts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Contract not found"))?;
        let employee = state
            .employees
            .get(contract.employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        require_read_employee(&ctx, &caller, &employee)?;
        Ok(contract_view(&ctx, &caller, &contract, &employee.user_id))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateContract>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Contract> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        if let Some(contract_type) = &input.contract_type {
            if !matches!(
                contract_type.as_str(),
                "permanent" | "fixed_term" | "intern" | "part_time" | "contractor"
            ) {
                return Err(UnifiedError::bad_request(
                    "contract_type must be one of: permanent, fixed_term, intern, part_time, contractor",
                ));
            }
        }
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "expired" | "terminated") {
                return Err(UnifiedError::bad_request(
                    "status must be one of: active, expired, terminated",
                ));
            }
        }
        let start_date = match &input.start_date {
            Some(raw) => Some(parse_date(raw, "start_date")?),
            None => None,
        };
        let end_date = match &input.end_date {
            Some(Some(raw)) if !raw.trim().is_empty() => Some(Some(parse_date(raw, "end_date")?)),
            Some(_) => Some(None),
            None => None,
        };
        let salary_amount = match input.salary_amount {
            Some(value) => Some(parse_money(value, "salary_amount")?),
            None => None,
        };
        let currency = match &input.currency {
            Some(raw) => Some(valid_currency(raw)?),
            None => None,
        };
        let current = state
            .contracts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Contract not found"))?;
        // Validate the new dates before writing anything.
        let new_type = input
            .contract_type
            .clone()
            .unwrap_or_else(|| current.contract_type.clone());
        let new_start = start_date.unwrap_or(current.start_date);
        let new_end = end_date.unwrap_or(current.end_date);
        if let Some(end) = new_end {
            if end < new_start {
                return Err(UnifiedError::bad_request(
                    "end_date must not be before start_date",
                ));
            }
        }
        if new_type == "fixed_term" && new_end.is_none() {
            return Err(UnifiedError::bad_request(
                "fixed_term contracts require an end_date",
            ));
        }
        let new_status = input.status.clone().unwrap_or_else(|| current.status.clone());
        if !contract_status_transition_allowed(&current.status, &new_status) {
            return Err(UnifiedError::conflict(format!(
                "A {} contract cannot become active; activate a contract only by creating a new one",
                current.status
            )));
        }
        // Activating this contract expires the employee's other
        // active contracts the same way creation does, so the
        // one-active-contract invariant holds after an update too.
        if new_status == "active" {
            state.supersede_previous_contracts(
                &current.employee_id,
                new_start,
                today(),
                Some(current.id.as_str()),
            );
        }
        let mut contract = state
            .contracts
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Contract not found"))?;
        contract.contract_type = new_type;
        contract.start_date = new_start;
        contract.end_date = new_end;
        if let Some(salary_amount) = salary_amount {
            contract.salary_amount = salary_amount;
        }
        if let Some(currency) = currency {
            contract.currency = currency;
        }
        contract.status = new_status;
        contract.updated_at = now();
        Ok(contract.clone())
    })();
    json::respond(result, debug)
}

/// Whether an update may move a contract from `from` to `to`.
/// A terminated or expired contract can never come back to active:
/// activation happens only by creating a new contract. Every other
/// move (including active to expired or terminated) is allowed.
pub(crate) fn contract_status_transition_allowed(from: &str, to: &str) -> bool {
    if to == "active" && matches!(from, "terminated" | "expired") {
        return false;
    }
    true
}

pub(crate) async fn terminate_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<Value>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Contract> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let end_date = body
            .get("end_date")
            .and_then(|v| v.as_str())
            .map(|raw| parse_date(raw, "end_date"))
            .transpose()?
            .unwrap_or_else(today);
        let mut contract = state
            .contracts
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Contract not found"))?;
        if contract.status == "terminated" {
            return Err(UnifiedError::conflict("Contract is already terminated"));
        }
        if end_date < contract.start_date {
            return Err(UnifiedError::bad_request(
                "end_date must not be before start_date",
            ));
        }
        contract.status = "terminated".to_string();
        contract.end_date = Some(end_date);
        contract.updated_at = now();
        Ok(contract.clone())
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminated_and_expired_contracts_cannot_become_active() {
        assert!(!contract_status_transition_allowed("terminated", "active"));
        assert!(!contract_status_transition_allowed("expired", "active"));
    }

    #[test]
    fn other_contract_transitions_are_allowed() {
        assert!(contract_status_transition_allowed("active", "active"));
        assert!(contract_status_transition_allowed("active", "expired"));
        assert!(contract_status_transition_allowed("active", "terminated"));
        assert!(contract_status_transition_allowed("expired", "terminated"));
        assert!(contract_status_transition_allowed("terminated", "terminated"));
        assert!(contract_status_transition_allowed("terminated", "expired"));
    }
}
