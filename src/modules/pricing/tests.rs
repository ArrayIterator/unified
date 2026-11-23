// pricing — unit tests for the pure calculation functions.
// This file is the body of `#[cfg(test)] mod tests;` declared
// in the module root.
//
// Unit tests (pure functions only; no database, no network).

use super::*;
use chrono::TimeZone;

fn money(amount: &str) -> MoneyInput {
    MoneyInput {
        amount: amount.to_string(),
        currency: Some("USD".to_string()),
    }
}

fn cost_line(label: &str, source: &str, amount: &str) -> CostLineInput {
    CostLineInput {
        label: label.to_string(),
        source: source.to_string(),
        cost: money(amount),
    }
}

#[test]
fn money_parses_and_formats_exactly() {
    assert_eq!(parse_scaled_amount("19.99", "USD").unwrap(), 199_900);
    assert_eq!(parse_scaled_amount("0.01", "USD").unwrap(), 100);
    assert_eq!(parse_scaled_amount("1999", "JPY").unwrap(), 19_990_000);
    assert_eq!(format_scaled(199_900, "USD"), "19.99");
    assert_eq!(format_scaled(19_990_000, "JPY"), "1999");
    assert_eq!(format_scaled(0, "USD"), "0.00");
    assert!(parse_scaled_amount("19.99999", "USD").is_err());
    assert!(parse_scaled_amount("-5.00", "USD").is_err());
    assert!(parse_scaled_amount("abc", "USD").is_err());
}

#[test]
fn money_rounds_half_up_to_minor() {
    // 19.995 -> 20.00 (half-up on the dropped digits).
    assert_eq!(round_to_minor(199_950, "USD"), 200_000);
    // 19.994 -> 19.99.
    assert_eq!(round_to_minor(199_940, "USD"), 199_900);
    // Zero-decimal: 1999.5 -> 2000.
    assert_eq!(round_to_minor(19_995_000, "JPY"), 20_000_000);
    assert_eq!(round_to_minor(19_994_999, "JPY"), 19_990_000);
}

#[test]
fn cents_convert_without_float() {
    assert_eq!(cents_to_scaled(1999).unwrap(), 199_900);
    assert_eq!(scaled_to_cents(199_950), 2000);
    assert_eq!(scaled_to_cents(199_949), 1999);
}

#[test]
fn percent_uses_basis_points() {
    assert_eq!(percent_of(199_900, 2_500).unwrap(), 49_975);
    assert_eq!(percent_of(100_000, 10_000).unwrap(), 100_000);
    assert_eq!(percent_of(100_000, 0).unwrap(), 0);
}

#[test]
fn cost_plus_shows_every_step() {
    let result = cost_plus_recommendation(
        &[
            cost_line("Hosting", "invoice-1001", "4.00"),
            cost_line("Support", "payroll-q3", "6.00"),
        ],
        2_500,
    )
    .unwrap();
    assert_eq!(result.total_cost_scaled, 100_000);
    assert_eq!(result.margin_amount_scaled, 25_000);
    assert_eq!(result.suggested_price_scaled, 125_000);
    assert_eq!(result.suggested_price, "12.50");
    assert_eq!(result.lines.len(), 5);
}

#[test]
fn cost_plus_rejects_mixed_currencies() {
    let result = cost_plus_recommendation(
        &[
            cost_line("A", "src", "4.00"),
            CostLineInput {
                label: "B".to_string(),
                source: "src".to_string(),
                cost: MoneyInput {
                    amount: "4.00".to_string(),
                    currency: Some("EUR".to_string()),
                },
            },
        ],
        1_000,
    );
    assert!(result.is_err());
}

