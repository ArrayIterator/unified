//! calculator - handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use super::types::*;
use super::state::*;
use super::validation::*;
use super::compute::*;

/// Run one calculation, record it in history, and return the result
/// with the exact inputs used. `company_id` is the verified caller's
/// server-resolved company, otherwise the caller-supplied fallback;
/// history always records a company id. The `X-Company-Id` header
/// is NEVER read here: a forged header would mislabel history rows
/// as another company's.
pub(crate) fn history_company_id(req: &HttpRequest, fallback: &str) -> String {
    unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

pub(crate) async fn calculate_roi(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<RoiInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = compute_roi(&input)?;
        let company_id = history_company_id(&req, "");
        state.record_history(&company_id, "roi", &caller.actor, inputs.clone(), results.clone());
        Ok(calculation_response("roi", inputs, results))
    })();
    json::respond(result, debug)
}

pub(crate) async fn calculate_break_even(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<BreakEvenInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = compute_break_even(&input)?;
        let company_id = history_company_id(&req, "");
        state.record_history(
            &company_id,
            "break_even",
            &caller.actor,
            inputs.clone(),
            results.clone(),
        );
        Ok(calculation_response("break_even", inputs, results))
    })();
    json::respond(result, debug)
}

pub(crate) async fn calculate_loan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<LoanInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = compute_loan(&input)?;
        let company_id = history_company_id(&req, "");
        state.record_history(&company_id, "loan", &caller.actor, inputs.clone(), results.clone());
        Ok(calculation_response("loan", inputs, results))
    })();
    json::respond(result, debug)
}

pub(crate) async fn calculate_depreciation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<DepreciationInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = compute_depreciation(&input)?;
        let company_id = history_company_id(&req, "");
        state.record_history(
            &company_id,
            "depreciation",
            &caller.actor,
            inputs.clone(),
            results.clone(),
        );
        Ok(calculation_response("depreciation", inputs, results))
    })();
    json::respond(result, debug)
}

pub(crate) async fn calculate_npv_irr(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<NpvIrrInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = compute_npv_irr(&input)?;
        let company_id = history_company_id(&req, "");
        state.record_history(
            &company_id,
            "npv_irr",
            &caller.actor,
            inputs.clone(),
            results.clone(),
        );
        Ok(calculation_response("npv_irr", inputs, results))
    })();
    json::respond(result, debug)
}

pub(crate) async fn calculate_cash_flow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<CashFlowInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = compute_cash_flow(&input)?;
        let company_id = history_company_id(&req, "");
        state.record_history(
            &company_id,
            "cash_flow",
            &caller.actor,
            inputs.clone(),
            results.clone(),
        );
        Ok(calculation_response("cash_flow", inputs, results))
    })();
    json::respond(result, debug)
}

pub(crate) async fn calculate_pricing(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<PricingInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = compute_pricing(&input)?;
        let company_id = history_company_id(&req, "");
        state.record_history(
            &company_id,
            "pricing",
            &caller.actor,
            inputs.clone(),
            results.clone(),
        );
        Ok(calculation_response("pricing", inputs, results))
    })();
    json::respond(result, debug)
}

pub(crate) async fn calculate_salary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<SalaryInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = compute_salary(&input)?;
        let company_id = history_company_id(&req, "");
        state.record_history(
            &company_id,
            "salary",
            &caller.actor,
            inputs.clone(),
            results.clone(),
        );
        Ok(calculation_response("salary", inputs, results))
    })();
    json::respond(result, debug)
}

