// pricing - service.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::middleware::TenantContext;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::money::*;
use super::recommendations::*;
use super::discounts::*;
use super::proration::*;
use super::reconciliation::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::handlers_recommendations::*;
use super::handlers_rules::*;
use super::handlers_coupons::*;
use super::handlers_prices::*;
use super::handlers_bundles::*;
use super::handlers_reconciliation::*;
use super::module::*;

/// Build the AI narrative draft for a recommendation. The text is
/// assembled from the stored recommendation record only: every
/// number in the draft is copied from the engine output. The AI
/// (or the admin reading the draft) never invents a price.
pub(crate) fn build_ai_narrative(recommendation: &PricingRecommendation) -> String {
    let mut draft = String::new();
    draft.push_str(&format!(
        "Price recommendation for product {} ({}): suggested price {} {}. ",
        recommendation.product_id,
        recommendation.kind,
        recommendation.suggested_price,
        recommendation.currency
    ));
    for line in &recommendation.lines {
        draft.push_str(&format!("{}: {} {}. ", line.label, line.amount, line.currency));
    }
    draft.push_str(&format!(
        "Status: {}. This is a suggestion only; an admin must confirm it before any price changes.",
        recommendation.status
    ));
    draft
}

// Shared evaluation (used by the HTTP endpoint and the hook).

/// Evaluate discounts for a caller that supplies lines, an
/// optional coupon code, and an optional customer id. Active rules
/// always run; the coupon runs last when usable. Returns the
/// evaluation plus, when a coupon was requested but unusable, the
/// reason (the evaluation still runs without it).
pub(crate) fn evaluate_for_caller(
    state: &PricingState,
    tenant_id: &str,
    lines: &[DiscountLineInput],
    coupon_code: Option<&str>,
    customer_id: Option<&str>,
    currency: &str,
    extra_rule_ids: &[String],
) -> UnifiedResult<(DiscountEvaluation, Option<String>)> {
    let mut rules = state.active_rules(tenant_id);
    // Extra rule IDs get exactly the same gating as the active
    // rules above: only rules that are active and inside their
    // validity window are applied. A disabled or expired rule
    // must not be applicable just because its ID was supplied.
    let active = rules.clone();
    for rule_id in extra_rule_ids {
        if rules.iter().any(|item| item.rule_id == *rule_id) {
            continue;
        }
        if let Some(rule) = active.iter().find(|item| item.rule_id == *rule_id) {
            rules.push(rule.clone());
        }
    }
    let mut coupon_problem: Option<String> = None;
    if let Some(code) = coupon_code.map(str::trim).filter(|code| !code.is_empty()) {
        let normalized = code.to_uppercase();
        let found = state
            .coupons
            .iter()
            .map(|entry| entry.value().clone())
            .find(|coupon| coupon.code == normalized && coupon.tenant_id == tenant_id);
        match found {
            Some(coupon) => {
                let problem = coupon_usability(state, &coupon, customer_id, Utc::now());
                match problem {
                    Some(reason) => coupon_problem = Some(reason),
                    None => rules.push(coupon_to_engine(&coupon)),
                }
            }
            None => coupon_problem = Some("Coupon not found".to_string()),
        }
    }
    let evaluation = evaluate_discounts(lines, &rules, currency)?;
    Ok((evaluation, coupon_problem))
}

/// Convert an HTTP discount request into engine lines, resolving
/// every money input against the request currency.
pub(crate) fn discount_lines_from_request(
    request: &DiscountEvaluateRequest,
    currency: &str,
) -> UnifiedResult<Vec<DiscountLineInput>> {
    let mut lines = Vec::new();
    for line in &request.lines {
        let money = resolve_money(&line.unit_price, Some(currency))?;
        if money.currency != currency {
            return Err(UnifiedError::bad_request(
                "All discount lines must use the request currency",
            ));
        }
        lines.push(DiscountLineInput {
            product_id: line.product_id.trim().to_string(),
            unit_price_scaled: round_to_minor(money.amount_scaled, currency),
            quantity: line.quantity,
        });
    }
    Ok(lines)
}

