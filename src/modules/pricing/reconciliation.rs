// pricing - reconciliation.

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

// Invoice reconciliation (pure).

/// One invoice line as the reconciler sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileLineInput {
    pub name: String,
    pub quantity: u32,
    pub unit_price_scaled: i64,
    /// Tax rate in basis points (1,000 = 10.00%).
    #[serde(default)]
    pub tax_bp: i64,
    /// When true the unit price already includes tax.
    #[serde(default)]
    pub tax_inclusive: bool,
}

/// One signed adjustment line: a discount (negative) or a
/// proration line (positive charge, negative credit).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileAdjustmentInput {
    pub label: String,
    pub amount_scaled: i64,
}

/// The invoice figures the caller claims, against which the engine
/// recomputes every component.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileClaim {
    pub subtotal_scaled: i64,
    pub discounts_scaled: i64,
    pub proration_scaled: i64,
    pub tax_total_scaled: i64,
    pub total_scaled: i64,
}

/// One component of the reconciliation comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationComponent {
    pub component: String,
    pub expected_scaled: i64,
    pub expected: String,
    pub claimed_scaled: i64,
    pub claimed: String,
    pub discrepancy_scaled: i64,
    pub discrepancy: String,
    pub matches: bool,
}

/// Result of reconciling an invoice before it is issued.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationResult {
    pub matched: bool,
    pub currency: String,
    pub components: Vec<ReconciliationComponent>,
    /// Total discrepancy: claimed total minus expected total.
    pub total_discrepancy_scaled: i64,
    pub total_discrepancy: String,
    pub lines: Vec<CalculationLine>,
}

/// Split one gross or net line amount into (net, tax), both
/// rounded half-up to the minor unit. Tax-exclusive: tax is the
/// rate applied to the net amount. Tax-inclusive: the net amount
/// is the gross divided by (1 + rate), rounded half-up, and tax is
/// the remainder, so net + tax always equals the gross exactly.
pub(crate) fn split_line_tax(
    line_amount: i64,
    tax_bp: i64,
    tax_inclusive: bool,
    currency: &str,
) -> UnifiedResult<(i64, i64)> {
    if !(0..=BASIS_POINTS).contains(&tax_bp) {
        return Err(UnifiedError::bad_request(
            "Tax rate must be between 0 and 10000 basis points",
        ));
    }
    if tax_inclusive {
        let net = round_to_minor(
            mul_div_half_up(line_amount, BASIS_POINTS, BASIS_POINTS + tax_bp)?,
            currency,
        );
        Ok((net, line_amount - net))
    } else {
        let tax = round_to_minor(percent_of(line_amount, tax_bp)?, currency);
        Ok((line_amount, tax))
    }
}

