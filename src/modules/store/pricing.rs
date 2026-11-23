// Store module - pricing, coupon, and subscription-period rules.
//
// Split from `src/modules/store.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use actix_web::HttpRequest;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::helpers::{
    apply_ratio_to_minor, div_rounded, minor_to_major, saturating_i64, scaled_to_cents,
};
use super::state::{StoreState, now};
use super::types::{Coupon, OrderItem, PricingTier, Subscription};

pub(crate) fn valid_kind(kind: &str) -> bool {
    matches!(kind, "theme" | "plugin" | "software")
}

pub(crate) fn valid_currency(currency: &str) -> bool {
    currency.len() == 3 && currency.chars().all(|c| c.is_ascii_alphabetic())
}

pub(crate) fn valid_pricing_model(model: &str) -> bool {
    matches!(model, "one_time" | "subscription")
}

pub(crate) fn valid_billing_period(period: &str) -> bool {
    matches!(period, "monthly" | "yearly" | "weekly")
}

/// Subscription plan settings as resolved at checkout: the
/// proration policy must be one of `prorate`, `no_prorate`,
/// `credit_only`; grace days 0..=90; dunning retries 0..=10;
/// retry interval 1..=30 days. Bounds match the products module.
pub(crate) fn validate_subscription_settings(
    proration_policy: Option<&str>,
    grace_period_days: Option<u32>,
    dunning_max_retries: Option<u32>,
    dunning_retry_interval_days: Option<u32>,
) -> UnifiedResult<()> {
    if let Some(policy) = proration_policy {
        if !matches!(policy, "prorate" | "no_prorate" | "credit_only") {
            return Err(UnifiedError::bad_request(
                "Proration policy must be one of: prorate, no_prorate, credit_only",
            ));
        }
    }
    if let Some(grace) = grace_period_days {
        if grace > 90 {
            return Err(UnifiedError::bad_request(
                "Grace period must be at most 90 days",
            ));
        }
    }
    if let Some(max_retries) = dunning_max_retries {
        if max_retries > 10 {
            return Err(UnifiedError::bad_request(
                "Dunning max retries must be at most 10",
            ));
        }
    }
    if let Some(interval) = dunning_retry_interval_days {
        if interval == 0 || interval > 30 {
            return Err(UnifiedError::bad_request(
                "Dunning retry interval must be between 1 and 30 days",
            ));
        }
    }
    Ok(())
}

pub(crate) fn valid_product_status(status: &str) -> bool {
    matches!(status, "draft" | "published")
}

/// Valid order status transitions. Anything not listed here is rejected
/// with a 422 so an order can never jump backwards or skip a step.
///
/// `pending_verification` is the bank-transfer state: checkout with
/// provider `bank_transfer` opens a transfer verification, approval
/// moves the order to `paid` (`payment.captured`), and rejection
/// moves it to `cancelled`. Returning to `pending` stays allowed so
/// an administrator can put a rejected order back to awaiting payment
/// for a retry. `refund_pending` is the state of an order whose refund
/// was accepted by the payment adapter but is not complete yet (a
/// manual bank-transfer refund, for example): it only becomes
/// `refunded` through the refund-completion path, and returns to
/// `paid` when the refund is abandoned.
pub(crate) fn allowed_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("pending", "payment_pending")
            | ("pending", "pending_verification")
            | ("pending", "paid")
            | ("pending", "cancelled")
            | ("payment_pending", "pending_verification")
            | ("payment_pending", "paid")
            | ("payment_pending", "cancelled")
            | ("pending_verification", "paid")
            | ("pending_verification", "cancelled")
            | ("pending_verification", "payment_pending")
            | ("pending_verification", "pending")
            | ("paid", "completed")
            | ("paid", "refund_pending")
            | ("paid", "refunded")
            | ("completed", "refund_pending")
            | ("completed", "refunded")
            | ("refund_pending", "refunded")
            | ("refund_pending", "paid")
    )
}

