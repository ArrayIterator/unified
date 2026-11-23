// Tax module — pure tax computation.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Pure computation
// ---------------------------------------------------------------------------

/// Round a rate to 4 decimal places. Rates stay `f64`; money no
/// longer flows through here — amounts are integer minor units
/// (see `major_to_minor` / `minor_to_major`), so the old
/// money-to-4-decimals rounding is gone by design: sub-cent
/// precision is dropped at the boundary, half away from zero,
/// exactly like the billing and store modules.
pub(crate) fn round_tax(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// True for zero-decimal currencies (no minor units), matching the
/// billing and store modules' lists.
/// True for zero-decimal currencies (no minor units). Delegates to the
/// central ISO 4217 catalogue via `unified_core::db::money`: a currency
/// is zero-decimal exactly when the catalogue lists it with zero minor
/// digits. No local list is kept here.
pub(crate) fn is_zero_decimal_currency(currency: &str) -> bool {
    unified_core::db::money::is_zero_decimal_currency(currency)
}

/// Convert a major-unit amount to integer minor units, rounding
/// half away from zero — the same rounding the old `round_tax`
/// money math applied in cents.
pub(crate) fn major_to_minor(amount_major: f64, currency: &str) -> i64 {
    if is_zero_decimal_currency(currency) {
        amount_major.round() as i64
    } else {
        (amount_major * 100.0).round() as i64
    }
}

/// Convert integer minor units to major units, as JSON and hook
/// payloads carry them. This is the boundary conversion out of the
/// integer representation; business logic keeps minor units.
pub(crate) fn minor_to_major(amount_minor: i64, currency: &str) -> f64 {
    if is_zero_decimal_currency(currency) {
        amount_minor as f64
    } else {
        ((amount_minor as f64 / 100.0) * 100.0).round() / 100.0
    }
}

/// Apply a ratio (a tax rate as a fraction) to a minor-unit
/// amount, rounding half away from zero to minor units. The
/// multiplication itself stays float; the result is rounded
/// exactly once to minor units, which is the same rounding the
/// old `round_tax` float math applied (`round(base * rate)` in
/// cents equals `round(base_minor * rate)` in minor units).
pub(crate) fn apply_ratio_to_minor(amount_minor: i64, ratio: f64) -> i64 {
    (amount_minor as f64 * ratio).round() as i64
}

/// Divide a minor-unit amount by a divisor, rounding half away
/// from zero to minor units. Used for the tax-inclusive split,
/// where the net is `amount / (1 + rate)`.
pub(crate) fn div_ratio_to_minor(amount_minor: i64, divisor: f64) -> i64 {
    (amount_minor as f64 / divisor).round() as i64
}

/// Pure tax computation over (`amount`, rules, `on`). Amounts
/// are integer minor units; rates stay `f64` fractions.
///
/// This function is public, so it must be safe to call directly:
/// it filters the rules itself — disabled rules never contribute,
/// and a rule whose `threshold_amount` the amount does not meet
/// never contributes — and it runs the rules in `sort_order` (then
/// creation) order so compound taxes stack deterministically.
/// Callers normally pass `applicable_rules` output, which is
/// already filtered and sorted; filtering again here is what makes
/// a direct call safe.
///
/// Exclusive rules add tax on top of the amount; a compound
/// exclusive rule is computed on the amount plus earlier exclusive
/// taxes. Inclusive rules extract the tax already inside the amount:
/// `tax = gross - gross / (1 + rate)`. A compound flag on an
/// inclusive rule has no effect — the line reports
/// `compound_applied: false` — because extracting twice from the
/// same gross amount would double-count the tax.
pub fn compute_tax_breakdown(amount: i64, rules: &[TaxRule], on: NaiveDate) -> TaxBreakdown {
    let mut rules: Vec<&TaxRule> = rules
        .iter()
        .filter(|rule| {
            rule.enabled
                && rule
                    .threshold_amount
                    .is_none_or(|threshold| amount >= threshold)
        })
        .collect();
    rules.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then(a.created_at.cmp(&b.created_at))
    });
    let mut lines: Vec<TaxLine> = Vec::new();
    let mut exclusive_tax_total: i64 = 0;
    let mut inclusive_tax_total: i64 = 0;
    for rule in rules {
        if rule.effective_from > on || rule.effective_to.is_some_and(|until| on > until) {
            continue;
        }
        let rate = rule.rate_percent / 100.0;
        if rule.inclusive {
            let tax = amount - div_ratio_to_minor(amount, 1.0 + rate);
            inclusive_tax_total += tax;
            lines.push(TaxLine {
                rule_id: rule.id.clone(),
                rule_name: rule.name.clone(),
                tax_type: rule.tax_type.clone(),
                rate_percent: rule.rate_percent,
                base: amount,
                amount: tax,
                inclusive: true,
                compound_applied: false,
            });
        } else {
            let compound_applied = rule.compound;
            let base = if compound_applied {
                amount + exclusive_tax_total
            } else {
                amount
            };
            let tax = apply_ratio_to_minor(base, rate);
            exclusive_tax_total += tax;
            lines.push(TaxLine {
                rule_id: rule.id.clone(),
                rule_name: rule.name.clone(),
                tax_type: rule.tax_type.clone(),
                rate_percent: rule.rate_percent,
                base,
                amount: tax,
                inclusive: false,
                compound_applied,
            });
        }
    }
    let tax_total = exclusive_tax_total + inclusive_tax_total;
    let has_inclusive = lines.iter().any(|line| line.inclusive);
    let grand_total = if has_inclusive && exclusive_tax_total == 0 {
        amount
    } else if has_inclusive {
        amount + exclusive_tax_total
    } else {
        amount + tax_total
    };
    TaxBreakdown {
        base: amount,
        lines,
        tax_total,
        grand_total,
        currency: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(rate: f64, inclusive: bool, compound: bool) -> TaxRule {
        rule_effective(
            rate,
            inclusive,
            compound,
            NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            None,
        )
    }

    fn rule_effective(
        rate: f64,
        inclusive: bool,
        compound: bool,
        from: NaiveDate,
        to: Option<NaiveDate>,
    ) -> TaxRule {
        TaxRule {
            id: "rule-1".to_string(),
            company_id: Uuid::nil(),
            country_code: "ID".to_string(),
            region_code: None,
            name: "VAT".to_string(),
            tax_type: "vat".to_string(),
            rate_percent: rate,
            threshold_amount: None,
            currency: "IDR".to_string(),
            inclusive,
            compound,
            effective_from: from,
            effective_to: to,
            sort_order: 0,
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn on() -> NaiveDate {
        NaiveDate::from_ymd_opt(2025, 6, 1).unwrap()
    }

    #[test]
    fn exclusive_rule_adds_tax_on_top() {
        let result = compute_tax_breakdown(10000, &[rule(10.0, false, false)], on());
        assert_eq!(result.tax_total, 1000);
        assert_eq!(result.grand_total, 11000);
        assert_eq!(result.lines.len(), 1);
        assert!(!result.lines[0].inclusive);
    }

    #[test]
    fn inclusive_rule_extracts_tax_from_gross() {
        let result = compute_tax_breakdown(11000, &[rule(10.0, true, false)], on());
        assert_eq!(result.tax_total, 1000);
        assert_eq!(result.grand_total, 11000);
        assert!(result.lines[0].inclusive);
        assert!(!result.lines[0].compound_applied);
    }

    #[test]
    fn compound_rule_taxes_earlier_tax_too() {
        let rules = vec![rule(10.0, false, false), rule(10.0, false, true)];
        let result = compute_tax_breakdown(10000, &rules, on());
        assert_eq!(result.lines[0].amount, 1000);
        assert_eq!(result.lines[1].base, 11000);
        assert_eq!(result.lines[1].amount, 1100);
        assert_eq!(result.tax_total, 2100);
        assert_eq!(result.grand_total, 12100);
    }

    #[test]
    fn rule_outside_its_effective_dates_is_skipped() {
        let future = rule_effective(
            10.0,
            false,
            false,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            None,
        );
        let expired = rule_effective(
            10.0,
            false,
            false,
            NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            Some(NaiveDate::from_ymd_opt(2024, 12, 31).unwrap()),
        );
        let result = compute_tax_breakdown(10000, &[future, expired], on());
        assert!(result.lines.is_empty());
        assert_eq!(result.tax_total, 0);
        assert_eq!(result.grand_total, 10000);
    }

    #[test]
    fn compute_tax_breakdown_filters_disabled_and_below_threshold_rules() {
        // Regression: the public function used to apply any rule it
        // was handed, including disabled rules and rules whose
        // threshold the amount did not meet.
        let mut disabled = rule(10.0, false, false);
        disabled.enabled = false;
        let mut below_threshold = rule(10.0, false, false);
        below_threshold.id = "rule-2".to_string();
        below_threshold.threshold_amount = Some(50000);
        let mut at_threshold = rule(10.0, false, false);
        at_threshold.id = "rule-3".to_string();
        at_threshold.threshold_amount = Some(10000);
        let result = compute_tax_breakdown(10000, &[disabled, below_threshold, at_threshold], on());
        assert_eq!(result.lines.len(), 1);
        assert_eq!(result.lines[0].rule_id, "rule-3");
        assert_eq!(result.tax_total, 1000);
    }

    #[test]
    fn compute_tax_breakdown_orders_rules_by_sort_order() {
        // A compound rule handed first must still run in sort_order:
        // sort_order 0 (compound) runs before sort_order 1, so it
        // has no earlier tax to stack on and both lines base 10000.
        let mut first = rule(10.0, false, false);
        first.sort_order = 1;
        let mut second = rule(10.0, false, true);
        second.id = "rule-2".to_string();
        second.sort_order = 0;
        let result = compute_tax_breakdown(10000, &[first, second], on());
        assert_eq!(result.lines[0].rule_id, "rule-2");
        assert_eq!(result.lines[0].base, 10000);
        assert_eq!(result.lines[1].rule_id, "rule-1");
        assert_eq!(result.lines[1].base, 10000);
        assert_eq!(result.tax_total, 2000);
    }

    #[test]
    fn inclusive_split_rounds_half_away_from_zero_to_minor_units() {
        // Gross 100.05 at 10% inclusive: net rounds to 9095 minor,
        // so the extracted tax is 910 minor.
        let result = compute_tax_breakdown(10005, &[rule(10.0, true, false)], on());
        assert_eq!(result.lines[0].base, 10005);
        assert_eq!(result.lines[0].amount, 910);
        assert_eq!(result.tax_total, 910);
        assert_eq!(result.grand_total, 10005);
    }

    #[test]
    fn ratio_helpers_round_half_away_from_zero() {
        // 105 minor at 10% is 10.5 minor: the old float math
        // rounded 0.105 major to 0.11, i.e. 11 minor units.
        assert_eq!(apply_ratio_to_minor(105, 0.10), 11);
        assert_eq!(apply_ratio_to_minor(-105, 0.10), -11);
        assert_eq!(apply_ratio_to_minor(10000, 0.10), 1000);
        // 110.00 gross at 10% inclusive nets exactly 100.00.
        assert_eq!(div_ratio_to_minor(11000, 1.1), 10000);
    }

    #[test]
    fn major_to_minor_and_minor_to_major_are_currency_aware() {
        assert_eq!(major_to_minor(10.005, "USD"), 1001);
        assert_eq!(major_to_minor(10.004, "USD"), 1000);
        assert_eq!(major_to_minor(500.0, "JPY"), 500);
        assert_eq!(minor_to_major(10050, "USD"), 100.5);
        assert_eq!(minor_to_major(500, "JPY"), 500.0);
    }

    #[test]
    fn breakdown_json_still_carries_major_units() {
        // Money is minor units in Rust but major units on the wire.
        let result = compute_tax_breakdown(10000, &[rule(10.0, false, false)], on());
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["base"], serde_json::json!(100.0));
        assert_eq!(json["tax_total"], serde_json::json!(10.0));
        assert_eq!(json["grand_total"], serde_json::json!(110.0));
        assert_eq!(json["lines"][0]["amount"], serde_json::json!(10.0));
        let back: TaxBreakdown = serde_json::from_value(json).unwrap();
        assert_eq!(back.base, 10000);
        assert_eq!(back.tax_total, 1000);
    }

    #[test]
    fn round_tax_rounds_to_four_decimals() {
        assert_eq!(round_tax(1.23456), 1.2346);
        assert_eq!(round_tax(1.23454), 1.2345);
        assert_eq!(round_tax(2.5), 2.5);
    }
}
