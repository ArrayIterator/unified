//! calculator - hooks.

use unified_core::hooks::HookContext;
use serde_json::{json, Value};
use std::sync::Arc;
use super::compute::*;
use super::state::*;
use super::types::*;
use super::validation::*;

/// The unit cost for a product from the latest saved pricing
/// calculation whose inputs name that product. The cost is the
/// solved `cost` in the calculation's results -- stored data, never
/// computed here and never guessed. When the request carries a
/// company id, only calculations of that company count. Returns
/// the cost plus the calculation it came from, for traceability.
pub(crate) fn latest_pricing_cost(
    state: &CalculatorState,
    product_id: &str,
    company_id: Option<&str>,
) -> Option<(f64, String)> {
    state
        .calculations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|calculation| calculation.calculator_type == "pricing")
        .filter(|calculation| {
            company_id.is_none_or(|company| calculation.company_id == company)
        })
        .filter(|calculation| {
            calculation
                .inputs
                .get("product_id")
                .and_then(|value| value.as_str())
                == Some(product_id)
        })
        .filter_map(|calculation| {
            let cost = calculation.results.get("cost").and_then(|value| value.as_f64())?;
            if cost.is_finite() && cost >= 0.0 {
                Some((calculation.created_at, cost, calculation.id.clone()))
            } else {
                None
            }
        })
        .max_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, cost, id)| (cost, id))
}

/// Answer `manufacturing.cost.requested` with per-product unit
/// costs from saved pricing calculations (see
/// `latest_pricing_cost`). The answer carries `unit_costs`
/// (product id to unit cost), `gaps` (one entry per product with
/// no stored cost, so the caller can report `partial: true`),
/// `partial`, and `sources` (product id to the calculation the
/// cost came from). Products with no stored cost are listed in
/// `gaps` -- never answered with zero, never fabricated. The
/// request may carry `items: [{product_id, ...}]` (the shape
/// manufacturing sends) or a single `product_id`.
pub(crate) fn on_manufacturing_cost_requested(state: Arc<CalculatorState>, ctx: &mut HookContext) {
    let mut product_ids: Vec<String> = ctx
        .get("items")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("product_id").and_then(|value| value.as_str()))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if product_ids.is_empty() {
        if let Some(product_id) = ctx
            .get("product_id")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            product_ids.push(product_id.to_string());
        }
    }
    if product_ids.is_empty() {
        return;
    }
    product_ids.sort();
    product_ids.dedup();
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut unit_costs = serde_json::Map::new();
    let mut sources = serde_json::Map::new();
    let mut gaps: Vec<Value> = Vec::new();
    for product_id in &product_ids {
        match latest_pricing_cost(&state, product_id, company_id.as_deref()) {
            Some((cost, calculation_id)) => {
                unit_costs.insert(product_id.clone(), json!(round_money(cost)));
                sources.insert(product_id.clone(), json!(calculation_id));
            }
            None => gaps.push(json!({
                "product_id": product_id,
                "reason": "No saved pricing calculation holds a unit cost for this product",
            })),
        }
    }
    let partial = !gaps.is_empty();
    let answer = json!({
        "unit_costs": unit_costs,
        "gaps": gaps,
        "partial": partial,
        "sources": sources,
    });
    ctx.set("unit_costs", answer["unit_costs"].clone());
    ctx.set("gaps", answer["gaps"].clone());
    ctx.set("partial", json!(partial));
    ctx.add_result(answer);
}

/// Answer `calculator.rate_requested` with the stored rate for a
/// currency pair, when one is stored. No result means "no rate
/// stored" and the requester must treat the rate as unavailable.
pub(crate) fn on_rate_requested(state: Arc<CalculatorState>, ctx: &mut HookContext) {
    let base = ctx
        .get("base_currency")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let target = ctx
        .get("target_currency")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if !valid_currency_code(&base) || !valid_currency_code(&target) {
        return;
    }
    if let Some(rate) = state.rate_for(&base, &target) {
        let age_seconds = now()
            .signed_duration_since(rate.fetched_at)
            .num_seconds()
            .max(0) as u64;
        ctx.add_result(json!({
            "base_currency": rate.base_currency,
            "target_currency": rate.target_currency,
            "rate": round_ratio(rate.rate),
            "provider": rate.provider,
            "stale": age_seconds > rate.max_age_seconds,
            "rate_age_seconds": age_seconds,
        }));
    }
}

