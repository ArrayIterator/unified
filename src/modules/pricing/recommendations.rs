// pricing — recommendations.

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
use super::discounts::*;
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

/// One labelled step of a calculation, so an admin can audit the
/// math line by line. `amount_scaled` is signed: credits are
/// negative, charges are positive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalculationLine {
    pub label: String,
    pub amount_scaled: i64,
    pub currency: String,
    /// Exact decimal string of the amount.
    pub amount: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl CalculationLine {
    pub(crate) fn new(label: &str, amount_scaled: i64, currency: &str, detail: Option<String>) -> Self {
        Self {
            label: label.to_string(),
            amount_scaled,
            currency: currency.to_string(),
            amount: format_scaled(amount_scaled, currency),
            detail,
        }
    }
}

// ---------------------------------------------------------------------------
// Pure pricing engines (no database, no I/O; unit-tested below).
// ---------------------------------------------------------------------------

/// One cost component of a cost-plus recommendation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostLineInput {
    pub label: String,
    /// Where the cost figure came from, for example a supplier
    /// invoice number or a cost table name. Never guessed.
    pub source: String,
    pub cost: MoneyInput,
}

/// Result of a cost-plus recommendation: total cost, margin
/// amount, and suggested price, with every step shown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostPlusResult {
    pub total_cost_scaled: i64,
    pub total_cost: String,
    pub margin_bp: i64,
    pub margin_amount_scaled: i64,
    pub margin_amount: String,
    pub suggested_price_scaled: i64,
    pub suggested_price: String,
    pub currency: String,
    pub lines: Vec<CalculationLine>,
}

/// Cost-plus: suggested price = total cost + margin, where the
/// margin is `margin_bp` basis points of the total cost. The margin
/// is rounded half-up to the minor unit, then added to the rounded
/// total cost. Returns an error when the cost list is empty or the
/// currencies differ.
pub fn cost_plus_recommendation(
    costs: &[CostLineInput],
    margin_bp: i64,
) -> UnifiedResult<CostPlusResult> {
    if costs.is_empty() {
        return Err(UnifiedError::bad_request(
            "At least one cost line is required",
        ));
    }
    if !(0..=BASIS_POINTS).contains(&margin_bp) {
        return Err(UnifiedError::bad_request(
            "Margin must be between 0 and 10000 basis points (0% to 100%)",
        ));
    }
    let mut currency: Option<String> = None;
    let mut lines: Vec<CalculationLine> = Vec::new();
    let mut total_cost: i64 = 0;
    for cost in costs {
        let label = cost.label.trim();
        if label.is_empty() {
            return Err(UnifiedError::bad_request("Cost line label must not be empty"));
        }
        if cost.source.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Cost line source must not be empty",
            ));
        }
        let money = resolve_money(&cost.cost, currency.as_deref())?;
        match &currency {
            None => currency = Some(money.currency.clone()),
            Some(expected) if *expected != money.currency => {
                return Err(UnifiedError::bad_request(
                    "All cost lines must use the same currency",
                ));
            }
            _ => {}
        }
        let rounded = round_to_minor(money.amount_scaled, &money.currency);
        total_cost = total_cost
            .checked_add(rounded)
            .ok_or_else(|| UnifiedError::bad_request("Total cost is too large"))?;
        lines.push(CalculationLine::new(
            &format!("Cost: {label}"),
            rounded,
            &money.currency,
            Some(format!("Source: {}", cost.source.trim())),
        ));
    }
    let currency = currency.unwrap_or_else(|| DEFAULT_CURRENCY.to_string());
    let margin_amount = round_to_minor(percent_of(total_cost, margin_bp)?, &currency);
    let suggested_price = total_cost
        .checked_add(margin_amount)
        .ok_or_else(|| UnifiedError::bad_request("Suggested price is too large"))?;
    lines.push(CalculationLine::new(
        "Total cost",
        total_cost,
        &currency,
        None,
    ));
    lines.push(CalculationLine::new(
        &format!("Margin ({} basis points)", margin_bp),
        margin_amount,
        &currency,
        Some(format!(
            "Margin target: {}.{}%",
            margin_bp / 100,
            margin_bp % 100
        )),
    ));
    lines.push(CalculationLine::new(
        "Suggested price",
        suggested_price,
        &currency,
        None,
    ));
    Ok(CostPlusResult {
        total_cost_scaled: total_cost,
        total_cost: format_scaled(total_cost, &currency),
        margin_bp,
        margin_amount_scaled: margin_amount,
        margin_amount: format_scaled(margin_amount, &currency),
        suggested_price_scaled: suggested_price,
        suggested_price: format_scaled(suggested_price, &currency),
        currency,
        lines,
    })
}