#[test]
fn tiered_price_splits_across_tiers() {
    let tiers = vec![
        TierInput {
            min_quantity: 1,
            max_quantity: Some(10),
            unit_price: money("10.00"),
        },
        TierInput {
            min_quantity: 11,
            max_quantity: None,
            unit_price: money("8.00"),
        },
    ];
    let result = tiered_price(&tiers, 15).unwrap();
    // 10 x 10.00 + 5 x 8.00 = 140.00.
    assert_eq!(result.total_scaled, 1_400_000);
    assert_eq!(result.total, "140.00");
    assert_eq!(result.lines.len(), 3);
    // Exactly at the boundary: only the first tier applies.
    assert_eq!(tiered_price(&tiers, 10).unwrap().total_scaled, 1_000_000);
    // Above the highest bounded tier without an open tier errors.
    let bounded = vec![TierInput {
        min_quantity: 1,
        max_quantity: Some(10),
        unit_price: money("10.00"),
    }];
    assert!(tiered_price(&bounded, 11).is_err());
    // Overlapping tiers are rejected.
    let overlapping = vec![
        TierInput {
            min_quantity: 1,
            max_quantity: Some(10),
            unit_price: money("10.00"),
        },
        TierInput {
            min_quantity: 10,
            max_quantity: None,
            unit_price: money("8.00"),
        },
    ];
    assert!(tiered_price(&overlapping, 5).is_err());
}

#[test]
fn bundle_price_applies_percent_and_caps_fixed() {
    let components = vec![
        BundleComponentInput {
            product_id: "a".to_string(),
            label: "A".to_string(),
            price: money("30.00"),
        },
        BundleComponentInput {
            product_id: "b".to_string(),
            label: "B".to_string(),
            price: money("20.00"),
        },
    ];
    let percent = bundle_price(
        &components,
        &BundleDiscountInput {
            kind: "percent".to_string(),
            percent_bp: 1_000,
            amount: None,
        },
    )
    .unwrap();
    assert_eq!(percent.subtotal_scaled, 500_000);
    assert_eq!(percent.discount_scaled, 50_000);
    assert_eq!(percent.total_scaled, 450_000);
    let fixed = bundle_price(
        &components,
        &BundleDiscountInput {
            kind: "fixed".to_string(),
            percent_bp: 0,
            amount: Some(money("60.00")),
        },
    )
    .unwrap();
    // Fixed discount is capped at the subtotal.
    assert_eq!(fixed.discount_scaled, 500_000);
    assert_eq!(fixed.total_scaled, 0);
}

fn discount_line(product: &str, unit: i64, quantity: u32) -> DiscountLineInput {
    DiscountLineInput {
        product_id: product.to_string(),
        unit_price_scaled: unit,
        quantity,
    }
}

fn percent_rule(id: &str, order: i64, bp: i64, stackable: bool, exclusive: bool) -> DiscountRuleInput {
    DiscountRuleInput {
        rule_id: id.to_string(),
        name: id.to_string(),
        kind: "percent".to_string(),
        percent_bp: bp,
        amount_scaled: 0,
        buy_quantity: 0,
        get_quantity: 0,
        min_quantity: 0,
        min_subtotal_scaled: 0,
        product_ids: vec![],
        evaluation_order: order,
        stackable,
        exclusive,
    }
}

#[test]
fn discounts_stack_in_declared_order() {
    let lines = vec![discount_line("p1", 1_000_000, 1)]; // 100.00
    let rules = vec![
        percent_rule("r2", 2, 1_000, true, false), // 10% second
        percent_rule("r1", 1, 1_000, true, false), // 10% first
    ];
    let result = evaluate_discounts(&lines, &rules, "USD").unwrap();
    // 100.00 -> 90.00 -> 81.00 (sequential on the remainder).
    assert_eq!(result.subtotal_scaled, 1_000_000);
    assert_eq!(result.total_discount_scaled, 190_000);
    assert_eq!(result.total_scaled, 810_000);
    assert_eq!(result.trace[0].rule_id, "r1");
    assert_eq!(result.trace[1].rule_id, "r2");
    assert!(result.trace.iter().all(|entry| entry.applied));
}