pub(crate) async fn convert_currency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<CurrencyConvertInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let amount = require_positive(input.amount, "amount")?;
        let from = input.from_currency.trim().to_string();
        let to = input.to_currency.trim().to_string();
        if !valid_currency_code(&from) {
            return Err(UnifiedError::bad_request(
                "from_currency must be a 3-letter uppercase ISO 4217 code",
            ));
        }
        if !valid_currency_code(&to) {
            return Err(UnifiedError::bad_request(
                "to_currency must be a 3-letter uppercase ISO 4217 code",
            ));
        }
        if from == to {
            let results = json!({
                "amount": round_money(amount),
                "from_currency": from,
                "to_currency": to,
                "rate": 1.0,
                "converted_amount": round_money(amount),
                "provider": null,
                "stale": false,
                "rate_age_seconds": null,
            });
            let inputs = serde_json::to_value(&input)
                .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
            let company_id = history_company_id(&req, "");
            state.record_history(
                &company_id,
                "currency",
                &caller.actor,
                inputs.clone(),
                results.clone(),
            );
            return Ok(calculation_response("currency", inputs, results));
        }
        let rate = state.rate_for(&from, &to).ok_or_else(|| {
            UnifiedError::not_found(format!(
                "No stored exchange rate for {from}/{to}: configure a rate first"
            ))
            .with_metadata("error_code", json!("currency_rate_unavailable"))
            .with_metadata("base_currency", json!(from))
            .with_metadata("target_currency", json!(to))
        })?;
        let age_seconds = now()
            .signed_duration_since(rate.fetched_at)
            .num_seconds()
            .max(0) as u64;
        let stale = age_seconds > rate.max_age_seconds;
        let converted = amount * rate.rate;
        if !converted.is_finite() {
            return Err(UnifiedError::bad_request(
                "converted amount could not be computed from the stored rate",
            ));
        }
        let results = json!({
            "amount": round_money(amount),
            "from_currency": from,
            "to_currency": to,
            "rate": round_ratio(rate.rate),
            "converted_amount": round_money(converted),
            "provider": rate.provider,
            "rate_fetched_at": rate.fetched_at,
            "rate_age_seconds": age_seconds,
            "rate_max_age_seconds": rate.max_age_seconds,
            "stale": stale,
        });
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let company_id = history_company_id(&req, "");
        state.record_history(
            &company_id,
            "currency",
            &caller.actor,
            inputs.clone(),
            results.clone(),
        );
        Ok(calculation_response("currency", inputs, results))
    })();
    json::respond(result, debug)
}

pub(crate) async fn estimate_tax(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<TaxEstimateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let company_id = require_text(&input.company_id, "company_id", 64)?;
        if !input.inputs.is_object() {
            return Err(UnifiedError::bad_request(
                "inputs must be a JSON object",
            ));
        }
        // Delegate to the tax module through the hook bus. This module
        // holds no tax rules and never guesses a tax figure.
        let hook = ctx.hooks.emit(
            "tax.estimate_requested",
            json!({
                "company_id": company_id,
                "inputs": input.inputs,
                "jurisdiction": input.jurisdiction,
                "tax_year": input.tax_year,
                "actor": caller.actor,
            }),
        );
        let mut answer: Option<Value> = None;
        for result in &hook.results {
            if result.get("estimated_tax").and_then(|v| v.as_f64()).is_some()
                || result.get("tax_amount").and_then(|v| v.as_f64()).is_some()
            {
                answer = Some(result.clone());
                break;
            }
        }
        let Some(answer) = answer else {
            return Err(UnifiedError::unprocessable(
                "Tax estimate is unavailable: no tax handler answered tax.estimate_requested",
            )
            .with_metadata("error_code", json!("tax_estimate_unavailable"))
            .with_metadata("company_id", json!(company_id)));
        };
        let estimated_tax = answer
            .get("estimated_tax")
            .or_else(|| answer.get("tax_amount"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        if !estimated_tax.is_finite() || estimated_tax < 0.0 {
            return Err(UnifiedError::unprocessable(
                "Tax estimate is unavailable: the tax handler returned an invalid estimate",
            )
            .with_metadata("error_code", json!("tax_estimate_unavailable")));
        }
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = json!({
            "company_id": company_id,
            "estimated_tax": round_money(estimated_tax),
            "currency": answer.get("currency").and_then(|v| v.as_str()).unwrap_or(""),
            "jurisdiction": input.jurisdiction,
            "tax_year": input.tax_year,
            "breakdown": answer.get("breakdown").cloned().unwrap_or(Value::Null),
            "provider": answer.get("provider").and_then(|v| v.as_str()).unwrap_or(""),
        });
        state.record_history(
            &company_id,
            "tax_estimate",
            &caller.actor,
            inputs.clone(),
            results.clone(),
        );
        Ok(calculation_response("tax_estimate", inputs, results))
    })();
    json::respond(result, debug)
}