// Hook handlers.

/// Read a scaled money amount from one hook payload line: a
/// `_scaled` field that is present must be an integer, the plain
/// decimal/number form is parsed with integer math, and a missing
/// amount is an error. A malformed or absent amount must never
/// silently become 0 and understate a subtotal or tax.
fn hook_scaled_amount(
    item: &Value,
    scaled_key: &str,
    plain_key: &str,
    currency: &str,
) -> Result<i64, String> {
    match item.get(scaled_key) {
        Some(value) => value
            .as_i64()
            .ok_or_else(|| format!("{scaled_key} must be an integer")),
        None => match item.get(plain_key) {
            Some(value) => {
                value_to_scaled(value, currency, plain_key).map_err(|error| error.to_string())
            }
            None => Err(format!("Line needs {plain_key}")),
        },
    }
}

/// Serialize a response value, mapping a serialization failure to
/// a 500 instead of silently answering with a null body.
pub(crate) fn serialize_response<T: Serialize>(value: &T) -> UnifiedResult<Value> {
    serde_json::to_value(value).map_err(|error| {
        UnifiedError::internal(format!("Failed to serialize response: {error}"))
    })
}

/// Answer `pricing.discount.evaluate_requested`: the store and
/// billing call this to evaluate discounts without touching their
/// own order. The request payload carries `lines` (each with
/// `product_id`, `unit_price` as a decimal string or scaled
/// integer, and `quantity`), plus optional `coupon_code`,
/// `customer_id`, and `currency`. The answer is added as a result
/// and never mutates the caller's order. A missing or invalid
/// payload is answered with an error object so the caller can
/// fail closed instead of guessing.
pub(crate) fn on_discount_evaluate_requested(state: Arc<PricingState>, ctx: &mut HookContext) {
    let tenant_id = match hook_tenant_id(ctx) {
        Ok(tenant_id) => tenant_id,
        Err(error) => {
            ctx.add_result(json!({ "valid": false, "error": error.to_string() }));
            return;
        }
    };
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| state.settings_for(&tenant_id).default_currency);
    let lines_value = match ctx.get("lines") {
        Some(value) => value.clone(),
        None => {
            ctx.add_result(json!({
                "valid": false,
                "error": "Discount evaluation needs order lines",
            }));
            return;
        }
    };
    let lines_array = match lines_value.as_array() {
        Some(array) => array.clone(),
        None => {
            ctx.add_result(json!({
                "valid": false,
                "error": "Order lines must be an array",
            }));
            return;
        }
    };
    let mut lines: Vec<DiscountLineInput> = Vec::new();
    for item in &lines_array {
        let product_id = item
            .get("product_id")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let quantity = item
            .get("quantity")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        // The unit price may arrive as a decimal string
        // ("19.99"), as scaled integer units, or as a JSON
        // number; all three are parsed with integer math. A
        // `_scaled` value that is not an integer, or a missing
        // unit price, is an error, never a silent 0.
        let unit_price = match hook_scaled_amount(item, "unit_price_scaled", "unit_price", &currency)
        {
            Ok(scaled) => scaled,
            Err(message) => {
                ctx.add_result(json!({
                    "valid": false,
                    "error": message,
                }));
                return;
            }
        };
        if product_id.trim().is_empty() || quantity == 0 || quantity > u32::MAX as u64 {
            ctx.add_result(json!({
                "valid": false,
                "error": "Each order line needs a product_id and a quantity of at least 1",
            }));
            return;
        }
        lines.push(DiscountLineInput {
            product_id,
            unit_price_scaled: round_to_minor(unit_price, &currency),
            quantity: quantity as u32,
        });
    }
    if lines.is_empty() {
        ctx.add_result(json!({
            "valid": false,
            "error": "Discount evaluation needs at least one order line",
        }));
        return;
    }
    let coupon_code = ctx
        .get("coupon_code")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let customer_id = ctx
        .get("customer_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    match evaluate_for_caller(
        &state,
        &tenant_id,
        &lines,
        coupon_code.as_deref(),
        customer_id.as_deref(),
        &currency,
        &[],
    ) {
        Ok((evaluation, coupon_problem)) => {
            state.emit(
                "pricing.discount.evaluated",
                json!({
                    "tenant_id": tenant_id,
                    "subtotal_scaled": evaluation.subtotal_scaled,
                    "total_discount_scaled": evaluation.total_discount_scaled,
                    "total_scaled": evaluation.total_scaled,
                    "currency": evaluation.currency,
                    "coupon_code": coupon_code,
                    "customer_id": customer_id,
                }),
            );
            let mut answer = match serialize_response(&evaluation) {
                Ok(value) => value,
                Err(error) => {
                    ctx.add_result(json!({ "valid": false, "error": error.to_string() }));
                    return;
                }
            };
            if let Value::Object(map) = &mut answer {
                map.insert("valid".to_string(), Value::Bool(true));
                if let Some(problem) = coupon_problem {
                    map.insert("coupon_problem".to_string(), Value::String(problem));
                }
            }
            ctx.add_result(answer);
        }
        Err(error) => {
            ctx.add_result(json!({ "valid": false, "error": error.to_string() }));
        }
    }
}