/// Answer `calculator.depreciation_requested` with the schedule
/// computed by `compute_depreciation` -- the same math the HTTP
/// `POST /calculator/depreciation` endpoint runs. The request
/// carries the fields the assets module sends: `purchase_price`
/// (the asset cost), `salvage_value`, `useful_life_years`,
/// `method` (`straight_line` or `declining_balance`), and
/// optionally `multiplier`. The answer is the computed result
/// object itself, so it carries `schedule` (the field the assets
/// side stores) plus `cost`, `salvage_value`,
/// `useful_life_years`, `method`, `multiplier`,
/// `annual_depreciation`, and `total_depreciation` -- the same
/// field names the HTTP endpoint returns under `results`. The
/// computation is pure, so the state is not used; it is taken
/// only to keep this handler's shape identical to the other
/// handlers in this file. When the request is missing a field or
/// the inputs fail validation, the answer is `{"error": ...}`
/// instead, so the requester can record the reason honestly
/// instead of treating silence as "no schedule exists".
pub(crate) fn on_depreciation_requested(_state: Arc<CalculatorState>, ctx: &mut HookContext) {
    let cost = match ctx.get("purchase_price").and_then(Value::as_f64) {
        Some(cost) => cost,
        None => {
            ctx.add_result(json!({"error": "purchase_price is required"}));
            return;
        }
    };
    let salvage_value = ctx
        .get("salvage_value")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let useful_life_years = match ctx
        .get("useful_life_years")
        .and_then(Value::as_u64)
        .and_then(|years| u32::try_from(years).ok())
    {
        Some(years) => years,
        None => {
            ctx.add_result(json!({
                "error": "useful_life_years is required and must be a whole number of years",
            }));
            return;
        }
    };
    let method = match ctx
        .get("method")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|method| !method.is_empty())
    {
        Some(method) => method.to_string(),
        None => {
            ctx.add_result(json!({"error": "method is required"}));
            return;
        }
    };
    let multiplier = ctx.get("multiplier").and_then(Value::as_f64);
    let input = DepreciationInput {
        cost,
        salvage_value,
        useful_life_years,
        method,
        multiplier,
    };
    match compute_depreciation(&input) {
        Ok(results) => ctx.add_result(results),
        Err(error) => ctx.add_result(json!({"error": error.message})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset_request() -> Value {
        json!({
            "asset_id": "00000000-0000-0000-0000-000000000001",
            "method": "straight_line",
            "purchase_price": 10000.0,
            "salvage_value": 1000.0,
            "useful_life_years": 5,
            "purchase_date": null,
            "currency": "USD",
        })
    }

    fn answer_for(request: Value) -> HookContext {
        let state = Arc::new(CalculatorState::new());
        let mut ctx = HookContext::new("calculator.depreciation_requested", request);
        on_depreciation_requested(state, &mut ctx);
        ctx
    }

    #[test]
    fn depreciation_request_answers_with_schedule() {
        // The exact payload the assets module sends, and the exact
        // shape it stores: one result whose `schedule` is an array
        // of one row per year.
        let ctx = answer_for(asset_request());
        assert_eq!(ctx.results.len(), 1);
        let result = &ctx.results[0];
        let schedule = result["schedule"].as_array().expect("schedule is an array");
        assert_eq!(schedule.len(), 5);
        assert_eq!(schedule[0]["depreciation"].as_f64().unwrap(), 1800.0);
        assert_eq!(schedule[4]["book_value"].as_f64().unwrap(), 1000.0);
        assert_eq!(result["method"].as_str().unwrap(), "straight_line");
        assert_eq!(result["total_depreciation"].as_f64().unwrap(), 9000.0);
        assert_eq!(result["cost"].as_f64().unwrap(), 10000.0);
    }

    #[test]
    fn depreciation_request_declining_balance_front_loads() {
        let mut request = asset_request();
        request["method"] = json!("declining_balance");
        request["salvage_value"] = json!(0.0);
        let ctx = answer_for(request);
        assert_eq!(ctx.results.len(), 1);
        let schedule = ctx.results[0]["schedule"].as_array().expect("schedule is an array");
        assert_eq!(schedule.len(), 5);
        assert_eq!(schedule[0]["depreciation"].as_f64().unwrap(), 4000.0);
        assert!(schedule[1]["depreciation"].as_f64().unwrap() < 4000.0);
    }

    #[test]
    fn depreciation_request_missing_price_answers_error() {
        // A missing field is answered with an error, never with
        // silence: the requester records the reason instead of
        // treating "no answer" as a pending schedule.
        let mut request = asset_request();
        request.as_object_mut().unwrap().remove("purchase_price");
        let ctx = answer_for(request);
        assert_eq!(ctx.results.len(), 1);
        assert!(ctx.results[0]["schedule"].is_null());
        assert_eq!(
            ctx.results[0]["error"].as_str().unwrap(),
            "purchase_price is required"
        );
    }

    #[test]
    fn depreciation_request_missing_method_answers_error() {
        let mut request = asset_request();
        request.as_object_mut().unwrap().remove("method");
        let ctx = answer_for(request);
        assert_eq!(ctx.results.len(), 1);
        assert!(ctx.results[0]["schedule"].is_null());
        assert_eq!(ctx.results[0]["error"].as_str().unwrap(), "method is required");
    }

    #[test]
    fn depreciation_request_invalid_inputs_answer_error() {
        // Unknown method and cost at or below salvage value both
        // fail validation inside compute_depreciation; the handler
        // surfaces the message as the answer's error.
        let mut request = asset_request();
        request["method"] = json!("sideways");
        let ctx = answer_for(request);
        assert_eq!(ctx.results.len(), 1);
        assert!(ctx.results[0]["schedule"].is_null());
        assert!(ctx.results[0]["error"]
            .as_str()
            .unwrap()
            .contains("method must be one of"));

        let mut request = asset_request();
        request["purchase_price"] = json!(1000.0);
        request["salvage_value"] = json!(1000.0);
        let ctx = answer_for(request);
        assert_eq!(ctx.results.len(), 1);
        assert!(ctx.results[0]["schedule"].is_null());
        assert!(ctx.results[0]["error"]
            .as_str()
            .unwrap()
            .contains("cost must be greater than salvage_value"));
    }
}