#[test]
fn exclusive_rule_stops_later_rules() {
    let lines = vec![discount_line("p1", 1_000_000, 1)];
    let rules = vec![
        percent_rule("first", 1, 2_000, true, true),
        percent_rule("second", 2, 5_000, true, false),
    ];
    let result = evaluate_discounts(&lines, &rules, "USD").unwrap();
    assert_eq!(result.total_discount_scaled, 200_000);
    assert_eq!(result.total_scaled, 800_000);
    assert!(result.trace[0].applied);
    assert!(!result.trace[1].applied);
    assert!(result.trace[1].skipped_reason.is_some());
}

#[test]
fn non_stackable_rule_stops_later_rules() {
    let lines = vec![discount_line("p1", 1_000_000, 1)];
    let rules = vec![
        percent_rule("first", 1, 1_000, false, false),
        percent_rule("second", 2, 1_000, true, false),
    ];
    let result = evaluate_discounts(&lines, &rules, "USD").unwrap();
    assert_eq!(result.total_discount_scaled, 100_000);
    assert!(!result.trace[1].applied);
}

#[test]
fn buy_x_get_y_frees_whole_groups() {
    let lines = vec![discount_line("p1", 100_000, 7)]; // 7 x 10.00
    let rule = DiscountRuleInput {
        rule_id: "b2g1".to_string(),
        name: "Buy 2 get 1".to_string(),
        kind: "buy_x_get_y".to_string(),
        percent_bp: 0,
        amount_scaled: 0,
        buy_quantity: 2,
        get_quantity: 1,
        min_quantity: 0,
        min_subtotal_scaled: 0,
        product_ids: vec![],
        evaluation_order: 1,
        stackable: true,
        exclusive: false,
    };
    let result = evaluate_discounts(&lines, &[rule], "USD").unwrap();
    // 7 units = 2 full groups of 3 -> 2 free units = 20.00 off.
    assert_eq!(result.total_discount_scaled, 200_000);
    assert_eq!(result.total_scaled, 500_000);
}

#[test]
fn volume_discount_needs_the_threshold() {
    let lines = vec![discount_line("p1", 100_000, 4)];
    let rule = DiscountRuleInput {
        rule_id: "vol".to_string(),
        name: "Volume".to_string(),
        kind: "volume".to_string(),
        percent_bp: 2_000,
        amount_scaled: 0,
        buy_quantity: 0,
        get_quantity: 0,
        min_quantity: 5,
        min_subtotal_scaled: 0,
        product_ids: vec![],
        evaluation_order: 1,
        stackable: true,
        exclusive: false,
    };
    let below = evaluate_discounts(&lines, &[rule.clone()], "USD").unwrap();
    assert_eq!(below.total_discount_scaled, 0);
    let lines = vec![discount_line("p1", 100_000, 5)];
    let above = evaluate_discounts(&lines, &[rule], "USD").unwrap();
    assert_eq!(above.total_discount_scaled, 100_000);
}

#[test]
fn fixed_discount_never_exceeds_the_total() {
    let lines = vec![discount_line("p1", 100_000, 1)];
    let rule = DiscountRuleInput {
        rule_id: "fix".to_string(),
        name: "Fixed".to_string(),
        kind: "fixed".to_string(),
        percent_bp: 0,
        amount_scaled: 5_000_000,
        buy_quantity: 0,
        get_quantity: 0,
        min_quantity: 0,
        min_subtotal_scaled: 0,
        product_ids: vec![],
        evaluation_order: 1,
        stackable: true,
        exclusive: false,
    };
    let result = evaluate_discounts(&lines, &[rule], "USD").unwrap();
    assert_eq!(result.total_discount_scaled, 100_000);
    assert_eq!(result.total_scaled, 0);
}