/// Answer `pricing.invoice.validate_requested`: billing calls this
/// before issuing an invoice. The payload carries `lines` (each
/// with `name`, `quantity`, `unit_price`, `tax_bp`, and
/// `tax_inclusive`), `discounts`, `proration_lines`, `claimed`
/// (subtotal, discounts, proration, tax_total, total), and
/// `currency`. Amounts may be decimal strings, scaled integers, or
/// JSON numbers; all are parsed with integer math. The answer
/// carries `valid` (true only when every component matches) and
/// the full reconciliation, and the module emits
/// `pricing.invoice.reconciled` or
/// `pricing.invoice.reconciliation_failed`. Billing must block
/// issuing when `valid` is false.
pub(crate) fn on_invoice_validate_requested(state: Arc<PricingState>, ctx: &mut HookContext) {
    let tenant_id = match hook_tenant_id(ctx) {
        Ok(tenant_id) => tenant_id,
        Err(error) => {
            ctx.add_result(json!({ "valid": false, "error": error.to_string() }));
            return;
        }
    };
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| state.settings_for(&tenant_id).default_currency);
    let invoice_id = ctx
        .get("invoice_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let parse_lines = |key: &str| -> Result<Vec<ReconcileLineInput>, String> {
        let value = match ctx.get(key) {
            Some(value) => value.clone(),
            None => return Ok(Vec::new()),
        };
        let array = value.as_array().ok_or_else(|| format!("{key} must be an array"))?;
        let mut lines = Vec::new();
        for item in array {
            let name = item
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let quantity = item
                .get("quantity")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let unit_price = match hook_scaled_amount(item, "unit_price_scaled", "unit_price", &currency)
            {
                Ok(scaled) => scaled,
                Err(message) => return Err(message),
            };
            let tax_bp = item
                .get("tax_bp")
                .and_then(|value| value.as_i64())
                .unwrap_or(0);
            let tax_inclusive = item
                .get("tax_inclusive")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            if quantity > u32::MAX as u64 {
                return Err("Line quantity is too large".to_string());
            }
            lines.push(ReconcileLineInput {
                name,
                quantity: quantity as u32,
                unit_price_scaled: unit_price,
                tax_bp,
                tax_inclusive,
            });
        }
        Ok(lines)
    };
    let parse_adjustments = |key: &str| -> Result<Vec<ReconcileAdjustmentInput>, String> {
        let value = match ctx.get(key) {
            Some(value) => value.clone(),
            None => return Ok(Vec::new()),
        };
        let array = value
            .as_array()
            .ok_or_else(|| format!("{key} must be an array"))?;
        let mut adjustments = Vec::new();
        for item in array {
            let label = item
                .get("label")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let amount = match hook_scaled_amount(item, "amount_scaled", "amount", &currency) {
                Ok(scaled) => scaled,
                Err(message) => return Err(message),
            };
            adjustments.push(ReconcileAdjustmentInput {
                label,
                amount_scaled: amount,
            });
        }
        Ok(adjustments)
    };
    let fail = |ctx: &mut HookContext, message: &str| {
        ctx.add_result(json!({ "valid": false, "error": message }));
    };
    let lines = match parse_lines("lines") {
        Ok(lines) => lines,
        Err(message) => {
            fail(ctx, &message);
            return;
        }
    };
    let discounts = match parse_adjustments("discounts") {
        Ok(items) => items,
        Err(message) => {
            fail(ctx, &message);
            return;
        }
    };
    let proration_lines = match parse_adjustments("proration_lines") {
        Ok(items) => items,
        Err(message) => {
            fail(ctx, &message);
            return;
        }
    };
    let claimed_value = match ctx.get("claimed") {
        Some(value) => value.clone(),
        None => {
            fail(ctx, "Invoice validation needs the claimed totals");
            return;
        }
    };
    let claimed_field = |field: &str| -> Result<i64, String> {
        match claimed_value.get(field) {
            Some(value) => value_to_scaled(value, &currency, field).map_err(|e| e.to_string()),
            None => match claimed_value.get(format!("{field}_scaled").as_str()) {
                Some(value) => value
                    .as_i64()
                    .ok_or_else(|| format!("Claimed {field} must be an integer")),
                None => Err(format!("Claimed totals must include {field}")),
            },
        }
    };
    let claim = (|| -> Result<ReconcileClaim, String> {
        Ok(ReconcileClaim {
            subtotal_scaled: claimed_field("subtotal")?,
            discounts_scaled: claimed_field("discounts")?,
            proration_scaled: claimed_field("proration")?,
            tax_total_scaled: claimed_field("tax_total")?,
            total_scaled: claimed_field("total")?,
        })
    })();
    let claim = match claim {
        Ok(claim) => claim,
        Err(message) => {
            fail(ctx, &message);
            return;
        }
    };
    match reconcile_invoice(&lines, &discounts, &proration_lines, &claim, &currency) {
        Ok(result) => {
            let record =
                state.record_reconciliation(&tenant_id, invoice_id.clone(), &result, "pricing-hook");
            if result.matched {
                state.emit(
                    "pricing.invoice.reconciled",
                    json!({
                        "tenant_id": tenant_id,
                        "invoice_id": invoice_id,
                        "reconciliation_id": record.id,
                        "currency": currency,
                    }),
                );
            } else {
                state.emit(
                    "pricing.invoice.reconciliation_failed",
                    json!({
                        "tenant_id": tenant_id,
                        "invoice_id": invoice_id,
                        "reconciliation_id": record.id,
                        "currency": currency,
                        "total_discrepancy_scaled": result.total_discrepancy_scaled,
                        "components": result.components,
                    }),
                );
            }
            let mut answer = match serialize_response(&result) {
                Ok(value) => value,
                Err(error) => {
                    fail(ctx, &error.to_string());
                    return;
                }
            };
            if let Value::Object(map) = &mut answer {
                map.insert("valid".to_string(), Value::Bool(result.matched));
                map.insert(
                    "reconciliation_id".to_string(),
                    Value::String(record.id.clone()),
                );
            }
            ctx.add_result(answer);
        }
        Err(error) => {
            fail(ctx, &error.to_string());
        }
    }
}