/// License expiry from the pricing model: a one-time purchase grants a
/// long-lived license whose updates window lasts `updates_years` years
/// (default 1); a subscription lasts exactly one billing period.
pub(crate) fn license_expiry_for(
    pricing_model: &str,
    billing_period: Option<&str>,
    updates_years: Option<u32>,
) -> Option<DateTime<Utc>> {
    let current = now();
    match pricing_model {
        "subscription" => match billing_period {
            Some("weekly") => Some(current + chrono::Duration::days(7)),
            Some("monthly") => Some(current + chrono::Duration::days(30)),
            Some("yearly") => Some(current + chrono::Duration::days(365)),
            // An unknown or missing billing period has no expiry to
            // compute: fail closed with None instead of silently
            // granting a 30-day license. Callers that need a
            // subscription expiry must reject the order instead.
            _ => None,
        },
        _ => Some(current + chrono::Duration::days(365 * updates_years.unwrap_or(1) as i64)),
    }
}

/// Length of one billing period in days, using the same convention
/// as `license_expiry_for`: weekly 7, monthly 30, yearly 365.
/// `None` for an unknown period: callers must fail closed instead
/// of silently treating it as a month.
pub(crate) fn period_days(billing_period: &str) -> Option<i64> {
    match billing_period {
        "weekly" => Some(7),
        "monthly" => Some(30),
        "yearly" => Some(365),
        _ => None,
    }
}

/// End of the billing period that starts at `start`. `None` for an
/// unknown billing period.
pub(crate) fn period_end(start: DateTime<Utc>, billing_period: &str) -> Option<DateTime<Utc>> {
    period_days(billing_period).map(|days| start + chrono::Duration::days(days))
}

/// True when the subscription currently grants access: trialing or
/// active inside the current period; past-due inside the grace
/// period; cancelled at period end inside the current period.
/// A subscription cancelled immediately, or past its grace period,
/// grants nothing.
pub(crate) fn subscription_grants_access(subscription: &Subscription, at: DateTime<Utc>) -> bool {
    match subscription.status.as_str() {
        "trialing" | "active" => at <= subscription.current_period_end,
        "past_due" => {
            at <= subscription.current_period_end
                + chrono::Duration::days(i64::from(subscription.grace_period_days))
        }
        "cancelled" => {
            subscription.cancel_at_period_end && at <= subscription.current_period_end
        }
        _ => false,
    }
}

/// Proration for a mid-cycle plan change, in integer minor units.
///
/// `remaining_days` is the time left in the current period and
/// `period_total_days` its full length. The credit is the unused
/// share of the old price; the charge is the new price for the same
/// remaining share of the new period. Both are rounded to minor
/// units (half away from zero, exactly like the old `round_money`
/// float math); the net amount is `charge - credit` and may be
/// negative (a downgrade credits more than it charges).
/// `no_prorate` changes return zeroes: nothing is charged or
/// credited mid-cycle.
pub(crate) fn proration_amounts(
    policy: &str,
    old_price: i64,
    new_price: i64,
    remaining_days: f64,
    period_total_days: f64,
    new_period_total_days: f64,
) -> (i64, i64, i64) {
    // An unknown policy charges and credits nothing instead of
    // silently behaving as `prorate`: only the three known policies
    // (`prorate`, `no_prorate`, `credit_only`) ever move money.
    // `validate_subscription_settings` already rejects unknown
    // policies on input; this is the defense for stored data that
    // predates the validation.
    if !matches!(policy, "prorate" | "no_prorate" | "credit_only") {
        return (0, 0, 0);
    }
    if policy == "no_prorate" || remaining_days <= 0.0 || period_total_days <= 0.0 {
        return (0, 0, 0);
    }
    let share = (remaining_days / period_total_days).clamp(0.0, 1.0);
    let credit = apply_ratio_to_minor(old_price, share);
    let new_share = if new_period_total_days > 0.0 {
        (remaining_days / new_period_total_days).clamp(0.0, 1.0)
    } else {
        0.0
    };
    // `credit_only` credits the unused time but charges nothing
    // until the next period; `prorate` charges the new plan for the
    // remaining time as well.
    let charge = if policy == "credit_only" {
        0
    } else {
        apply_ratio_to_minor(new_price, new_share)
    };
    let net = charge - credit;
    (credit, charge, net)
}

