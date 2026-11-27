// pricing - discounts.

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
use super::proration::*;
use super::reconciliation::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::service::*;
use super::handlers_recommendations::*;
use super::handlers_rules::*;
use super::handlers_coupons::*;
use super::handlers_prices::*;
use super::handlers_bundles::*;
use super::handlers_reconciliation::*;
use super::module::*;

// Discount engine (pure).

/// One order line the discount engine evaluates. Prices are scaled
/// minor units; the engine never sees float money.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscountLineInput {
    pub product_id: String,
    pub unit_price_scaled: i64,
    pub quantity: u32,
}

/// One discount rule as the engine sees it: a stored rule or a
/// coupon normalized into rule shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscountRuleInput {
    pub rule_id: String,
    pub name: String,
    /// `percent`, `fixed`, `buy_x_get_y`, or `volume`.
    pub kind: String,
    /// Basis points for `percent` and `volume`.
    #[serde(default)]
    pub percent_bp: i64,
    /// Fixed amount (scaled) for `fixed`.
    #[serde(default)]
    pub amount_scaled: i64,
    /// Buy quantity for `buy_x_get_y`.
    #[serde(default)]
    pub buy_quantity: u32,
    /// Free quantity for `buy_x_get_y`.
    #[serde(default)]
    pub get_quantity: u32,
    /// Minimum quantity for `volume`.
    #[serde(default)]
    pub min_quantity: u32,
    /// Minimum subtotal (scaled) for the rule to apply; 0 means none.
    #[serde(default)]
    pub min_subtotal_scaled: i64,
    /// Empty means the rule applies to every product.
    #[serde(default)]
    pub product_ids: Vec<String>,
    /// Declared evaluation order; lower runs first.
    pub evaluation_order: i64,
    /// When true the rule may combine with rules evaluated later.
    pub stackable: bool,
    /// When true and the rule applies, no later rule is evaluated.
    pub exclusive: bool,
}

/// One step of the evaluation trace: which rule applied, in which
/// order, and how much it removed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscountTraceEntry {
    pub rule_id: String,
    pub rule_name: String,
    pub kind: String,
    pub evaluation_order: i64,
    pub applied: bool,
    pub amount_removed_scaled: i64,
    pub amount_removed: String,
    pub running_total_scaled: i64,
    pub running_total: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
}

/// Result of a discount evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscountEvaluation {
    pub subtotal_scaled: i64,
    pub subtotal: String,
    pub total_discount_scaled: i64,
    pub total_discount: String,
    pub total_scaled: i64,
    pub total: String,
    pub currency: String,
    pub trace: Vec<DiscountTraceEntry>,
}

/// Line subtotal: quantity times the unit price, rounded half-up
/// to the minor unit.
pub(crate) fn discount_line_total(line: &DiscountLineInput, currency: &str) -> UnifiedResult<i64> {
    let raw = line
        .unit_price_scaled
        .checked_mul(i64::from(line.quantity))
        .ok_or_else(|| UnifiedError::bad_request("Line amount is too large"))?;
    Ok(round_to_minor(raw, currency))
}

/// Compute what one rule removes from the current running total.
/// Percentage and volume rules apply to the matching lines' share
/// of the running total; fixed rules remove a capped fixed amount;
/// buy-X-get-Y removes the price of the free units on matching
/// lines. Every amount is rounded half-up to the minor unit.
pub(crate) fn rule_removal(
    rule: &DiscountRuleInput,
    lines: &[DiscountLineInput],
    line_totals: &[i64],
    subtotal: i64,
    running_total: i64,
    currency: &str,
) -> UnifiedResult<i64> {
    if subtotal > 0 && rule.min_subtotal_scaled > 0 && subtotal < rule.min_subtotal_scaled {
        return Ok(0);
    }
    let matches = |product_id: &str| {
        rule.product_ids.is_empty()
            || rule.product_ids.iter().any(|id| id == product_id)
    };
    match rule.kind.as_str() {
        "percent" => {
            if !(0..=BASIS_POINTS).contains(&rule.percent_bp) {
                return Err(UnifiedError::bad_request(
                    "Discount percent must be between 0 and 10000 basis points",
                ));
            }
            // Apply to the matching lines' share of the running
            // total, so a product-scoped percent never discounts
            // lines it does not cover.
            let mut matching: i64 = 0;
            for (line, total) in lines.iter().zip(line_totals.iter()) {
                if matches(&line.product_id) {
                    matching = matching
                        .checked_add(*total)
                        .ok_or_else(|| UnifiedError::bad_request("Amount is too large"))?;
                }
            }
            if matching == 0 || subtotal == 0 {
                return Ok(0);
            }
            let share = mul_div_half_up(running_total, matching, subtotal)?;
            Ok(round_to_minor(percent_of(share, rule.percent_bp)?, currency).min(running_total))
        }
        "volume" => {
            if !(0..=BASIS_POINTS).contains(&rule.percent_bp) {
                return Err(UnifiedError::bad_request(
                    "Volume discount percent must be between 0 and 10000 basis points",
                ));
            }
            let mut matching: i64 = 0;
            let mut matching_quantity: u32 = 0;
            for (line, total) in lines.iter().zip(line_totals.iter()) {
                if matches(&line.product_id) {
                    matching_quantity += line.quantity;
                    matching = matching
                        .checked_add(*total)
                        .ok_or_else(|| UnifiedError::bad_request("Amount is too large"))?;
                }
            }
            if matching_quantity < rule.min_quantity || matching == 0 || subtotal == 0 {
                return Ok(0);
            }
            let share = mul_div_half_up(running_total, matching, subtotal)?;
            Ok(round_to_minor(percent_of(share, rule.percent_bp)?, currency).min(running_total))
        }
        "fixed" => {
            if rule.amount_scaled < 0 {
                return Err(UnifiedError::bad_request(
                    "Fixed discount amount must not be negative",
                ));
            }
            Ok(rule.amount_scaled.min(running_total))
        }
        "buy_x_get_y" => {
            if rule.buy_quantity == 0 || rule.get_quantity == 0 {
                return Err(UnifiedError::bad_request(
                    "Buy-X-get-Y needs a buy quantity and a get quantity above zero",
                ));
            }
            let group = i64::from(rule.buy_quantity) + i64::from(rule.get_quantity);
            let mut removed: i64 = 0;
            for line in lines {
                if !matches(&line.product_id) {
                    continue;
                }
                let free_units =
                    (i64::from(line.quantity) / group) * i64::from(rule.get_quantity);
                if free_units == 0 {
                    continue;
                }
                let unit = round_to_minor(line.unit_price_scaled, currency);
                let line_removed = round_to_minor(
                    unit.checked_mul(free_units)
                        .ok_or_else(|| UnifiedError::bad_request("Amount is too large"))?,
                    currency,
                );
                removed = removed
                    .checked_add(line_removed)
                    .ok_or_else(|| UnifiedError::bad_request("Amount is too large"))?;
            }
            Ok(removed.min(running_total))
        }
        _ => Err(UnifiedError::bad_request(
            "Discount kind must be \"percent\", \"fixed\", \"buy_x_get_y\", or \"volume\"",
        )),
    }
}

