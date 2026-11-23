// Handlers: categories.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// --- Handlers: categories ---------------------------------------------------

pub(crate) async fn list_categories(
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
        let mut items: Vec<ExpenseCategory> = state
            .categories
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
                    let haystack = format!("{} {}", row.name, row.code).to_lowercase();
                    if !haystack.contains(needle) {
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

pub(crate) async fn create_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    body: web::Json<CreateCategoryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpenseCategory> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let code = require_text(&input.code, "code", MAX_CODE_LEN)?;
        // Tenant scope: the company is stamped from the resolved
        // tenant context, never from the caller's free text.
        let scope = tenant_scope(&req, input.company_id.clone())?;
        let company_id = Some(scope.company_id);
        let receipt_required_above_minor =
            optional_amount(input.receipt_required_above_minor, "receipt_required_above_minor")?;
        let per_expense_limit_minor =
            optional_amount(input.per_expense_limit_minor, "per_expense_limit_minor")?;
        let per_day_limit_minor =
            optional_amount(input.per_day_limit_minor, "per_day_limit_minor")?;
        let now = Utc::now();
        let category = ExpenseCategory {
            id: Uuid::now_v7(),
            name,
            code,
            company_id,
            receipt_required: input.receipt_required.unwrap_or(false),
            receipt_required_above_minor,
            per_expense_limit_minor,
            per_day_limit_minor,
            active: true,
            created_at: now,
            updated_at: now,
        };
        state.categories.insert(category.id, category.clone());
        state.emit(
            "expenses.category.created",
            json!({ "category_id": category.id.to_string(), "name": category.name }),
        );
        Ok(category)
    })();
    match result {
        Ok(category) => created(category),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpenseCategory> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let category = load_category(&state, &path.into_inner())?;
        require_company_scope(&category.company_id, &tenant_scope(&req, None)?)?;
        Ok(category)
    })();
    match result {
        Ok(category) => ok(category),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCategoryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpenseCategory> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let input = body.into_inner();
        let mut category = load_category(&state, &path.into_inner())?;
        require_company_scope(&category.company_id, &tenant_scope(&req, None)?)?;
        if let Some(raw) = &input.name {
            category.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(raw) = &input.code {
            category.code = require_text(raw, "code", MAX_CODE_LEN)?;
        }
        if let Some(value) = input.receipt_required {
            category.receipt_required = value;
        }
        if input.receipt_required_above_minor.is_some() {
            category.receipt_required_above_minor =
                optional_amount(input.receipt_required_above_minor, "receipt_required_above_minor")?;
        }
        if input.per_expense_limit_minor.is_some() {
            category.per_expense_limit_minor =
                optional_amount(input.per_expense_limit_minor, "per_expense_limit_minor")?;
        }
        if input.per_day_limit_minor.is_some() {
            category.per_day_limit_minor =
                optional_amount(input.per_day_limit_minor, "per_day_limit_minor")?;
        }
        if let Some(value) = input.active {
            category.active = value;
        }
        category.updated_at = Utc::now();
        state.categories.insert(category.id, category.clone());
        state.emit(
            "expenses.category.updated",
            json!({ "category_id": category.id.to_string(), "name": category.name }),
        );
        Ok(category)
    })();
    match result {
        Ok(category) => ok(category),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let category = load_category(&state, &path.into_inner())?;
        require_company_scope(&category.company_id, &tenant_scope(&req, None)?)?;
        let in_use = state
            .expenses
            .iter()
            .any(|entry| entry.value().category_id == category.id);
        if in_use {
            return Err(UnifiedError::conflict(
                "Category is used by expenses and cannot be deleted; deactivate it instead",
            ));
        }
        state.categories.remove(&category.id);
        state.emit(
            "expenses.category.deleted",
            json!({ "category_id": category.id.to_string() }),
        );
        Ok(json!({ "deleted": true, "category_id": category.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