pub(crate) fn valid_calculator_type(value: &str) -> bool {
    CALCULATOR_TYPES.contains(&value)
}

pub(crate) async fn list_calculations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<SavedCalculation>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let params = query.into_inner();
        let mut items: Vec<SavedCalculation> = state
            .calculations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                matches_search(
                    &format!("{} {} {}", item.title, item.calculator_type, item.company_id),
                    &params,
                ) && params
                    .filters
                    .get("company_id")
                    .is_none_or(|company| item.company_id == *company)
                    && params
                        .filters
                        .get("calculator_type")
                        .is_none_or(|kind| item.calculator_type == *kind)
                    && params
                        .filters
                        .get("created_by")
                        .is_none_or(|actor| item.created_by == *actor)
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

pub(crate) async fn create_calculation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<CreateSavedCalculation>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SavedCalculation> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let input = body.into_inner();
        let company_id = require_text(&input.company_id, "company_id", 64)?;
        let title = require_text(&input.title, "title", 200)?;
        if !valid_calculator_type(&input.calculator_type) {
            return Err(UnifiedError::bad_request(format!(
                "calculator_type must be one of: {}",
                CALCULATOR_TYPES.join(", ")
            )));
        }
        if !input.inputs.is_object() || !input.results.is_object() {
            return Err(UnifiedError::bad_request(
                "inputs and results must be JSON objects",
            ));
        }
        let calculation = SavedCalculation {
            id: new_id(),
            company_id: company_id.clone(),
            calculator_type: input.calculator_type.clone(),
            title,
            inputs: input.inputs,
            results: input.results,
            created_by: caller.actor.clone(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .calculations
            .insert(calculation.id.clone(), calculation.clone());
        ctx.hooks.emit(
            "calculator.calculation_saved",
            json!({
                "calculation_id": calculation.id,
                "company_id": company_id,
                "calculator_type": calculation.calculator_type,
                "actor": caller.actor,
            }),
        );
        Ok(calculation)
    })();
    respond_created(result, debug)
}