pub(crate) fn resolve_tier(state: &StoreState, product_id: &str, tier_id: &str) -> UnifiedResult<PricingTier> {
    let tier = state
        .tiers
        .get(tier_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))?;
    if tier.product_id != product_id {
        return Err(UnifiedError::bad_request(
            "Pricing tier does not belong to this product",
        ));
    }
    if tier.status != "active" {
        return Err(UnifiedError::unprocessable(
            "Pricing tier is disabled",
        ));
    }
    Ok(tier)
}

/// Validate that a coupon code exists, is active, has not expired, and
/// still has uses left. Product applicability is checked separately so
/// a multi-item order can discount only the items a coupon covers.
pub(crate) fn validate_coupon_usable(state: &StoreState, code: &str) -> UnifiedResult<Coupon> {
    let normalized = code.trim().to_uppercase();
    let coupon = state
        .coupons
        .iter()
        .map(|entry| entry.value().clone())
        .find(|coupon| coupon.code == normalized)
        .ok_or_else(|| UnifiedError::not_found("Coupon not found"))?;
    if !coupon.active {
        return Err(UnifiedError::unprocessable("Coupon is not active"));
    }
    if let Some(expires_at) = coupon.expires_at {
        if expires_at <= now() {
            return Err(UnifiedError::unprocessable("Coupon has expired"));
        }
    }
    if let Some(limit) = coupon.usage_limit {
        if coupon.used_count >= limit {
            return Err(UnifiedError::unprocessable(
                "Coupon usage limit has been reached",
            ));
        }
    }
    Ok(coupon)
}

/// Atomically claim one use of a coupon: the active, unexpired,
/// and under-limit checks and the `used_count` increment all happen
/// while holding the coupon entry's lock, so two concurrent
/// checkouts cannot both spend the last use. Returns the coupon
/// after the increment. Callers must claim only after the discount
/// has been evaluated as positive: a claim is never refunded by a
/// later checkout failure, and nothing fallible runs between the
/// claim and the order insert.
pub(crate) fn claim_coupon_use(state: &StoreState, code: &str) -> UnifiedResult<Coupon> {
    let normalized = code.trim().to_uppercase();
    let coupon_id = state
        .coupons
        .iter()
        .find(|entry| entry.value().code == normalized)
        .map(|entry| entry.key().clone())
        .ok_or_else(|| UnifiedError::not_found("Coupon not found"))?;
    let Some(mut coupon) = state.coupons.get_mut(&coupon_id) else {
        return Err(UnifiedError::not_found("Coupon not found"));
    };
    if !coupon.active {
        return Err(UnifiedError::unprocessable("Coupon is not active"));
    }
    if let Some(expires_at) = coupon.expires_at {
        if expires_at <= now() {
            return Err(UnifiedError::unprocessable("Coupon has expired"));
        }
    }
    if let Some(limit) = coupon.usage_limit {
        if coupon.used_count >= limit {
            return Err(UnifiedError::unprocessable(
                "Coupon usage limit has been reached",
            ));
        }
    }
    coupon.used_count += 1;
    coupon.updated_at = now();
    Ok(coupon.clone())
}

/// Discount a coupon gives on one line subtotal, or 0 when the coupon
/// does not cover that product. The discount never exceeds the subtotal.
/// Amounts are integer minor units throughout: the percent branch
/// computes `subtotal * basis_points / 10_000` with `i128`
/// intermediates (exact, no floats), and the fixed branch is already
/// minor units, so the currency is no longer needed here. The
/// `currency` parameter stays so existing callers do not change.
pub(crate) fn coupon_discount_for(
    coupon: &Coupon,
    product_id: &str,
    subtotal: i64,
    _currency: &str,
) -> i64 {
    if !coupon.product_ids.is_empty() && !coupon.product_ids.iter().any(|id| id == product_id) {
        return 0;
    }
    match coupon.discount_type.as_str() {
        // A percent discount above 100% (or on a subtotal smaller
        // than the computed amount) never pays the buyer: cap at
        // the subtotal, exactly like the fixed branch.
        "percent" => saturating_i64(div_rounded(
            i128::from(subtotal) * i128::from(coupon.discount_percent_bp),
            10_000,
        ))
        .min(subtotal),
        _ => coupon.discount_fixed_minor.min(subtotal),
    }
}