#[test]
fn proration_upgrade_charges_the_difference() {
    let start = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap();
    let change = Utc.with_ymd_and_hms(2026, 11, 16, 0, 0, 0).unwrap();
    let result = prorate(&ProrationInput {
        kind: "upgrade".to_string(),
        old_price_scaled: 300_000, // 30.00 / 30 days = 1.00 per day
        new_price_scaled: 600_000, // 60.00 / 30 days = 2.00 per day
        period_start: start,
        period_end: end,
        change_at: change,
        basis: "per_day".to_string(),
        days_per_month: 30,
        currency: "USD".to_string(),
    })
    .unwrap();
    assert_eq!(result.period_days, 30);
    assert_eq!(result.days_used, 15);
    assert_eq!(result.days_remaining, 15);
    assert_eq!(result.old_daily_rate_scaled, 10_000);
    assert_eq!(result.new_daily_rate_scaled, 20_000);
    assert_eq!(result.unused_credit_scaled, 150_000);
    assert_eq!(result.new_charge_scaled, 300_000);
    assert_eq!(result.net_due_scaled, 150_000);
}

#[test]
fn proration_downgrade_owes_a_credit() {
    let start = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap();
    let change = Utc.with_ymd_and_hms(2026, 11, 11, 0, 0, 0).unwrap();
    let result = prorate(&ProrationInput {
        kind: "downgrade".to_string(),
        old_price_scaled: 600_000,
        new_price_scaled: 300_000,
        period_start: start,
        period_end: end,
        change_at: change,
        basis: "per_day".to_string(),
        days_per_month: 30,
        currency: "USD".to_string(),
    })
    .unwrap();
    assert_eq!(result.days_used, 10);
    assert_eq!(result.days_remaining, 20);
    assert_eq!(result.net_due_scaled, -200_000);
}

#[test]
fn proration_rejects_a_change_outside_the_period() {
    let start = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap();
    let outside = Utc.with_ymd_and_hms(2026, 12, 15, 0, 0, 0).unwrap();
    let result = prorate(&ProrationInput {
        kind: "upgrade".to_string(),
        old_price_scaled: 300_000,
        new_price_scaled: 600_000,
        period_start: start,
        period_end: end,
        change_at: outside,
        basis: "per_day".to_string(),
        days_per_month: 30,
        currency: "USD".to_string(),
    });
    assert!(result.is_err());
}

fn reconcile_line(name: &str, quantity: u32, unit: i64, tax_bp: i64) -> ReconcileLineInput {
    ReconcileLineInput {
        name: name.to_string(),
        quantity,
        unit_price_scaled: unit,
        tax_bp,
        tax_inclusive: false,
    }
}

#[test]
fn reconciliation_matches_when_every_component_matches() {
    let lines = vec![
        reconcile_line("Plan", 1, 1_000_000, 1_000), // 100.00 + 10% tax
        reconcile_line("Seat", 2, 100_000, 1_000),   // 20.00 + 10% tax
    ];
    let discounts = vec![ReconcileAdjustmentInput {
        label: "Coupon".to_string(),
        amount_scaled: 100_000, // 10.00
    }];
    let claim = ReconcileClaim {
        subtotal_scaled: 1_200_000,
        discounts_scaled: 100_000,
        proration_scaled: 0,
        tax_total_scaled: 120_000,
        total_scaled: 1_220_000,
    };
    let result = reconcile_invoice(&lines, &discounts, &[], &claim, "USD").unwrap();
    assert!(result.matched);
    assert_eq!(result.total_discrepancy_scaled, 0);
}

#[test]
fn reconciliation_reports_the_exact_discrepancy() {
    let lines = vec![reconcile_line("Plan", 1, 1_000_000, 1_000)];
    let claim = ReconcileClaim {
        subtotal_scaled: 1_000_000,
        discounts_scaled: 0,
        proration_scaled: 0,
        tax_total_scaled: 90_000, // wrong: 10% of 100.00 is 10.00
        total_scaled: 1_090_000,
    };
    let result = reconcile_invoice(&lines, &[], &[], &claim, "USD").unwrap();
    assert!(!result.matched);
    let tax = result
        .components
        .iter()
        .find(|component| component.component == "tax_total")
        .unwrap();
    assert_eq!(tax.expected_scaled, 100_000);
    assert_eq!(tax.discrepancy_scaled, -10_000);
    assert_eq!(result.total_discrepancy_scaled, -10_000);
}