/// Evaluate discount rules against order lines in the declared
/// order. Rules are sorted by `evaluation_order` (ties by rule id,
/// so the order is deterministic). Stackable rules apply in
/// sequence on the running remainder; a non-stackable rule that
/// applies stops later rules; an exclusive rule that applies stops
/// later rules as well. The trace records every rule, applied or
/// skipped, with the running total after each step. The engine
/// never mutates an order: it only returns the evaluation.
pub fn evaluate_discounts(
    lines: &[DiscountLineInput],
    rules: &[DiscountRuleInput],
    currency: &str,
) -> UnifiedResult<DiscountEvaluation> {
    if lines.is_empty() {
        return Err(UnifiedError::bad_request(
            "At least one order line is required",
        ));
    }
    validate_currency(currency)?;
    let mut line_totals: Vec<i64> = Vec::new();
    let mut subtotal: i64 = 0;
    for line in lines {
        if line.product_id.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Order line product_id must not be empty",
            ));
        }
        if line.unit_price_scaled < 0 {
            return Err(UnifiedError::bad_request(
                "Order line unit price must not be negative",
            ));
        }
        if line.quantity == 0 {
            return Err(UnifiedError::bad_request(
                "Order line quantity must be at least 1",
            ));
        }
        let total = discount_line_total(line, currency)?;
        line_totals.push(total);
        subtotal = subtotal
            .checked_add(total)
            .ok_or_else(|| UnifiedError::bad_request("Order subtotal is too large"))?;
    }
    let mut ordered: Vec<&DiscountRuleInput> = rules.iter().collect();
    ordered.sort_by(|a, b| {
        a.evaluation_order
            .cmp(&b.evaluation_order)
            .then(a.rule_id.cmp(&b.rule_id))
    });
    let mut running_total = subtotal;
    let mut total_discount: i64 = 0;
    let mut trace: Vec<DiscountTraceEntry> = Vec::new();
    let mut stopped = false;
    for rule in ordered {
        if stopped {
            trace.push(DiscountTraceEntry {
                rule_id: rule.rule_id.clone(),
                rule_name: rule.name.clone(),
                kind: rule.kind.clone(),
                evaluation_order: rule.evaluation_order,
                applied: false,
                amount_removed_scaled: 0,
                amount_removed: format_scaled(0, currency),
                running_total_scaled: running_total,
                running_total: format_scaled(running_total, currency),
                skipped_reason: Some(
                    "Skipped: an earlier exclusive or non-stackable rule stopped evaluation"
                        .to_string(),
                ),
            });
            continue;
        }
        let removed = rule_removal(rule, lines, &line_totals, subtotal, running_total, currency)?;
        if removed == 0 {
            trace.push(DiscountTraceEntry {
                rule_id: rule.rule_id.clone(),
                rule_name: rule.name.clone(),
                kind: rule.kind.clone(),
                evaluation_order: rule.evaluation_order,
                applied: false,
                amount_removed_scaled: 0,
                amount_removed: format_scaled(0, currency),
                running_total_scaled: running_total,
                running_total: format_scaled(running_total, currency),
                skipped_reason: Some("Rule conditions did not match".to_string()),
            });
            continue;
        }
        running_total -= removed;
        total_discount += removed;
        trace.push(DiscountTraceEntry {
            rule_id: rule.rule_id.clone(),
            rule_name: rule.name.clone(),
            kind: rule.kind.clone(),
            evaluation_order: rule.evaluation_order,
            applied: true,
            amount_removed_scaled: removed,
            amount_removed: format_scaled(removed, currency),
            running_total_scaled: running_total,
            running_total: format_scaled(running_total, currency),
            skipped_reason: None,
        });
        if rule.exclusive || !rule.stackable {
            stopped = true;
        }
    }
    Ok(DiscountEvaluation {
        subtotal_scaled: subtotal,
        subtotal: format_scaled(subtotal, currency),
        total_discount_scaled: total_discount,
        total_discount: format_scaled(total_discount, currency),
        total_scaled: running_total,
        total: format_scaled(running_total, currency),
        currency: currency.to_string(),
        trace,
    })
}
