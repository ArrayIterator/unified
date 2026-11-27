// Handlers: expenses.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// Handlers: expenses

pub(crate) async fn list_report_expenses(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let report = load_report(&state, &path.into_inner())?;
        require_report_scope(&report, &tenant_scope(&req, None)?)?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let category_filter = params.filters.get("category_id").cloned();
        let search = params.search.clone().map(|value| value.to_lowercase());
        let mut items: Vec<Expense> = report_expenses(&state, report.id)
            .into_iter()
            .filter(|row| {
                if let Some(kind) = &kind_filter {
                    if &row.kind != kind {
                        return false;
                    }
                }
                if let Some(category) = &category_filter {
                    if row.category_id.to_string() != *category {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    if !row.description.to_lowercase().contains(needle) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| (a.expense_date, a.created_at).cmp(&(b.expense_date, b.created_at)));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_expense(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateExpenseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Expense> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_SUBMIT)?;
        let scope = tenant_scope(&req, None)?;
        let report = load_report(&state, &path.into_inner())?;
        require_report_scope(&report, &scope)?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        if report.status != REPORT_DRAFT {
            return Err(UnifiedError::conflict(format!(
                "Expenses can only be added to a draft report; this report is \"{}\"",
                report.status
            ))
            .with_metadata("allowed_transitions", json!(allowed_report_transitions(&report.status))));
        }
        let input = body.into_inner();
        let settings = state.settings();
        let kind = require_in_list(&input.kind, "kind", &settings.expense_kinds)?;
        let category = load_category(&state, &input.category_id)?;
        require_company_scope(&category.company_id, &scope)?;
        if !category.active {
            return Err(UnifiedError::bad_request("Category is not active"));
        }
        let expense_date = parse_date(&input.expense_date, "expense_date")?;
        let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let project_id = optional_text(&input.project_id, "project_id", MAX_SLUG_LEN)?;
        let client_id = optional_text(&input.client_id, "client_id", MAX_SLUG_LEN)?;
        let currency = match &input.currency {
            Some(raw) => {
                let code = require_in_list(raw, "currency", &settings.supported_currencies)
                    .and_then(|value| require_currency(&value))?;
                if code != report.currency {
                    // The line is stored in the report currency;
                    // convert with a stored rate only.
                    convert_amount(&state, &ctx.hooks, 0, &code, &report.currency)?;
                }
                code
            }
            None => report.currency.clone(),
        };
        // Mileage and per-diem amounts are computed server-side
        // from policy rates; a client-supplied amount is ignored
        // for those kinds so the rate can never be overridden.
        let (amount_minor, distance, days, rate_minor_applied) = match kind.as_str() {
            "mileage" => {
                let distance = input.distance.ok_or_else(|| {
                    UnifiedError::bad_request("Mileage expenses require a distance")
                })?;
                if !(distance > 0.0) || distance > MAX_MILEAGE_DISTANCE || !distance.is_finite() {
                    return Err(UnifiedError::bad_request(format!(
                        "distance must be between 0 and {MAX_MILEAGE_DISTANCE}"
                    )));
                }
                let rate = mileage_rate_for(&state, &report);
                let amount = (distance * rate as f64).round() as i64;
                let amount = require_amount(amount, "amount_minor")?;
                (amount, Some(distance), None, Some(rate))
            }
            "per_diem" => {
                let days = input.days.ok_or_else(|| {
                    UnifiedError::bad_request("Per-diem expenses require days")
                })?;
                if days < 1 || days > MAX_PER_DIEM_DAYS {
                    return Err(UnifiedError::bad_request(format!(
                        "days must be between 1 and {MAX_PER_DIEM_DAYS}"
                    )));
                }
                let rate = per_diem_rate_for(&state, &report);
                let amount = require_amount(days * rate, "amount_minor")?;
                (amount, None, Some(days), Some(rate))
            }
            _ => {
                let amount = require_amount(
                    input.amount_minor.ok_or_else(|| {
                        UnifiedError::bad_request("Standard expenses require amount_minor")
                    })?,
                    "amount_minor",
                )?;
                (amount, None, None, None)
            }
        };
        // Convert a foreign-currency standard line into the report
        // currency with a stored rate only; mileage and per-diem
        // lines are already priced in the report currency.
        let amount_minor = if currency != report.currency && rate_minor_applied.is_none() {
            convert_amount(&state, &ctx.hooks, amount_minor, &currency, &report.currency)?
        } else {
            amount_minor
        };
        let now = Utc::now();
        let mut expense = Expense {
            id: Uuid::now_v7(),
            report_id: report.id,
            kind,
            category_id: category.id,
            expense_date,
            amount_minor,
            currency: report.currency.clone(),
            description,
            project_id,
            client_id,
            distance,
            days,
            rate_minor_applied,
            violations: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        expense.violations = compute_violations(&state, &report, &expense);
        state.expenses.insert(expense.id, expense.clone());
        refresh_report_total(&state, report.id);
        refresh_report_violations(&state, report.id);
        state.emit(
            "expenses.expense.created",
            json!({
                "expense_id": expense.id.to_string(),
                "report_id": report.id.to_string(),
                "kind": expense.kind,
                "amount_minor": expense.amount_minor,
                "currency": expense.currency,
                "violations": expense.violations,
            }),
        );
        Ok(expense)
    })();
    match result {
        Ok(expense) => created(expense),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_expense(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let expense = load_expense(&state, &path.into_inner())?;
        let report = load_report(&state, &expense.report_id.to_string())?;
        require_report_scope(&report, &tenant_scope(&req, None)?)?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        let receipts = expense_receipts(&state, expense.id);
        Ok(json!({ "expense": expense, "receipts": receipts }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_expense(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateExpenseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Expense> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_SUBMIT)?;
        let input = body.into_inner();
        let scope = tenant_scope(&req, None)?;
        let mut expense = load_expense(&state, &path.into_inner())?;
        let report = load_report(&state, &expense.report_id.to_string())?;
        require_report_scope(&report, &scope)?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        if report.status != REPORT_DRAFT {
            return Err(UnifiedError::conflict(format!(
                "Expenses can only be edited on a draft report; this report is \"{}\"",
                report.status
            ))
            .with_metadata("allowed_transitions", json!(allowed_report_transitions(&report.status))));
        }
        if let Some(raw) = &input.category_id {
            let category = load_category(&state, raw)?;
            if !category.active {
                return Err(UnifiedError::bad_request("Category is not active"));
            }
            require_company_scope(&category.company_id, &scope)?;
            expense.category_id = category.id;
        }
        if let Some(raw) = &input.expense_date {
            expense.expense_date = parse_date(raw, "expense_date")?;
        }
        if let Some(raw) = &input.description {
            expense.description = require_text(raw, "description", MAX_DESCRIPTION_LEN)?;
        }
        if input.project_id.is_some() {
            expense.project_id = optional_text(&input.project_id, "project_id", MAX_SLUG_LEN)?;
        }
        if input.client_id.is_some() {
            expense.client_id = optional_text(&input.client_id, "client_id", MAX_SLUG_LEN)?;
        }
        // Recompute computed kinds from the policy rate whenever
        // the distance or days change; a standard line keeps its
        // stored amount unless a new amount is supplied.
        match expense.kind.as_str() {
            "mileage" => {
                if let Some(distance) = input.distance {
                    if !(distance > 0.0) || distance > MAX_MILEAGE_DISTANCE || !distance.is_finite() {
                        return Err(UnifiedError::bad_request(format!(
                            "distance must be between 0 and {MAX_MILEAGE_DISTANCE}"
                        )));
                    }
                    let rate = mileage_rate_for(&state, &report);
                    expense.distance = Some(distance);
                    expense.rate_minor_applied = Some(rate);
                    expense.amount_minor =
                        require_amount((distance * rate as f64).round() as i64, "amount_minor")?;
                }
            }
            "per_diem" => {
                if let Some(days) = input.days {
                    if days < 1 || days > MAX_PER_DIEM_DAYS {
                        return Err(UnifiedError::bad_request(format!(
                            "days must be between 1 and {MAX_PER_DIEM_DAYS}"
                        )));
                    }
                    let rate = per_diem_rate_for(&state, &report);
                    expense.days = Some(days);
                    expense.rate_minor_applied = Some(rate);
                    expense.amount_minor = require_amount(days * rate, "amount_minor")?;
                }
            }
            _ => {
                if let Some(amount) = input.amount_minor {
                    expense.amount_minor = require_amount(amount, "amount_minor")?;
                }
            }
        }
        expense.violations = compute_violations(&state, &report, &expense);
        expense.updated_at = Utc::now();
        state.expenses.insert(expense.id, expense.clone());
        refresh_report_total(&state, report.id);
        refresh_report_violations(&state, report.id);
        state.emit(
            "expenses.expense.updated",
            json!({
                "expense_id": expense.id.to_string(),
                "report_id": report.id.to_string(),
                "amount_minor": expense.amount_minor,
                "violations": expense.violations,
            }),
        );
        Ok(expense)
    })();
    match result {
        Ok(expense) => ok(expense),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_expense(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_SUBMIT)?;
        let expense = load_expense(&state, &path.into_inner())?;
        let report = load_report(&state, &expense.report_id.to_string())?;
        require_report_scope(&report, &tenant_scope(&req, None)?)?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        if report.status != REPORT_DRAFT {
            return Err(UnifiedError::conflict(format!(
                "Expenses can only be deleted from a draft report; this report is \"{}\"",
                report.status
            ))
            .with_metadata("allowed_transitions", json!(allowed_report_transitions(&report.status))));
        }
        state
            .receipts
            .iter()
            .filter(|entry| entry.value().expense_id == expense.id)
            .map(|entry| *entry.key())
            .collect::<Vec<Uuid>>()
            .into_iter()
            .for_each(|receipt_id| {
                state.receipts.remove(&receipt_id);
            });
        state.expenses.remove(&expense.id);
        refresh_report_total(&state, report.id);
        refresh_report_violations(&state, report.id);
        state.emit(
            "expenses.expense.deleted",
            json!({
                "expense_id": expense.id.to_string(),
                "report_id": report.id.to_string(),
            }),
        );
        Ok(json!({ "deleted": true, "expense_id": expense.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
