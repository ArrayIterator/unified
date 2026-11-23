// pricing — proration.

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

// ---------------------------------------------------------------------------
// Proration engine (pure).
// ---------------------------------------------------------------------------

/// Input for one proration computation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProrationInput {
    /// `upgrade`, `downgrade`, `plan_change`, or `seat_change`.
    pub kind: String,
    /// Price of the old plan for one full period (scaled).
    pub old_price_scaled: i64,
    /// Price of the new plan for one full period (scaled).
    pub new_price_scaled: i64,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    /// Moment the change takes effect.
    pub change_at: DateTime<Utc>,
    /// `per_day` or `per_month`.
    pub basis: String,
    /// Days in one month when `basis` is `per_month`. Configurable
    /// default is 30.
    pub days_per_month: i64,
    pub currency: String,
}

/// Result of a proration computation, with the exact math shown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProrationResult {
    pub kind: String,
    pub basis: String,
    pub period_days: i64,
    pub days_used: i64,
    pub days_remaining: i64,
    pub old_daily_rate_scaled: i64,
    pub old_daily_rate: String,
    pub new_daily_rate_scaled: i64,
    pub new_daily_rate: String,
    /// Credit for the unused time on the old plan (positive number;
    /// a credit the customer receives).
    pub unused_credit_scaled: i64,
    pub unused_credit: String,
    /// Charge for the remaining time on the new plan.
    pub new_charge_scaled: i64,
    pub new_charge: String,
    /// Net amount due: charge minus credit. Negative means the
    /// customer is owed a credit.
    pub net_due_scaled: i64,
    pub net_due: String,
    pub currency: String,
    pub lines: Vec<CalculationLine>,
}