#[test]
fn reconciliation_inclusive_tax_splits_exactly() {
    let line = ReconcileLineInput {
        name: "Plan".to_string(),
        quantity: 1,
        unit_price_scaled: 1_100_000, // 110.00 tax-inclusive at 10%
        tax_bp: 1_000,
        tax_inclusive: true,
    };
    let claim = ReconcileClaim {
        subtotal_scaled: 1_000_000,
        discounts_scaled: 0,
        proration_scaled: 0,
        tax_total_scaled: 100_000,
        total_scaled: 1_100_000,
    };
    let result = reconcile_invoice(&[line], &[], &[], &claim, "USD").unwrap();
    assert!(result.matched);
}

#[test]
fn credit_note_reconciliation_requires_exact_lines() {
    let lines = vec![
        ReconcileAdjustmentInput {
            label: "Refund".to_string(),
            amount_scaled: 150_000,
        },
        ReconcileAdjustmentInput {
            label: "Goodwill".to_string(),
            amount_scaled: 50_000,
        },
    ];
    let ok = reconcile_credit_note(&lines, 200_000, "USD").unwrap();
    assert!(ok.matched);
    let bad = reconcile_credit_note(&lines, 199_900, "USD").unwrap();
    assert!(!bad.matched);
    assert_eq!(bad.total_discrepancy_scaled, -100);
}

#[test]
fn approval_threshold_uses_basis_points() {
    assert_eq!(price_change_bp(1_000_000, 1_100_000), Some(1_000));
    assert_eq!(price_change_bp(1_000_000, 1_090_000), Some(900));
    assert_eq!(price_change_bp(0, 0), Some(0));
    assert_eq!(price_change_bp(0, 100_000), None);
    assert!(needs_second_approver(1_000_000, 1_100_000, 1_000));
    assert!(!needs_second_approver(1_000_000, 1_100_000, 1_001));
    assert!(needs_second_approver(0, 100_000, 1_000));
}

#[test]
fn currency_conversion_uses_integer_ratios() {
    // 100.00 at a 15,000/10,000 rate is 150.00.
    assert_eq!(convert_currency(1_000_000, 15_000, 10_000, "USD").unwrap(), 1_500_000);
    assert!(convert_currency(1_000_000, 0, 10_000, "USD").is_err());
    assert!(convert_currency(1_000_000, 15_000, 0, "USD").is_err());
}

#[test]
fn reference_summary_median_is_half_up() {
    assert_eq!(
        summarize_references(&[100_000, 200_000, 300_000]),
        Some((100_000, 300_000, 200_000, 3))
    );
    // Even count: average of 100.00 and 200.00 is 150.00.
    assert_eq!(
        summarize_references(&[100_000, 200_000]),
        Some((100_000, 200_000, 150_000, 2))
    );
    assert_eq!(summarize_references(&[]), None);
}

#[test]
fn tenant_id_validation_accepts_uuidv7_shape_only() {
    assert!(validate_tenant_id("019a1234-5678-7abc-8def-0123456789ab").is_ok());
    // Version nibble is 4, not 7.
    assert!(validate_tenant_id("019a1234-5678-4abc-8def-0123456789ab").is_err());
    assert!(validate_tenant_id("not-a-uuid").is_err());
    assert!(validate_tenant_id("").is_err());
}

#[test]
fn settings_default_to_configurable_values() {
    let settings = PricingSettings::default();
    assert_eq!(settings.rounding_mode, "half_up");
    assert_eq!(settings.proration_basis, "per_day");
    assert_eq!(settings.days_per_month, 30);
    assert_eq!(settings.approval_threshold_bp, 1_000);
    assert!(settings.require_second_approver);
    assert_eq!(settings.default_currency, "USD");
}