/// Validate a coupon code for one product and return the discount it
/// gives on the given subtotal.
pub(crate) fn coupon_discount(
    state: &StoreState,
    code: &str,
    product_id: &str,
    subtotal: i64,
    currency: &str,
) -> UnifiedResult<(Coupon, i64)> {
    let coupon = validate_coupon_usable(state, code)?;
    if !coupon.product_ids.is_empty() && !coupon.product_ids.iter().any(|id| id == product_id) {
        return Err(UnifiedError::unprocessable(
            "Coupon does not apply to this product",
        ));
    }
    let discount = coupon_discount_for(&coupon, product_id, subtotal, currency);
    Ok((coupon, discount))
}

/// Evaluate a store coupon through the shared pricing evaluator
/// (`pricing.discount.evaluate_requested`, which never mutates the
/// order). Returns the discount in integer minor units plus the
/// evaluation trace to record on the order. The pricing answer is
/// used only when it is valid AND it priced this coupon (no
/// `coupon_problem`): pricing holds its own coupon records, so a
/// store coupon it does not know reports
/// `coupon_problem: "Coupon not found"` and must not zero out a
/// valid store coupon. When pricing does not answer, answers
/// invalid, does not hold the coupon, or no tenant context
/// resolves for the request, the store-local integer math applies
/// instead and the trace says so. Exactly one evaluator's math is
/// ever applied to an order. The tenant id comes from the
/// request's resolved tenant context, never from a client header.
/// Store money is integer minor units; the pricing discount
/// arrives as integer scaled units and is converted to minor units
/// with half-up rounding at this boundary. The trace records the
/// local discount in major units, as before.
pub(crate) fn evaluate_coupon_with_pricing(
    state: &StoreState,
    req: &HttpRequest,
    items: &[OrderItem],
    coupon: &Coupon,
    customer_id: &str,
    currency: &str,
) -> (i64, Value) {
    let mut local_discount: i64 = 0;
    for item in items {
        local_discount += coupon_discount_for(coupon, &item.product_id, item.line_total, currency);
    }
    let local_trace = |reason: &str, pricing_answer: Value| {
        json!({
            "evaluator": "store-local",
            "reason": reason,
            "coupon_code": coupon.code,
            "currency": currency,
            "local_discount": minor_to_major(local_discount, currency),
            "pricing_answer": pricing_answer,
        })
    };
    let Ok(tenant) = unified_core::tenancy::TenantContext::from_http_request(req) else {
        return (
            local_discount,
            local_trace(
                "No resolved tenant context, so the pricing hook could not run",
                Value::Null,
            ),
        );
    };
    let lines: Vec<Value> = items
        .iter()
        .map(|item| {
            json!({
                "product_id": item.product_id,
                "unit_price": format!("{:.4}", minor_to_major(item.unit_price, currency)),
                "quantity": item.quantity,
            })
        })
        .collect();
    let hook_context = state.emit_with_results(
        "pricing.discount.evaluate_requested",
        json!({
            "tenant_id": tenant.tenant_id.to_string(),
            "lines": lines,
            "coupon_code": coupon.code,
            "customer_id": customer_id,
            "currency": currency,
        }),
    );
    let Some(answer) = hook_context.results.into_iter().next() else {
        return (
            local_discount,
            local_trace("No pricing evaluator answered", Value::Null),
        );
    };
    let valid = answer
        .get("valid")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let coupon_problem = answer
        .get("coupon_problem")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    if !valid {
        let reason = answer
            .get("error")
            .and_then(|value| value.as_str())
            .map(|error| format!("Pricing evaluator refused the evaluation: {error}"))
            .unwrap_or_else(|| "Pricing evaluator answered valid: false".to_string());
        return (local_discount, local_trace(&reason, answer));
    }
    if let Some(problem) = coupon_problem {
        return (
            local_discount,
            local_trace(
                &format!("Pricing evaluator did not hold this coupon ({problem}); store-local math applied"),
                answer,
            ),
        );
    }
    let discount_scaled = answer
        .get("total_discount_scaled")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    // Scaled units (exponent 4) convert to minor units with
    // half-up rounding; that is already the minor-unit discount.
    let discount_minor = scaled_to_cents(discount_scaled);
    let mut trace = answer;
    if let Value::Object(map) = &mut trace {
        map.insert("evaluator".to_string(), Value::String("pricing".to_string()));
    }
    (discount_minor, trace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subscription(
        status: &str,
        period_end: DateTime<Utc>,
        grace_days: u32,
        cancel_at_end: bool,
    ) -> Subscription {
        let now = Utc::now();
        Subscription {
            id: "sub-1".to_string(),
            user_id: "user-1".to_string(),
            company_id: None,
            product_id: "prod-1".to_string(),
            tier_id: None,
            order_id: "ord-1".to_string(),
            module_id: None,
            status: status.to_string(),
            billing_period: "monthly".to_string(),
            price: 1000,
            currency: "USD".to_string(),
            trial_days: 0,
            trial_end: None,
            current_period_start: now,
            current_period_end: period_end,
            proration_policy: "prorate".to_string(),
            grace_period_days: grace_days,
            dunning_max_retries: 3,
            dunning_retry_interval_days: 1,
            cancel_at_period_end: cancel_at_end,
            cancelled_at: None,
            pending_product_id: None,
            pending_tier_id: None,
            pending_price: None,
            pending_billing_period: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn coupon(
        discount_type: &str,
        discount_percent_bp: i64,
        discount_fixed_minor: i64,
        product_ids: Vec<String>,
    ) -> Coupon {
        let now = Utc::now();
        Coupon {
            id: "cp-1".to_string(),
            code: "SAVE10".to_string(),
            discount_type: discount_type.to_string(),
            discount_percent_bp,
            discount_fixed_minor,
            expires_at: None,
            usage_limit: None,
            used_count: 0,
            product_ids,
            active: true,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn valid_currency_accepts_three_letter_codes() {
        assert!(valid_currency("USD"));
        assert!(valid_currency("usd"));
        assert!(!valid_currency("US"));
        assert!(!valid_currency("USDD"));
        assert!(!valid_currency("U1D"));
    }

    #[test]
    fn valid_kind_model_period_and_status() {
        assert!(valid_kind("theme"));
        assert!(valid_kind("plugin"));
        assert!(valid_kind("software"));
        assert!(!valid_kind("course"));
        assert!(valid_pricing_model("one_time"));
        assert!(valid_pricing_model("subscription"));
        assert!(!valid_pricing_model("free"));
        assert!(valid_billing_period("monthly"));
        assert!(valid_billing_period("yearly"));
        assert!(valid_billing_period("weekly"));
        assert!(!valid_billing_period("daily"));
        assert!(valid_product_status("draft"));
        assert!(valid_product_status("published"));
        assert!(!valid_product_status("active"));
    }

    #[test]
    fn allowed_transition_guards_order_flow() {
        assert!(allowed_transition("pending", "paid"));
        assert!(!allowed_transition("paid", "pending"));
        assert!(allowed_transition("paid", "completed"));
        assert!(!allowed_transition("completed", "paid"));
        assert!(allowed_transition("refund_pending", "paid"));
        assert!(!allowed_transition("pending", "refunded"));
        assert!(allowed_transition("pending_verification", "pending"));
    }

    #[test]
    fn period_days_and_period_end() {
        assert_eq!(period_days("weekly"), Some(7));
        assert_eq!(period_days("monthly"), Some(30));
        assert_eq!(period_days("yearly"), Some(365));
        assert_eq!(period_days("daily"), None);
        assert_eq!(period_days(""), None);
        let start = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let expected = DateTime::parse_from_rfc3339("2026-01-31T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(period_end(start, "monthly"), Some(expected));
        assert_eq!(period_end(start, "daily"), None);
    }

    #[test]
    fn proration_amounts_credit_and_charge() {
        assert_eq!(
            proration_amounts("prorate", 10000, 20000, 15.0, 30.0, 30.0),
            (5000, 10000, 5000)
        );
        assert_eq!(
            proration_amounts("credit_only", 10000, 20000, 15.0, 30.0, 30.0),
            (5000, 0, -5000)
        );
        assert_eq!(
            proration_amounts("no_prorate", 10000, 20000, 15.0, 30.0, 30.0),
            (0, 0, 0)
        );
        assert_eq!(
            proration_amounts("prorate", 10000, 20000, 0.0, 30.0, 30.0),
            (0, 0, 0)
        );
        assert_eq!(
            proration_amounts("prorate", 10000, 10000, 40.0, 30.0, 30.0),
            (10000, 10000, 0)
        );
    }

    #[test]
    fn proration_amounts_rounds_half_cents_away_from_zero() {
        // 105 minor at a half share is 52.5 minor: the old float
        // math rounded 0.525 major to 0.53, i.e. 53 minor units.
        assert_eq!(
            proration_amounts("prorate", 105, 105, 15.0, 30.0, 30.0),
            (53, 53, 0)
        );
    }

    #[test]
    fn proration_amounts_unknown_policy_moves_no_money() {
        assert_eq!(
            proration_amounts("bogus", 10000, 20000, 15.0, 30.0, 30.0),
            (0, 0, 0)
        );
        assert_eq!(
            proration_amounts("", 10000, 20000, 15.0, 30.0, 30.0),
            (0, 0, 0)
        );
    }

    #[test]
    fn validate_subscription_settings_bounds() {
        assert!(validate_subscription_settings(None, None, None, None).is_ok());
        assert!(validate_subscription_settings(Some("bogus"), None, None, None).is_err());
        assert!(validate_subscription_settings(Some("prorate"), Some(90), Some(10), Some(30)).is_ok());
        assert!(validate_subscription_settings(None, Some(91), None, None).is_err());
        assert!(validate_subscription_settings(None, None, Some(11), None).is_err());
        assert!(validate_subscription_settings(None, None, None, Some(0)).is_err());
        assert!(validate_subscription_settings(None, None, None, Some(31)).is_err());
    }

    #[test]
    fn license_expiry_for_uses_period_or_years() {
        let monthly = license_expiry_for("subscription", Some("monthly"), None).unwrap();
        assert!(monthly > Utc::now());
        assert!(monthly <= Utc::now() + chrono::Duration::days(31));
        let one_time = license_expiry_for("one_time", None, None).unwrap();
        let days = (one_time - Utc::now()).num_days();
        assert!((364..=365).contains(&days));
        let two_years = license_expiry_for("one_time", None, Some(2)).unwrap();
        let days = (two_years - Utc::now()).num_days();
        assert!((729..=730).contains(&days));
    }

    #[test]
    fn license_expiry_for_unknown_period_is_none() {
        assert_eq!(license_expiry_for("subscription", Some("daily"), None), None);
        assert_eq!(license_expiry_for("subscription", Some(""), None), None);
        assert_eq!(license_expiry_for("subscription", None, None), None);
    }

    #[test]
    fn subscription_grants_access_states() {
        let now = Utc::now();
        assert!(subscription_grants_access(
            &subscription("active", now + chrono::Duration::days(1), 0, false),
            now
        ));
        assert!(!subscription_grants_access(
            &subscription("active", now - chrono::Duration::days(1), 0, false),
            now
        ));
        assert!(subscription_grants_access(
            &subscription("trialing", now + chrono::Duration::days(1), 0, false),
            now
        ));
        assert!(subscription_grants_access(
            &subscription("past_due", now - chrono::Duration::days(3), 7, false),
            now
        ));
        assert!(!subscription_grants_access(
            &subscription("past_due", now - chrono::Duration::days(8), 7, false),
            now
        ));
        assert!(subscription_grants_access(
            &subscription("cancelled", now + chrono::Duration::days(1), 0, true),
            now
        ));
        assert!(!subscription_grants_access(
            &subscription("cancelled", now + chrono::Duration::days(1), 0, false),
            now
        ));
        assert!(!subscription_grants_access(
            &subscription("suspended", now + chrono::Duration::days(1), 0, false),
            now
        ));
    }

    #[test]
    fn coupon_discount_for_caps_and_scopes() {
        assert_eq!(coupon_discount_for(&coupon("percent", 1000, 0, vec![]), "p1", 20000, "USD"), 2000);
        assert_eq!(coupon_discount_for(&coupon("fixed", 0, 5000, vec![]), "p1", 3000, "USD"), 3000);
        assert_eq!(coupon_discount_for(&coupon("fixed", 0, 2500, vec![]), "p1", 10000, "USD"), 2500);
        assert_eq!(
            coupon_discount_for(&coupon("percent", 1000, 0, vec!["p1".to_string()]), "p2", 20000, "USD"),
            0
        );
    }

    #[test]
    fn coupon_discount_for_percent_never_exceeds_subtotal() {
        assert_eq!(
            coupon_discount_for(&coupon("percent", 15000, 0, vec![]), "p1", 8000, "USD"),
            8000
        );
        assert_eq!(
            coupon_discount_for(&coupon("percent", 10000, 0, vec![]), "p1", 8000, "USD"),
            8000
        );
    }

    #[test]
    fn coupon_discount_for_fixed_converts_with_currency() {
        // A fixed 50 JPY coupon is 50 minor units, not 5000; a
        // fixed 50 USD coupon is 5000 minor units. The fixed value
        // is stored in minor units, so no currency conversion
        // happens at the point of use any more.
        assert_eq!(coupon_discount_for(&coupon("fixed", 0, 50, vec![]), "p1", 10000, "JPY"), 50);
        assert_eq!(coupon_discount_for(&coupon("fixed", 0, 5000, vec![]), "p1", 10000, "USD"), 5000);
    }

    #[test]
    fn claim_coupon_use_is_atomic_under_the_limit() {
        let state = StoreState::new();
        let mut limited = coupon("percent", 1000, 0, vec![]);
        limited.usage_limit = Some(1);
        state.coupons.insert(limited.id.clone(), limited);
        let claimed = claim_coupon_use(&state, "save10").expect("first claim succeeds");
        assert_eq!(claimed.used_count, 1);
        assert_eq!(
            state.coupons.get("cp-1").map(|entry| entry.used_count),
            Some(1)
        );
        assert!(claim_coupon_use(&state, "save10").is_err());
        assert_eq!(
            state.coupons.get("cp-1").map(|entry| entry.used_count),
            Some(1),
            "a refused claim must not consume another use"
        );
    }

    #[test]
    fn claim_coupon_use_rejects_unknown_expired_and_inactive() {
        let state = StoreState::new();
        assert!(claim_coupon_use(&state, "nope").is_err());
        let now = Utc::now();
        let mut expired = coupon("percent", 1000, 0, vec![]);
        expired.id = "cp-expired".to_string();
        expired.code = "OLD".to_string();
        expired.expires_at = Some(now - chrono::Duration::days(1));
        state.coupons.insert(expired.id.clone(), expired);
        assert!(claim_coupon_use(&state, "old").is_err());
        let mut inactive = coupon("percent", 1000, 0, vec![]);
        inactive.id = "cp-off".to_string();
        inactive.code = "OFF".to_string();
        inactive.active = false;
        state.coupons.insert(inactive.id.clone(), inactive);
        assert!(claim_coupon_use(&state, "off").is_err());
    }
}