/// One tier of a tiered price: quantities from `min_quantity` up
/// to `max_quantity` (inclusive; `None` means no upper bound) are
/// charged at `unit_price` each.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierInput {
    pub min_quantity: u32,
    #[serde(default)]
    pub max_quantity: Option<u32>,
    pub unit_price: MoneyInput,
}

/// Result of pricing a quantity against tiers, with the per-tier
/// breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TieredPriceResult {
    pub quantity: u32,
    pub total_scaled: i64,
    pub total: String,
    pub currency: String,
    pub lines: Vec<CalculationLine>,
}

/// Tiered pricing: walk the tiers in ascending `min_quantity`
/// order, allocate the quantity to each tier in turn, and price
/// each tier's units at that tier's unit price (rounded half-up to
/// the minor unit per tier line, then summed). Tiers must start at
/// 1, must be contiguous (each tier starts exactly one above the
/// previous tier maximum: no gaps, no overlaps), no tier may
/// follow an unbounded tier, and all tiers must share one
/// currency. A quantity above the highest bounded tier is an error
/// unless an unbounded tier covers it.
pub fn tiered_price(tiers: &[TierInput], quantity: u32) -> UnifiedResult<TieredPriceResult> {
    if tiers.is_empty() {
        return Err(UnifiedError::bad_request("At least one tier is required"));
    }
    if quantity == 0 {
        return Err(UnifiedError::bad_request("Quantity must be at least 1"));
    }
    let mut ordered: Vec<&TierInput> = tiers.iter().collect();
    ordered.sort_by_key(|tier| tier.min_quantity);
    if ordered[0].min_quantity != 1 {
        return Err(UnifiedError::bad_request(
            "The first tier must start at quantity 1",
        ));
    }
    let mut currency: Option<String> = None;
    let mut previous_max: Option<u32> = None;
    let mut has_previous = false;
    for tier in &ordered {
        if tier.min_quantity == 0 {
            return Err(UnifiedError::bad_request(
                "Tier minimum quantity must be at least 1",
            ));
        }
        if let Some(max) = tier.max_quantity {
            if max < tier.min_quantity {
                return Err(UnifiedError::bad_request(
                    "Tier maximum quantity must not be below its minimum",
                ));
            }
        }
        if has_previous {
            match previous_max {
                // The previous tier has no upper bound, so no
                // later tier can ever apply: anything after it
                // is unreachable and must be rejected.
                None => {
                    return Err(UnifiedError::bad_request(
                        "No tier may follow an unbounded tier",
                    ));
                }
                Some(prev) => {
                    let expected_min = prev.checked_add(1).ok_or_else(|| {
                        UnifiedError::bad_request(
                            "No tier may follow a tier ending at the maximum quantity",
                        )
                    })?;
                    if tier.min_quantity < expected_min {
                        return Err(UnifiedError::bad_request(
                            "Tiers must not overlap: each tier must start above the previous tier maximum",
                        ));
                    }
                    if tier.min_quantity > expected_min {
                        return Err(UnifiedError::bad_request(
                            "Tiers must be contiguous: each tier must start exactly one above the previous tier maximum, with no gap",
                        ));
                    }
                }
            }
        }
        has_previous = true;
        previous_max = tier.max_quantity;
        let money = resolve_money(&tier.unit_price, currency.as_deref())?;
        match &currency {
            None => currency = Some(money.currency.clone()),
            Some(expected) if *expected != money.currency => {
                return Err(UnifiedError::bad_request(
                    "All tiers must use the same currency",
                ));
            }
            _ => {}
        }
    }
    let currency = currency.unwrap_or_else(|| DEFAULT_CURRENCY.to_string());
    let mut remaining = quantity;
    let mut total: i64 = 0;
    let mut lines: Vec<CalculationLine> = Vec::new();
    for tier in &ordered {
        if remaining == 0 {
            break;
        }
        // Only units inside this tier's own range
        // [min_quantity, max_quantity] are priced here. Units
        // already allocated to earlier tiers are below
        // min_quantity, and units above max_quantity belong to a
        // later tier; neither is ever charged at this tier's rate.
        let consumed = quantity - remaining;
        let first_unit = consumed + 1;
        if first_unit < tier.min_quantity {
            return Err(UnifiedError::bad_request(
                "Quantity falls in a gap between tiers: no tier covers it",
            ));
        }
        let last_unit = match tier.max_quantity {
            Some(max) => quantity.min(max),
            None => quantity,
        };
        if last_unit < first_unit {
            continue;
        }
        let units = last_unit - first_unit + 1;
        let unit = resolve_money(&tier.unit_price, Some(&currency))?;
        let unit_rounded = round_to_minor(unit.amount_scaled, &currency);
        let line_amount = round_to_minor(
            unit_rounded
                .checked_mul(i64::from(units))
                .ok_or_else(|| UnifiedError::bad_request("Tier amount is too large"))?,
            &currency,
        );
        total = total
            .checked_add(line_amount)
            .ok_or_else(|| UnifiedError::bad_request("Tiered total is too large"))?;
        let range = match tier.max_quantity {
            Some(max) => format!("units {}-{} ({} units)", tier.min_quantity, max, units),
            None => format!("units {}+ ({} units)", tier.min_quantity, units),
        };
        lines.push(CalculationLine::new(
            &format!("Tier {} at {} per unit", range, unit.amount),
            line_amount,
            &currency,
            Some(format!("{} units x {}", units, unit.amount)),
        ));
        remaining -= units;
    }
    if remaining > 0 {
        return Err(UnifiedError::bad_request(
            "Quantity exceeds the highest tier: add an unbounded tier to cover it",
        ));
    }
    lines.push(CalculationLine::new("Total", total, &currency, None));
    Ok(TieredPriceResult {
        quantity,
        total_scaled: total,
        total: format_scaled(total, &currency),
        currency,
        lines,
    })
}