/// Reconcile an invoice: recompute every component from the lines
/// and adjustments with integer math, then compare with the claimed
/// figures component by component. The expected total is
/// `lines subtotal - discounts + proration + tax`. A match on every
/// component is required; a match on the total alone is not
/// enough, because equal and opposite errors could cancel out.
pub fn reconcile_invoice(
    lines: &[ReconcileLineInput],
    discounts: &[ReconcileAdjustmentInput],
    proration_lines: &[ReconcileAdjustmentInput],
    claim: &ReconcileClaim,
    currency: &str,
) -> UnifiedResult<ReconciliationResult> {
    if lines.is_empty() {
        return Err(UnifiedError::bad_request(
            "At least one invoice line is required",
        ));
    }
    validate_currency(currency)?;
    let mut expected_subtotal: i64 = 0;
    let mut expected_tax: i64 = 0;
    let mut calc_lines: Vec<CalculationLine> = Vec::new();
    for line in lines {
        if line.name.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Invoice line name must not be empty",
            ));
        }
        if line.unit_price_scaled < 0 {
            return Err(UnifiedError::bad_request(
                "Invoice line unit price must not be negative",
            ));
        }
        if line.quantity == 0 {
            return Err(UnifiedError::bad_request(
                "Invoice line quantity must be at least 1",
            ));
        }
        let gross = round_to_minor(
            line.unit_price_scaled
                .checked_mul(i64::from(line.quantity))
                .ok_or_else(|| UnifiedError::bad_request("Line amount is too large"))?,
            currency,
        );
        let (net, tax) = split_line_tax(gross, line.tax_bp, line.tax_inclusive, currency)?;
        expected_subtotal = expected_subtotal
            .checked_add(net)
            .ok_or_else(|| UnifiedError::bad_request("Subtotal is too large"))?;
        expected_tax = expected_tax
            .checked_add(tax)
            .ok_or_else(|| UnifiedError::bad_request("Tax total is too large"))?;
        calc_lines.push(CalculationLine::new(
            &format!("Line: {} ({} x {})", line.name.trim(), line.quantity, format_scaled(line.unit_price_scaled, currency)),
            net,
            currency,
            Some(format!(
                "Tax {} basis points, {}",
                line.tax_bp,
                if line.tax_inclusive { "tax-inclusive" } else { "tax-exclusive" }
            )),
        ));
    }
    let mut expected_discounts: i64 = 0;
    for discount in discounts {
        if discount.amount_scaled < 0 {
            return Err(UnifiedError::bad_request(
                "Discount amounts must not be negative",
            ));
        }
        expected_discounts = expected_discounts
            .checked_add(discount.amount_scaled)
            .ok_or_else(|| UnifiedError::bad_request("Discount total is too large"))?;
        calc_lines.push(CalculationLine::new(
            &format!("Discount: {}", discount.label),
            -discount.amount_scaled,
            currency,
            None,
        ));
    }
    if expected_discounts > expected_subtotal {
        return Err(UnifiedError::bad_request(
            "Discounts must not exceed the line subtotal",
        ));
    }
    let mut expected_proration: i64 = 0;
    for proration in proration_lines {
        expected_proration = expected_proration
            .checked_add(proration.amount_scaled)
            .ok_or_else(|| UnifiedError::bad_request("Proration total is too large"))?;
        calc_lines.push(CalculationLine::new(
            &format!("Proration: {}", proration.label),
            proration.amount_scaled,
            currency,
            None,
        ));
    }
    let expected_total = expected_subtotal
        .checked_sub(expected_discounts)
        .and_then(|value| value.checked_add(expected_proration))
        .and_then(|value| value.checked_add(expected_tax))
        .ok_or_else(|| UnifiedError::bad_request("Invoice total is too large"))?;
    calc_lines.push(CalculationLine::new(
        "Expected total",
        expected_total,
        currency,
        None,
    ));
    let component = |name: &str, expected: i64, claimed: i64| ReconciliationComponent {
        component: name.to_string(),
        expected_scaled: expected,
        expected: format_scaled(expected, currency),
        claimed_scaled: claimed,
        claimed: format_scaled(claimed, currency),
        discrepancy_scaled: claimed - expected,
        discrepancy: format_scaled(claimed - expected, currency),
        matches: claimed == expected,
    };
    let components = vec![
        component("subtotal", expected_subtotal, claim.subtotal_scaled),
        component("discounts", expected_discounts, claim.discounts_scaled),
        component("proration", expected_proration, claim.proration_scaled),
        component("tax_total", expected_tax, claim.tax_total_scaled),
        component("total", expected_total, claim.total_scaled),
    ];
    let matched = components.iter().all(|item| item.matches);
    Ok(ReconciliationResult {
        matched,
        currency: currency.to_string(),
        components,
        total_discrepancy_scaled: claim.total_scaled - expected_total,
        total_discrepancy: format_scaled(claim.total_scaled - expected_total, currency),
        lines: calc_lines,
    })
}