pub(crate) async fn get_calculation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SavedCalculation> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        state
            .calculations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Saved calculation not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_calculation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSavedCalculation>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SavedCalculation> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let input = body.into_inner();
        if let Some(title) = &input.title {
            require_text(title, "title", 200)?;
        }
        if let Some(inputs) = &input.inputs {
            if !inputs.is_object() {
                return Err(UnifiedError::bad_request("inputs must be a JSON object"));
            }
        }
        if let Some(results) = &input.results {
            if !results.is_object() {
                return Err(UnifiedError::bad_request("results must be a JSON object"));
            }
        }
        let mut calculation = state
            .calculations
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Saved calculation not found"))?;
        if let Some(title) = input.title {
            calculation.title = title.trim().to_string();
        }
        if let Some(inputs) = input.inputs {
            calculation.inputs = inputs;
        }
        if let Some(results) = input.results {
            calculation.results = results;
        }
        calculation.updated_at = now();
        let updated = calculation.clone();
        drop(calculation);
        let _ = caller;
        Ok(updated)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_calculation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let calculation_id = path.as_str().to_string();
        let removed = state
            .calculations
            .remove(&calculation_id)
            .map(|(_, item)| item);
        let Some(removed) = removed else {
            return Err(UnifiedError::not_found("Saved calculation not found"));
        };
        // Reports built on the deleted calculation go with it.
        state
            .reports
            .retain(|_, report| report.calculation_id != calculation_id);
        ctx.hooks.emit(
            "calculator.calculation_deleted",
            json!({
                "calculation_id": calculation_id,
                "company_id": removed.company_id,
                "actor": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "id": calculation_id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_reports(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<CalculationReport>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let params = query.into_inner();
        let mut items: Vec<CalculationReport> = state
            .reports
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                matches_search(
                    &format!("{} {} {}", item.title, item.calculation_id, item.company_id),
                    &params,
                ) && params
                    .filters
                    .get("company_id")
                    .is_none_or(|company| item.company_id == *company)
                    && params
                        .filters
                        .get("visibility")
                        .is_none_or(|visibility| item.visibility == *visibility)
                    && params
                        .filters
                        .get("calculation_id")
                        .is_none_or(|calculation| item.calculation_id == *calculation)
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

pub(crate) async fn create_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<CreateReport>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalculationReport> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let input = body.into_inner();
        let company_id = require_text(&input.company_id, "company_id", 64)?;
        let title = require_text(&input.title, "title", 200)?;
        let visibility = input.visibility.unwrap_or_else(|| "private".to_string());
        if !REPORT_VISIBILITIES.contains(&visibility.as_str()) {
            return Err(UnifiedError::bad_request(
                "visibility must be one of: private, shared, public",
            ));
        }
        if !state.calculations.contains_key(input.calculation_id.as_str()) {
            return Err(UnifiedError::not_found(
                "Saved calculation not found for this report",
            ));
        }
        let report = CalculationReport {
            id: new_id(),
            calculation_id: input.calculation_id.clone(),
            company_id: company_id.clone(),
            title,
            share_token: new_share_token(),
            visibility: visibility.clone(),
            created_by: caller.actor.clone(),
            created_at: now(),
            updated_at: now(),
        };
        state.reports.insert(report.id.clone(), report.clone());
        ctx.hooks.emit(
            "calculator.report_shared",
            json!({
                "report_id": report.id,
                "calculation_id": report.calculation_id,
                "company_id": company_id,
                "share_token": report.share_token,
                "visibility": visibility,
                "actor": caller.actor,
            }),
        );
        Ok(report)
    })();
    respond_created(result, debug)
}

pub(crate) async fn get_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalculationReport> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let report = state
            .reports
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Report not found"))?;
        if report.visibility == "private" && report.created_by != caller.actor {
            return Err(UnifiedError::forbidden(
                "This report is private: only its creator may read it",
            ));
        }
        Ok(report)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateReport>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalculationReport> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let input = body.into_inner();
        if let Some(title) = &input.title {
            require_text(title, "title", 200)?;
        }
        if let Some(visibility) = &input.visibility {
            if !REPORT_VISIBILITIES.contains(&visibility.as_str()) {
                return Err(UnifiedError::bad_request(
                    "visibility must be one of: private, shared, public",
                ));
            }
        }
        let mut report = state
            .reports
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Report not found"))?;
        if let Some(title) = input.title {
            report.title = title.trim().to_string();
        }
        if let Some(visibility) = input.visibility {
            report.visibility = visibility;
        }
        report.updated_at = now();
        let updated = report.clone();
        drop(report);
        let _ = caller;
        Ok(updated)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let _ = caller;
        state
            .reports
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Report not found"))
    })();
    json::respond(result, debug)
}