/// Proration, computed per day. The daily rate is the period price
/// divided by the days in the period (per-day basis) or by the
/// configured month length (per-month basis), rounded half-up to
/// the minor unit first; the credit and the charge are then whole
/// days times the rounded daily rate, so the invoice lines add up
/// exactly. Days are whole calendar days: the change day counts as
/// a used day on the old plan.
pub fn prorate(input: &ProrationInput) -> UnifiedResult<ProrationResult> {
    if !PRORATION_KINDS.contains(&input.kind.as_str()) {
        return Err(UnifiedError::bad_request(
            "Proration kind must be \"upgrade\", \"downgrade\", \"plan_change\", or \"seat_change\"",
        ));
    }
    if !PRORATION_BASES.contains(&input.basis.as_str()) {
        return Err(UnifiedError::bad_request(
            "Proration basis must be \"per_day\" or \"per_month\"",
        ));
    }
    validate_currency(&input.currency)?;
    if input.old_price_scaled < 0 || input.new_price_scaled < 0 {
        return Err(UnifiedError::bad_request(
            "Proration prices must not be negative",
        ));
    }
    if input.period_end <= input.period_start {
        return Err(UnifiedError::bad_request(
            "Period end must be after period start",
        ));
    }
    if input.change_at < input.period_start || input.change_at > input.period_end {
        return Err(UnifiedError::bad_request(
            "Change time must fall inside the billing period",
        ));
    }
    if input.basis == "per_month" && input.days_per_month <= 0 {
        return Err(UnifiedError::bad_request(
            "Days per month must be a positive integer",
        ));
    }
    let period_days = input
        .period_end
        .signed_duration_since(input.period_start)
        .num_days();
    if period_days <= 0 {
        return Err(UnifiedError::bad_request(
            "Billing period must span at least one day",
        ));
    }
    // The change day counts as used: days used is at least 1 when
    // the change happens after the period start, and days remaining
    // excludes the change day. A change part-way through the first
    // day still uses that day, so a change at any time after the
    // period start counts at least one used day; a change exactly
    // at the period start uses zero days.
    let raw_days_used = input
        .change_at
        .signed_duration_since(input.period_start)
        .num_days()
        .max(0);
    let days_used = if input.change_at > input.period_start {
        raw_days_used.max(1)
    } else {
        0
    };
    let mut days_remaining = (period_days - days_used).max(0);
    if input.basis == "per_month" {
        // The daily rate divides by days_per_month, so the credit
        // must never cover more than days_per_month days: in a
        // longer real period (for example 31 days with a 30-day
        // month) the uncapped credit would exceed the old plan's
        // full price. Cap the remaining days at the month length.
        days_remaining = days_remaining.min(input.days_per_month);
    }
    let divisor = if input.basis == "per_month" {
        input.days_per_month
    } else {
        period_days
    };
    let old_daily = round_to_minor(
        mul_div_half_up(input.old_price_scaled, 1, divisor)?,
        &input.currency,
    );
    let new_daily = round_to_minor(
        mul_div_half_up(input.new_price_scaled, 1, divisor)?,
        &input.currency,
    );
    let unused_credit = round_to_minor(
        old_daily
            .checked_mul(days_remaining)
            .ok_or_else(|| UnifiedError::bad_request("Proration amount is too large"))?,
        &input.currency,
    );
    let new_charge = round_to_minor(
        new_daily
            .checked_mul(days_remaining)
            .ok_or_else(|| UnifiedError::bad_request("Proration amount is too large"))?,
        &input.currency,
    );
    let net_due = new_charge
        .checked_sub(unused_credit)
        .ok_or_else(|| UnifiedError::bad_request("Proration net amount is too large"))?;
    let mut lines = vec![
        CalculationLine::new(
            "Old plan daily rate",
            old_daily,
            &input.currency,
            Some(format!(
                "Old period price {} / {} days (basis {})",
                format_scaled(input.old_price_scaled, &input.currency),
                divisor,
                input.basis
            )),
        ),
        CalculationLine::new(
            "New plan daily rate",
            new_daily,
            &input.currency,
            Some(format!(
                "New period price {} / {} days (basis {})",
                format_scaled(input.new_price_scaled, &input.currency),
                divisor,
                input.basis
            )),
        ),
        CalculationLine::new(
            &format!("Unused time credit ({} days remaining)", days_remaining),
            -unused_credit,
            &input.currency,
            Some(format!(
                "{} days remaining x {} per day",
                days_remaining,
                format_scaled(old_daily, &input.currency)
            )),
        ),
        CalculationLine::new(
            &format!("New plan charge ({} days remaining)", days_remaining),
            new_charge,
            &input.currency,
            Some(format!(
                "{} days remaining x {} per day",
                days_remaining,
                format_scaled(new_daily, &input.currency)
            )),
        ),
        CalculationLine::new("Net amount due", net_due, &input.currency, None),
    ];
    // The period line goes first for readability.
    lines.insert(
        0,
        CalculationLine::new(
            "Billing period",
            0,
            &input.currency,
            Some(format!(
                "{} days in period, {} days used, {} days remaining",
                period_days, days_used, days_remaining
            )),
        ),
    );
    Ok(ProrationResult {
        kind: input.kind.clone(),
        basis: input.basis.clone(),
        period_days,
        days_used,
        days_remaining,
        old_daily_rate_scaled: old_daily,
        old_daily_rate: format_scaled(old_daily, &input.currency),
        new_daily_rate_scaled: new_daily,
        new_daily_rate: format_scaled(new_daily, &input.currency),
        unused_credit_scaled: unused_credit,
        unused_credit: format_scaled(unused_credit, &input.currency),
        new_charge_scaled: new_charge,
        new_charge: format_scaled(new_charge, &input.currency),
        net_due_scaled: net_due,
        net_due: format_scaled(net_due, &input.currency),
        currency: input.currency.clone(),
        lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn input(
        old_price_scaled: i64,
        new_price_scaled: i64,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        change_at: DateTime<Utc>,
        basis: &str,
    ) -> ProrationInput {
        ProrationInput {
            kind: "upgrade".to_string(),
            old_price_scaled,
            new_price_scaled,
            period_start: start,
            period_end: end,
            change_at,
            basis: basis.to_string(),
            days_per_month: 30,
            currency: "USD".to_string(),
        }
    }

    #[test]
    fn per_month_credit_never_exceeds_the_old_plan_price() {
        // 31-day period with a 30-day month: before the cap the
        // credit was old_daily x 31, more than the old price.
        let start = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        let result = prorate(&input(300_000, 600_000, start, end, start, "per_month")).unwrap();
        assert_eq!(result.period_days, 31);
        assert_eq!(result.days_used, 0);
        assert_eq!(result.days_remaining, 30);
        assert_eq!(result.unused_credit_scaled, 300_000);
        assert!(result.unused_credit_scaled <= 300_000);
    }

    #[test]
    fn change_day_counts_as_a_used_day() {
        // Mid-day change on the first day: the truncating day
        // count is 0, but the change day counts as used.
        let start = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap();
        let change = Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0).unwrap();
        let result = prorate(&input(300_000, 600_000, start, end, change, "per_day")).unwrap();
        assert_eq!(result.days_used, 1);
        assert_eq!(result.days_remaining, 29);
        assert_eq!(result.unused_credit_scaled, 290_000);
        // A change exactly at the period start uses zero days.
        let at_start = prorate(&input(300_000, 600_000, start, end, start, "per_day")).unwrap();
        assert_eq!(at_start.days_used, 0);
        assert_eq!(at_start.days_remaining, 30);
    }

    #[test]
    fn net_due_is_charge_minus_credit_with_checked_subtraction() {
        let start = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap();
        let change = Utc.with_ymd_and_hms(2026, 11, 11, 0, 0, 0).unwrap();
        let mut downgrade = input(600_000, 300_000, start, end, change, "per_day");
        downgrade.kind = "downgrade".to_string();
        let result = prorate(&downgrade).unwrap();
        assert_eq!(result.new_charge_scaled, 200_000);
        assert_eq!(result.unused_credit_scaled, 400_000);
        assert_eq!(result.net_due_scaled, -200_000);
        assert_eq!(
            result.net_due_scaled,
            result.new_charge_scaled - result.unused_credit_scaled
        );
    }
}