/// Reconcile a credit note: the sum of its lines must equal the
/// credit amount exactly. A credit note is a correction, so its
/// lines are non-negative amounts that add up to the credit.
pub fn reconcile_credit_note(
    lines: &[ReconcileAdjustmentInput],
    credit_amount_scaled: i64,
    currency: &str,
) -> UnifiedResult<ReconciliationResult> {
    if lines.is_empty() {
        return Err(UnifiedError::bad_request(
            "A credit note needs at least one line",
        ));
    }
    validate_currency(currency)?;
    if credit_amount_scaled < 0 {
        return Err(UnifiedError::bad_request(
            "Credit note amount must not be negative",
        ));
    }
    let mut expected: i64 = 0;
    let mut calc_lines: Vec<CalculationLine> = Vec::new();
    for line in lines {
        if line.amount_scaled < 0 {
            return Err(UnifiedError::bad_request(
                "Credit note line amounts must not be negative",
            ));
        }
        expected = expected
            .checked_add(line.amount_scaled)
            .ok_or_else(|| UnifiedError::bad_request("Credit total is too large"))?;
        calc_lines.push(CalculationLine::new(
            &format!("Credit line: {}", line.label),
            line.amount_scaled,
            currency,
            None,
        ));
    }
    let component = ReconciliationComponent {
        component: "credit_total".to_string(),
        expected_scaled: expected,
        expected: format_scaled(expected, currency),
        claimed_scaled: credit_amount_scaled,
        claimed: format_scaled(credit_amount_scaled, currency),
        discrepancy_scaled: credit_amount_scaled - expected,
        discrepancy: format_scaled(credit_amount_scaled - expected, currency),
        matches: credit_amount_scaled == expected,
    };
    let matched = component.matches;
    calc_lines.push(CalculationLine::new(
        "Credit total",
        expected,
        currency,
        None,
    ));
    Ok(ReconciliationResult {
        matched,
        currency: currency.to_string(),
        components: vec![component],
        total_discrepancy_scaled: credit_amount_scaled - expected,
        total_discrepancy: format_scaled(credit_amount_scaled - expected, currency),
        lines: calc_lines,
    })
}

/// Change between an old and a new price, in basis points of the
/// old price. When the old price is zero and the new price is
/// above zero the change is treated as unbounded (always needs a
/// second approver); when both are zero there is no change.
pub fn price_change_bp(old_price_scaled: i64, new_price_scaled: i64) -> Option<i64> {
    if old_price_scaled == 0 {
        if new_price_scaled == 0 {
            Some(0)
        } else {
            None
        }
    } else {
        let difference = (new_price_scaled - old_price_scaled).abs();
        Some(mul_div_half_up_unchecked(
            difference,
            BASIS_POINTS,
            old_price_scaled,
        ))
    }
}

/// True when a price change needs a second approver: the change
/// exceeds the configured threshold, or the change cannot be
/// measured against a zero old price.
pub fn needs_second_approver(
    old_price_scaled: i64,
    new_price_scaled: i64,
    threshold_bp: i64,
) -> bool {
    match price_change_bp(old_price_scaled, new_price_scaled) {
        Some(change) => change > threshold_bp,
        None => true,
    }
}

/// Convert an amount between currencies with a stored integer
/// ratio (`rate_numerator / rate_denominator`). The result is
/// rounded half-up to the target currency minor unit. A missing
/// rate is an error, never a guessed conversion.
pub fn convert_currency(
    amount_scaled: i64,
    rate_numerator: i64,
    rate_denominator: i64,
    target_currency: &str,
) -> UnifiedResult<i64> {
    if rate_numerator <= 0 || rate_denominator <= 0 {
        return Err(UnifiedError::bad_request(
            "A stored exchange rate needs a positive numerator and denominator",
        ));
    }
    validate_currency(target_currency)?;
    Ok(round_to_minor(
        mul_div_half_up(amount_scaled, rate_numerator, rate_denominator)?,
        target_currency,
    ))
}

/// Summarize human-entered market reference prices for one
/// product: count, lowest, highest, and median, all in scaled
/// units. The median of an even count is the half-up average of
/// the two middle values. References are data, never scraped.
pub fn summarize_references(prices_scaled: &[i64]) -> Option<(i64, i64, i64, usize)> {
    if prices_scaled.is_empty() {
        return None;
    }
    let mut ordered: Vec<i64> = prices_scaled.to_vec();
    ordered.sort_unstable();
    let count = ordered.len();
    let lowest = ordered[0];
    let highest = ordered[count - 1];
    let median = if count % 2 == 1 {
        ordered[count / 2]
    } else {
        let pair = ordered[count / 2 - 1] + ordered[count / 2];
        // Half-up average of the two middle values.
        if pair >= 0 {
            (pair + 1) / 2
        } else {
            (pair - 1) / 2
        }
    };
    Some((lowest, highest, median, count))
}