/// Answer `pricing.ai.recommend_requested`: the AI module asks
/// for a narrative draft for a stored recommendation. The draft is
/// built from the engine record only - the AI words the summary,
/// it never invents prices. When the recommendation does not
/// exist, no result is added, so the caller can report the
/// recommendation as unavailable.
pub(crate) fn on_ai_recommend_requested(state: Arc<PricingState>, ctx: &mut HookContext) {
    let tenant_id = match hook_tenant_id(ctx) {
        Ok(tenant_id) => tenant_id,
        Err(_) => return,
    };
    let recommendation_id = match ctx.get("recommendation_id").and_then(|value| value.as_str()) {
        Some(id) if !id.trim().is_empty() => id.to_string(),
        _ => return,
    };
    if let Some(recommendation) = state.recommendations.get(&recommendation_id) {
        if recommendation.tenant_id != tenant_id {
            return;
        }
        let draft = build_ai_narrative(&recommendation);
        let numbers = json!({
            "suggested_price_scaled": recommendation.suggested_price_scaled,
            "suggested_price": recommendation.suggested_price,
            "currency": recommendation.currency,
            "lines": recommendation.lines,
        });
        state.emit(
            "pricing.ai.draft_created",
            json!({
                "tenant_id": tenant_id,
                "recommendation_id": recommendation.id,
                "currency": recommendation.currency,
            }),
        );
        ctx.add_result(json!({
            "tenant_id": tenant_id,
            "recommendation_id": recommendation.id,
            "draft": draft,
            "numbers": numbers,
        }));
    }
}

