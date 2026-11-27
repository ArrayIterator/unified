// procurement -- Receipt, budget and spend handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::NaiveDate;
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

use super::helpers::*;
use super::state::*;
use super::types::*;

// Receipt, budget, and spend handlers

pub(crate) async fn list_receipts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let params = query.into_inner();
        let po_filter = params.filters.get("purchase_order_id").cloned();
        let vendor_filter = params.filters.get("vendor_id").cloned();
        let vendor_pos: Option<Vec<Uuid>> = match &vendor_filter {
            Some(raw) => {
                let vendor_id = parse_uuid(raw, "vendor")?;
                Some(
                    state
                        .purchase_orders
                        .iter()
                        .map(|entry| entry.value().clone())
                        .filter(|po| po.vendor_id == vendor_id)
                        .map(|po| po.id)
                        .collect(),
                )
            }
            None => None,
        };
        let items: Vec<Value> = state
            .receipts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|receipt| {
                if let Some(po_id) = &po_filter {
                    if receipt.purchase_order_id.to_string() != *po_id {
                        return false;
                    }
                }
                if let Some(po_ids) = &vendor_pos {
                    if !po_ids.contains(&receipt.purchase_order_id) {
                        return false;
                    }
                }
                true
            })
            .map(|receipt| json!(receipt))
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_budgets(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let params = query.into_inner();
        let category_filter = params.filters.get("category").cloned();
        let vendor_filter = params.filters.get("vendor_id").cloned();
        let over_filter = params.filters.get("over_budget").cloned();
        let mut items: Vec<Value> = state
            .budgets
            .iter()
            .map(|entry| entry.value().clone())
            .map(|budget| budget_view(&state, &budget))
            .filter(|view| {
                if let Some(category) = &category_filter {
                    if view.get("category").and_then(Value::as_str) != Some(category.as_str()) {
                        return false;
                    }
                }
                if let Some(vendor_id) = &vendor_filter {
                    if view.get("vendor_id").and_then(Value::as_str) != Some(vendor_id.as_str()) {
                        return false;
                    }
                }
                if let Some(over) = &over_filter {
                    let want = over == "true";
                    if view
                        .get("over_budget")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                        != want
                    {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| {
            a.get("period_start")
                .and_then(Value::as_str)
                .cmp(&b.get("period_start").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) fn validate_budget_scope(
    category: &Option<String>,
    vendor_id: &Option<Uuid>,
    period_start: NaiveDate,
    period_end: NaiveDate,
    budget_amount: i64,
) -> UnifiedResult<(Option<String>, Option<Uuid>, i64)> {
    if category.is_none() && vendor_id.is_none() {
        return Err(UnifiedError::bad_request(
            "A budget must scope to a category, a vendor, or both",
        ));
    }
    let category = match category {
        Some(raw) => Some(require_text(raw, "category", MAX_CATEGORY_LEN)?),
        None => None,
    };
    if period_end < period_start {
        return Err(UnifiedError::bad_request(
            "period_end must be on or after period_start",
        ));
    }
    // `budget_amount` arrives as integer minor units (the request
    // JSON carries major units, converted by `serde_major`); only
    // the non-negative check is left here.
    let amount = require_non_negative_minor(budget_amount, "budget_amount")?;
    Ok((category, *vendor_id, amount))
}

pub(crate) async fn create_budget(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    body: web::Json<CreateBudgetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let input = body.into_inner();
        if let Some(vendor_id) = input.vendor_id {
            if !state.vendors.contains_key(&vendor_id) {
                return Err(UnifiedError::not_found(format!(
                    "Vendor not found: {vendor_id}"
                )));
            }
        }
        let (category, vendor_id, amount) = validate_budget_scope(
            &input.category,
            &input.vendor_id,
            input.period_start,
            input.period_end,
            input.budget_amount,
        )?;
        let currency = valid_currency(input.currency.as_deref().unwrap_or("USD"))?;
        let now = Utc::now();
        let budget = ProcurementBudget {
            id: Uuid::now_v7(),
            category,
            vendor_id,
            period_start: input.period_start,
            period_end: input.period_end,
            budget_amount: amount,
            currency,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.budgets.insert(budget.id, budget.clone());
        state.emit(
            "procurement.budget.created",
            json!({
                "budget_id": budget.id.to_string(),
                "category": budget.category,
                "vendor_id": budget.vendor_id.map(|id| id.to_string()),
                "budget_amount": minor_to_major(budget.budget_amount, &budget.currency),
                "created_by": caller.actor,
            }),
        );
        Ok(budget_view(&state, &budget))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_budget(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let budget = load_budget(&state, &path.into_inner())?;
        Ok(budget_view(&state, &budget))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_budget(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateBudgetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut budget = load_budget(&state, &path.into_inner())?;
        let input = body.into_inner();
        let new_category = match &input.category {
            Some(value) => match value {
                Some(raw) => Some(require_text(raw, "category", MAX_CATEGORY_LEN)?),
                None => None,
            },
            None => budget.category.clone(),
        };
        let new_vendor_id = match input.vendor_id {
            Some(value) => value,
            None => budget.vendor_id,
        };
        if let Some(vendor_id) = new_vendor_id {
            if !state.vendors.contains_key(&vendor_id) {
                return Err(UnifiedError::not_found(format!(
                    "Vendor not found: {vendor_id}"
                )));
            }
        }
        let new_start = input.period_start.unwrap_or(budget.period_start);
        let new_end = input.period_end.unwrap_or(budget.period_end);
        let new_amount = match input.budget_amount {
            Some(amount) => require_non_negative_minor(amount, "budget_amount")?,
            None => budget.budget_amount,
        };
        let (category, vendor_id, amount) = validate_budget_scope(
            &new_category,
            &new_vendor_id,
            new_start,
            new_end,
            new_amount,
        )?;
        budget.category = category;
        budget.vendor_id = vendor_id;
        budget.period_start = new_start;
        budget.period_end = new_end;
        budget.budget_amount = amount;
        if let Some(currency) = &input.currency {
            budget.currency = valid_currency(currency)?;
        }
        budget.updated_at = Utc::now();
        state.budgets.insert(budget.id, budget.clone());
        state.emit(
            "procurement.budget.updated",
            json!({
                "budget_id": budget.id.to_string(),
                "budget_amount": minor_to_major(budget.budget_amount, &budget.currency),
                "updated_by": caller.actor,
            }),
        );
        Ok(budget_view(&state, &budget))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_budget(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let budget = load_budget(&state, &path.into_inner())?;
        state.budgets.remove(&budget.id);
        state.emit(
            "procurement.budget.deleted",
            json!({
                "budget_id": budget.id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "budget_id": budget.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn spend_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let params = query.into_inner();
        let from = params
            .filters
            .get("from")
            .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok());
        let to = params
            .filters
            .get("to")
            .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok());
        if let (Some(from), Some(to)) = (from, to) {
            if to < from {
                return Err(UnifiedError::bad_request("to must be on or after from"));
            }
        }
        let category_filter = params.filters.get("category").cloned();
        let vendor_filter = params
            .filters
            .get("vendor_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        // Spend sums are integer minor units. The summary spans
        // currencies, as before, so the JSON carries major units
        // with the standard two-decimal fallback (same shape as
        // the POS module's currency-less totals).
        let total = committed_spend(&state, category_filter.as_deref(), vendor_filter, from, to);
        // Per-category breakdown.
        let mut by_category: Vec<Value> = Vec::new();
        let mut categories: Vec<String> = state
            .purchase_orders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|po| is_committed(po.status))
            .filter_map(|po| po.category.clone())
            .collect();
        categories.sort();
        categories.dedup();
        for category in categories {
            if let Some(filter) = &category_filter {
                if &category != filter {
                    continue;
                }
            }
            let spent = committed_spend(&state, Some(&category), vendor_filter, from, to);
            if spent > 0 {
                by_category.push(json!({ "category": category, "spent": minor_to_major(spent, "USD") }));
            }
        }
        // Per-vendor breakdown.
        let mut by_vendor: Vec<Value> = Vec::new();
        let mut vendor_ids: Vec<Uuid> = state
            .purchase_orders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|po| is_committed(po.status))
            .map(|po| po.vendor_id)
            .collect();
        vendor_ids.sort();
        vendor_ids.dedup();
        for vendor_id in vendor_ids {
            if let Some(filter) = vendor_filter {
                if vendor_id != filter {
                    continue;
                }
            }
            let spent = committed_spend(
                &state,
                category_filter.as_deref(),
                Some(vendor_id),
                from,
                to,
            );
            if spent > 0 {
                let name = state
                    .vendors
                    .get(&vendor_id)
                    .map(|entry| entry.value().name.clone());
                by_vendor.push(json!({
                    "vendor_id": vendor_id.to_string(),
                    "vendor_name": name,
                    "spent": minor_to_major(spent, "USD"),
                }));
            }
        }
        // Budget comparison for budgets overlapping the window.
        let budgets: Vec<Value> = state
            .budgets
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|budget| {
                if let Some(from) = from {
                    if budget.period_end < from {
                        return false;
                    }
                }
                if let Some(to) = to {
                    if budget.period_start > to {
                        return false;
                    }
                }
                if let Some(category) = &category_filter {
                    if budget.category.as_deref() != Some(category.as_str()) {
                        return false;
                    }
                }
                if let Some(vendor_id) = vendor_filter {
                    if budget.vendor_id != Some(vendor_id) {
                        return false;
                    }
                }
                true
            })
            .map(|budget| budget_view(&state, &budget))
            .collect();
        Ok(json!({
            "total_committed_spend": minor_to_major(total, "USD"),
            "from": from.map(|date| date.to_string()),
            "to": to.map(|date| date.to_string()),
            "by_category": by_category,
            "by_vendor": by_vendor,
            "budgets": budgets,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
