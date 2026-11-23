// Handlers: policies.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// --- Handlers: policies -----------------------------------------------------

pub(crate) async fn list_policies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let active_filter = params.filters.get("active").cloned();
        let search = params.search.clone().map(|value| value.to_lowercase());
        let scope = tenant_scope(&req, company_filter.clone())?;
        let mut items: Vec<ExpensePolicy> = state
            .policies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.company_id.as_deref() != Some(scope.company_id.as_str()) {
                    return false;
                }
                if let Some(company) = &company_filter {
                    if row.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                if let Some(active) = &active_filter {
                    let wanted = active == "true";
                    if row.active != wanted {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    if !row.name.to_lowercase().contains(needle) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    body: web::Json<CreatePolicyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpensePolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        // Tenant scope: the company is stamped from the resolved
        // tenant context, never from the caller's free text.
        let scope = tenant_scope(&req, input.company_id.clone())?;
        let company_id = Some(scope.company_id);
        let currency = match &input.currency {
            Some(raw) => require_in_list(raw, "currency", &settings.supported_currencies)
                .and_then(|value| require_currency(&value))?,
            None => settings.default_currency.clone(),
        };
        let mileage_rate_minor = optional_amount(input.mileage_rate_minor, "mileage_rate_minor")?;
        let mileage_unit = match &input.mileage_unit {
            Some(raw) => require_text(raw, "mileage_unit", 16)?,
            None => settings.default_mileage_unit.clone(),
        };
        let per_diem_rate_minor =
            optional_amount(input.per_diem_rate_minor, "per_diem_rate_minor")?;
        let max_report_total_minor =
            optional_amount(input.max_report_total_minor, "max_report_total_minor")?;
        let approval_chain_id =
            optional_text(&input.approval_chain_id, "approval_chain_id", MAX_SLUG_LEN)?;
        let now = Utc::now();
        let policy = ExpensePolicy {
            id: Uuid::now_v7(),
            name,
            company_id,
            currency,
            mileage_rate_minor,
            mileage_unit,
            per_diem_rate_minor,
            max_report_total_minor,
            approval_chain_id,
            active: true,
            created_at: now,
            updated_at: now,
        };
        state.policies.insert(policy.id, policy.clone());
        state.emit(
            "expenses.policy.created",
            json!({ "policy_id": policy.id.to_string(), "name": policy.name }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => created(policy),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpensePolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let policy = load_policy(&state, &path.into_inner())?;
        require_company_scope(&policy.company_id, &tenant_scope(&req, None)?)?;
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePolicyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpensePolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings();
        let mut policy = load_policy(&state, &path.into_inner())?;
        require_company_scope(&policy.company_id, &tenant_scope(&req, None)?)?;
        if let Some(raw) = &input.name {
            policy.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(raw) = &input.currency {
            policy.currency =
                require_in_list(raw, "currency", &settings.supported_currencies)
                    .and_then(|value| require_currency(&value))?;
        }
        if input.mileage_rate_minor.is_some() {
            policy.mileage_rate_minor =
                optional_amount(input.mileage_rate_minor, "mileage_rate_minor")?;
        }
        if let Some(raw) = &input.mileage_unit {
            policy.mileage_unit = require_text(raw, "mileage_unit", 16)?;
        }
        if input.per_diem_rate_minor.is_some() {
            policy.per_diem_rate_minor =
                optional_amount(input.per_diem_rate_minor, "per_diem_rate_minor")?;
        }
        if input.max_report_total_minor.is_some() {
            policy.max_report_total_minor =
                optional_amount(input.max_report_total_minor, "max_report_total_minor")?;
        }
        if input.approval_chain_id.is_some() {
            policy.approval_chain_id =
                optional_text(&input.approval_chain_id, "approval_chain_id", MAX_SLUG_LEN)?;
        }
        if let Some(value) = input.active {
            policy.active = value;
        }
        policy.updated_at = Utc::now();
        state.policies.insert(policy.id, policy.clone());
        state.emit(
            "expenses.policy.updated",
            json!({ "policy_id": policy.id.to_string(), "name": policy.name }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let policy = load_policy(&state, &path.into_inner())?;
        require_company_scope(&policy.company_id, &tenant_scope(&req, None)?)?;
        let in_use = state
            .reports
            .iter()
            .any(|entry| entry.value().policy_id == Some(policy.id));
        if in_use {
            return Err(UnifiedError::conflict(
                "Policy is used by reports and cannot be deleted; deactivate it instead",
            ));
        }
        state.policies.remove(&policy.id);
        state.emit(
            "expenses.policy.deleted",
            json!({ "policy_id": policy.id.to_string() }),
        );
        Ok(json!({ "deleted": true, "policy_id": policy.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