// HTTP handlers: recommendations.

pub(crate) fn store_recommendation(
    state: &PricingState,
    tenant_id: &str,
    product_id: &str,
    kind: &str,
    suggested_price_scaled: i64,
    currency: &str,
    lines: Vec<CalculationLine>,
    inputs: Value,
    actor: &str,
    cost_lines: &[CostLineInput],
) -> PricingRecommendation {
    let recommendation = PricingRecommendation {
        id: new_id(),
        tenant_id: tenant_id.to_string(),
        product_id: product_id.to_string(),
        kind: kind.to_string(),
        suggested_price_scaled,
        currency: currency.to_string(),
        suggested_price: format_scaled(suggested_price_scaled, currency),
        lines,
        inputs,
        status: "suggested".to_string(),
        created_by: actor.to_string(),
        confirmed_by: None,
        price_version_id: None,
        created_at: now(),
        updated_at: now(),
    };
    for cost in cost_lines {
        if let Ok(money) = resolve_money(&cost.cost, Some(currency)) {
            let line = PricingCostLine {
                id: new_id(),
                tenant_id: tenant_id.to_string(),
                recommendation_id: recommendation.id.clone(),
                label: cost.label.trim().to_string(),
                source: cost.source.trim().to_string(),
                cost_scaled: round_to_minor(money.amount_scaled, currency),
                currency: currency.to_string(),
            };
            state.cost_lines.insert(line.id.clone(), line);
        }
    }
    state
        .recommendations
        .insert(recommendation.id.clone(), recommendation.clone());
    state.emit(
        "pricing.recommendation.created",
        json!({
            "tenant_id": tenant_id,
            "recommendation_id": recommendation.id,
            "product_id": recommendation.product_id,
            "kind": recommendation.kind,
            "suggested_price_scaled": recommendation.suggested_price_scaled,
            "currency": recommendation.currency,
        }),
    );
    recommendation
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    const TENANT_ID: &str = "019a1234-5678-7abc-8def-0123456789ab";

    fn test_rule(id: &str, status: &str, valid_to: Option<DateTime<Utc>>) -> PricingRule {
        let now = Utc::now();
        PricingRule {
            id: id.to_string(),
            tenant_id: TENANT_ID.to_string(),
            name: id.to_string(),
            kind: "percent".to_string(),
            percent_bp: 1_000,
            amount_scaled: 0,
            currency: "USD".to_string(),
            buy_quantity: 0,
            get_quantity: 0,
            min_quantity: 0,
            min_subtotal_scaled: 0,
            product_ids: vec![],
            evaluation_order: 1,
            stackable: true,
            exclusive: false,
            status: status.to_string(),
            valid_from: None,
            valid_to,
            created_by: "tester".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn extra_rule_ids_cannot_apply_disabled_or_expired_rules() {
        let state = PricingState::new();
        let now = Utc::now();
        state.rules.insert(
            "rule-active".to_string(),
            test_rule("rule-active", "active", None),
        );
        state.rules.insert(
            "rule-draft".to_string(),
            test_rule("rule-draft", "draft", None),
        );
        state.rules.insert(
            "rule-expired".to_string(),
            test_rule("rule-expired", "active", Some(now - Duration::days(1))),
        );
        let lines = vec![DiscountLineInput {
            product_id: "p1".to_string(),
            unit_price_scaled: 1_000_000,
            quantity: 1,
        }];
        // The draft and expired rules are supplied deliberately as
        // extra IDs; only the active rule may apply (10% of 100.00).
        let (evaluation, _) = evaluate_for_caller(
            &state,
            TENANT_ID,
            &lines,
            None,
            None,
            "USD",
            &[
                "rule-active".to_string(),
                "rule-draft".to_string(),
                "rule-expired".to_string(),
            ],
        )
        .unwrap();
        assert_eq!(evaluation.total_discount_scaled, 100_000);
        assert_eq!(evaluation.total_scaled, 900_000);
        assert_eq!(evaluation.trace.len(), 1);
        assert_eq!(evaluation.trace[0].rule_id, "rule-active");
        assert!(evaluation.trace[0].applied);
    }

    fn discount_ctx(lines: Value) -> HookContext {
        HookContext::new(
            "pricing.discount.evaluate_requested",
            json!({
                "tenant_id": TENANT_ID,
                "currency": "USD",
                "lines": lines,
            }),
        )
    }

    #[test]
    fn hook_discount_evaluation_rejects_malformed_or_missing_money() {
        let state = Arc::new(PricingState::new());
        // A non-integer unit_price_scaled must fail closed, not
        // silently price the line at 0.
        let mut ctx = discount_ctx(json!([{
            "product_id": "p1",
            "quantity": 1,
            "unit_price_scaled": "19.99",
        }]));
        on_discount_evaluate_requested(Arc::clone(&state), &mut ctx);
        assert_eq!(ctx.results.len(), 1);
        assert_eq!(ctx.results[0].get("valid"), Some(&Value::Bool(false)));
        // A missing unit_price must fail closed as well.
        let mut ctx = discount_ctx(json!([{
            "product_id": "p1",
            "quantity": 1,
        }]));
        on_discount_evaluate_requested(Arc::clone(&state), &mut ctx);
        assert_eq!(ctx.results.len(), 1);
        assert_eq!(ctx.results[0].get("valid"), Some(&Value::Bool(false)));
        // A well-formed integer scaled price still evaluates.
        let mut ctx = discount_ctx(json!([{
            "product_id": "p1",
            "quantity": 1,
            "unit_price_scaled": 1_000_000,
        }]));
        on_discount_evaluate_requested(Arc::clone(&state), &mut ctx);
        assert_eq!(ctx.results.len(), 1);
        assert_eq!(ctx.results[0].get("valid"), Some(&Value::Bool(true)));
    }

    #[test]
    fn hook_invoice_validation_rejects_malformed_scaled_amounts() {
        let state = Arc::new(PricingState::new());
        let mut ctx = HookContext::new(
            "pricing.invoice.validate_requested",
            json!({
                "tenant_id": TENANT_ID,
                "currency": "USD",
                "lines": [{
                    "name": "Plan",
                    "quantity": 1,
                    "unit_price_scaled": 1_000_000,
                    "tax_bp": 0,
                    "tax_inclusive": false,
                }],
                "discounts": [{
                    "label": "Coupon",
                    "amount_scaled": "10.00",
                }],
            }),
        );
        on_invoice_validate_requested(Arc::clone(&state), &mut ctx);
        assert_eq!(ctx.results.len(), 1);
        assert_eq!(ctx.results[0].get("valid"), Some(&Value::Bool(false)));
    }

    struct FailingValue;

    impl Serialize for FailingValue {
        fn serialize<S: serde::Serializer>(
            &self,
            _serializer: S,
        ) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("serialization boom"))
        }
    }

    #[test]
    fn serialization_failure_is_a_server_error_not_a_null_body() {
        let error = serialize_response(&FailingValue).unwrap_err();
        assert_eq!(error.code, 500);
        assert!(serialize_response(&json!({ "valid": true })).is_ok());
    }
}