/// One component of a bundle: a product reference and its
/// standalone price.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleComponentInput {
    pub product_id: String,
    pub label: String,
    pub price: MoneyInput,
}

/// A bundle discount: either a percentage (basis points) off the
/// subtotal, or a fixed amount off.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleDiscountInput {
    /// `percent` or `fixed`.
    pub kind: String,
    /// Basis points when `kind` is `percent`; ignored otherwise.
    #[serde(default)]
    pub percent_bp: i64,
    /// Fixed amount when `kind` is `fixed`; ignored otherwise.
    #[serde(default)]
    pub amount: Option<MoneyInput>,
}

/// Result of pricing a bundle, with the line-by-line breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundlePriceResult {
    pub subtotal_scaled: i64,
    pub subtotal: String,
    pub discount_scaled: i64,
    pub discount: String,
    pub total_scaled: i64,
    pub total: String,
    pub currency: String,
    pub lines: Vec<CalculationLine>,
}

/// Bundle pricing: subtotal is the exact sum of the rounded
/// component prices; the discount is a percentage of the subtotal
/// (rounded half-up to the minor unit) or a fixed amount capped at
/// the subtotal; the total is subtotal minus discount.
pub fn bundle_price(
    components: &[BundleComponentInput],
    discount: &BundleDiscountInput,
) -> UnifiedResult<BundlePriceResult> {
    if components.is_empty() {
        return Err(UnifiedError::bad_request(
            "A bundle needs at least one component",
        ));
    }
    let mut currency: Option<String> = None;
    let mut lines: Vec<CalculationLine> = Vec::new();
    let mut subtotal: i64 = 0;
    for component in components {
        if component.product_id.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Bundle component product_id must not be empty",
            ));
        }
        let money = resolve_money(&component.price, currency.as_deref())?;
        match &currency {
            None => currency = Some(money.currency.clone()),
            Some(expected) if *expected != money.currency => {
                return Err(UnifiedError::bad_request(
                    "All bundle components must use the same currency",
                ));
            }
            _ => {}
        }
        let rounded = round_to_minor(money.amount_scaled, &money.currency);
        subtotal = subtotal
            .checked_add(rounded)
            .ok_or_else(|| UnifiedError::bad_request("Bundle subtotal is too large"))?;
        lines.push(CalculationLine::new(
            &format!(
                "Component: {}",
                if component.label.trim().is_empty() {
                    component.product_id.trim()
                } else {
                    component.label.trim()
                }
            ),
            rounded,
            &money.currency,
            Some(format!("Product: {}", component.product_id.trim())),
        ));
    }
    let currency = currency.unwrap_or_else(|| DEFAULT_CURRENCY.to_string());
    lines.push(CalculationLine::new("Subtotal", subtotal, &currency, None));
    let discount_amount = match discount.kind.as_str() {
        "percent" => {
            if !(0..=BASIS_POINTS).contains(&discount.percent_bp) {
                return Err(UnifiedError::bad_request(
                    "Bundle discount percent must be between 0 and 10000 basis points",
                ));
            }
            round_to_minor(percent_of(subtotal, discount.percent_bp)?, &currency)
        }
        "fixed" => {
            let fixed = match &discount.amount {
                Some(amount) => resolve_money(amount, Some(&currency))?,
                None => {
                    return Err(UnifiedError::bad_request(
                        "A fixed bundle discount needs an amount",
                    ))
                }
            };
            if fixed.currency != currency {
                return Err(UnifiedError::bad_request(
                    "Bundle discount currency must match the component currency",
                ));
            }
            round_to_minor(fixed.amount_scaled, &currency).min(subtotal)
        }
        _ => {
            return Err(UnifiedError::bad_request(
                "Bundle discount kind must be \"percent\" or \"fixed\"",
            ))
        }
    };
    let total = subtotal - discount_amount;
    lines.push(CalculationLine::new(
        "Bundle discount",
        -discount_amount,
        &currency,
        None,
    ));
    lines.push(CalculationLine::new("Bundle price", total, &currency, None));
    Ok(BundlePriceResult {
        subtotal_scaled: subtotal,
        subtotal: format_scaled(subtotal, &currency),
        discount_scaled: discount_amount,
        discount: format_scaled(discount_amount, &currency),
        total_scaled: total,
        total: format_scaled(total, &currency),
        currency,
        lines,
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    fn tier(min_quantity: u32, max_quantity: Option<u32>, amount: &str) -> TierInput {
        TierInput {
            min_quantity,
            max_quantity,
            unit_price: MoneyInput {
                amount: amount.to_string(),
                currency: Some("USD".to_string()),
            },
        }
    }

    #[test]
    fn tiered_price_rejects_gaps_and_tiers_after_an_unbounded_tier() {
        // Gap at quantity 11: it must error instead of silently
        // pricing unit 11 at the 8.00 tier rate.
        let gapped = vec![tier(1, Some(10), "10.00"), tier(12, Some(20), "8.00")];
        assert!(tiered_price(&gapped, 11).is_err());
        assert!(tiered_price(&gapped, 15).is_err());
        // The tier set is invalid whatever the quantity is, even
        // when the quantity never reaches the gap.
        assert!(tiered_price(&gapped, 5).is_err());
        // A tier after an unbounded tier can never apply.
        let after_unbounded = vec![
            tier(1, Some(10), "10.00"),
            tier(11, None, "8.00"),
            tier(21, Some(30), "6.00"),
        ];
        assert!(tiered_price(&after_unbounded, 25).is_err());
        // Contiguous tiers still price exactly as before.
        let contiguous = vec![tier(1, Some(10), "10.00"), tier(11, None, "8.00")];
        assert_eq!(tiered_price(&contiguous, 15).unwrap().total_scaled, 1_400_000);
        assert_eq!(tiered_price(&contiguous, 11).unwrap().total_scaled, 1_080_000);
    }
}