/// Fetch a report through its share token. The token is random and
/// unguessable, but the endpoint still requires an authenticated
/// caller with `calculator:use`; private reports additionally
/// require the caller to be the creator.
pub(crate) async fn get_shared_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let token = path.as_str();
        let report = state
            .reports
            .iter()
            .find(|entry| entry.value().share_token == token)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Shared report not found"))?;
        if report.visibility == "private" && report.created_by != caller.actor {
            return Err(UnifiedError::forbidden(
                "This report is private: only its creator may read it",
            ));
        }
        let calculation = state
            .calculations
            .get(report.calculation_id.as_str())
            .map(|entry| entry.value().clone());
        Ok(json!({
            "report": report,
            "calculation": calculation,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<CalculationHistoryEntry>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let params = query.into_inner();
        let mut items: Vec<CalculationHistoryEntry> = state
            .history
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                matches_search(
                    &format!("{} {} {}", item.calculator_type, item.company_id, item.actor),
                    &params,
                ) && params
                    .filters
                    .get("company_id")
                    .is_none_or(|company| item.company_id == *company)
                    && params
                        .filters
                        .get("calculator_type")
                        .is_none_or(|kind| item.calculator_type == *kind)
                    && params
                        .filters
                        .get("actor")
                        .is_none_or(|actor| item.actor == *actor)
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

pub(crate) async fn get_history_entry(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalculationHistoryEntry> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        state
            .history
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("History entry not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_rates(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<CurrencyRate>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let params = query.into_inner();
        let mut items: Vec<CurrencyRate> = state
            .rates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                matches_search(
                    &format!("{} {} {}", item.base_currency, item.target_currency, item.provider),
                    &params,
                ) && params
                    .filters
                    .get("base_currency")
                    .is_none_or(|base| item.base_currency == *base)
                    && params
                        .filters
                        .get("target_currency")
                        .is_none_or(|target| item.target_currency == *target)
                    && params
                        .filters
                        .get("provider")
                        .is_none_or(|provider| item.provider == *provider)
            })
            .collect();
        items.sort_by(|a, b| {
            a.base_currency
                .cmp(&b.base_currency)
                .then(a.target_currency.cmp(&b.target_currency))
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

pub(crate) async fn create_rate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<CreateCurrencyRate>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CurrencyRate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let input = body.into_inner();
        let base = input.base_currency.trim().to_string();
        let target = input.target_currency.trim().to_string();
        if !valid_currency_code(&base) {
            return Err(UnifiedError::bad_request(
                "base_currency must be a 3-letter uppercase ISO 4217 code",
            ));
        }
        if !valid_currency_code(&target) {
            return Err(UnifiedError::bad_request(
                "target_currency must be a 3-letter uppercase ISO 4217 code",
            ));
        }
        if base == target {
            return Err(UnifiedError::bad_request(
                "base_currency and target_currency must differ",
            ));
        }
        let rate = require_finite(input.rate, "rate")?;
        if rate <= 0.0 {
            return Err(UnifiedError::bad_request("rate must be greater than zero"));
        }
        let provider = match input.provider {
            Some(value) => require_text(&value, "provider", 120)?,
            None => state.settings_snapshot().rate_provider,
        };
        let max_age = match input.max_age_seconds {
            Some(seconds) if seconds >= 1 => seconds,
            Some(_) => {
                return Err(UnifiedError::bad_request(
                    "max_age_seconds must be at least 1",
                ));
            }
            None => state.settings_snapshot().rate_max_age_seconds,
        };
        // One stored rate per (base, target, provider): a new rate
        // for the same triple replaces the old one.
        state.rates.retain(|_, existing| {
            !(existing.base_currency == base
                && existing.target_currency == target
                && existing.provider == provider)
        });
        let record = CurrencyRate {
            id: new_id(),
            base_currency: base,
            target_currency: target,
            rate,
            provider,
            fetched_at: input.fetched_at.unwrap_or_else(now),
            max_age_seconds: max_age,
            created_at: now(),
            updated_at: now(),
        };
        state.rates.insert(record.id.clone(), record.clone());
        let _ = caller;
        Ok(record)
    })();
    respond_created(result, debug)
}

pub(crate) async fn delete_rate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let _ = caller;
        state
            .rates
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Currency rate not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalculatorSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        Ok(state.settings_snapshot())
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<UpdateCalculatorSettings>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalculatorSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let input = body.into_inner();
        if let Some(provider) = &input.rate_provider {
            require_text(provider, "rate_provider", 120)?;
        }
        if let Some(seconds) = input.rate_max_age_seconds {
            if seconds < 1 {
                return Err(UnifiedError::bad_request(
                    "rate_max_age_seconds must be at least 1",
                ));
            }
        }
        let mut settings = state
            .settings
            .write()
            .expect("calculator settings lock poisoned");
        if let Some(provider) = input.rate_provider {
            settings.rate_provider = provider.trim().to_string();
        }
        if let Some(seconds) = input.rate_max_age_seconds {
            settings.rate_max_age_seconds = seconds;
        }
        let updated = settings.clone();
        drop(settings);
        let _ = caller;
        Ok(updated)
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verified_request(company: &str) -> HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        if !company.is_empty() {
            req.extensions_mut().insert(
                unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                    .with_company(company.to_string()),
            );
        }
        req
    }

    #[test]
    fn history_company_id_prefers_the_verified_caller_company() {
        let req = verified_request("company-a");
        assert_eq!(history_company_id(&req, "fallback"), "company-a");
    }

    #[test]
    fn history_company_id_falls_back_without_a_verified_company() {
        let req = verified_request("");
        assert_eq!(history_company_id(&req, "fallback"), "fallback");
    }

    #[test]
    fn history_company_id_ignores_a_forged_company_header() {
        let req = actix_web::test::TestRequest::default()
            .insert_header(("x-company-id", "company-forged"))
            .to_http_request();
        assert_eq!(history_company_id(&req, "fallback"), "fallback");
    }
}
