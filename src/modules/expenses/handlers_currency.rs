// Handlers: currency rates.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;

use super::*;

// Handlers: currency rates

pub(crate) async fn list_currency_rates(
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
        let base_filter = params.filters.get("base_currency").cloned();
        let target_filter = params.filters.get("target_currency").cloned();
        let mut items: Vec<CurrencyRate> = state
            .currency_rates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if let Some(base) = &base_filter {
                    if row.base_currency != *base {
                        return false;
                    }
                }
                if let Some(target) = &target_filter {
                    if row.target_currency != *target {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_currency_rate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    body: web::Json<CreateCurrencyRateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CurrencyRate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let input = body.into_inner();
        let base = require_currency(&input.base_currency)?;
        let target = require_currency(&input.target_currency)?;
        if base == target {
            return Err(UnifiedError::bad_request(
                "base_currency and target_currency must differ",
            ));
        }
        if !input.rate.is_finite() || input.rate <= 0.0 || input.rate > 1_000_000.0 {
            return Err(UnifiedError::bad_request(
                "rate must be a finite number greater than 0",
            ));
        }
        let source = match &input.source {
            Some(raw) => require_text(raw, "source", MAX_SLUG_LEN)?,
            None => "manual".to_string(),
        };
        // One pair is one row: storing a rate for a pair that
        // already has one updates it in place (see
        // `upsert_currency_rate`) instead of adding a duplicate
        // row that would make lookups nondeterministic.
        let (rate, created) = upsert_currency_rate(&state, &base, &target, input.rate, &source);
        state.emit(
            if created {
                "expenses.currency_rate.created"
            } else {
                "expenses.currency_rate.updated"
            },
            json!({
                "rate_id": rate.id.to_string(),
                "base_currency": rate.base_currency,
                "target_currency": rate.target_currency,
                "rate": rate.rate,
            }),
        );
        Ok(rate)
    })();
    match result {
        Ok(rate) => created(rate),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_currency_rate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CurrencyRate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        load_currency_rate(&state, &path.into_inner())
    })();
    match result {
        Ok(rate) => ok(rate),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_currency_rate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let rate = load_currency_rate(&state, &path.into_inner())?;
        state.currency_rates.remove(&rate.id);
        state.emit(
            "expenses.currency_rate.deleted",
            json!({ "rate_id": rate.id.to_string() }),
        );
        Ok(json!({ "deleted": true, "rate_id": rate.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn convert_currency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    body: web::Json<ConvertRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let input = body.into_inner();
        let amount = require_amount(input.amount_minor, "amount_minor")?;
        let from = require_currency(&input.from_currency)?;
        let to = require_currency(&input.to_currency)?;
        let rate = resolve_rate(&state, &ctx.hooks, &from, &to).ok_or_else(|| {
            UnifiedError::unprocessable(format!(
                "No stored exchange rate for {from} to {to}; conversion refused rather than guessing a rate"
            ))
        })?;
        let converted = convert_amount(&state, &ctx.hooks, amount, &from, &to)?;
        Ok(json!({
            "amount_minor": amount,
            "from_currency": from,
            "to_currency": to,
            "converted_minor": converted,
            "rate": rate,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
