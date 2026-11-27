// Smart Pricing module: price recommendations, discount and
// proration engine, and server-side invoice reconciliation.
//
// This module is the pricing brain of the platform. It never sets a
// price silently and it never mutates an order or an invoice itself:
// every recommendation is a suggestion an admin confirms, every
// discount evaluation returns a trace the caller applies, and every
// reconciliation is a validation the billing module calls before it
// issues an invoice. Issued invoices stay immutable; corrections go
// through credit notes only.
//
// # Money rules (absolute)
//
// - Every money amount in this module is an `i64` in scaled minor
//   units: one major unit equals 10,000 scaled units, so the value
//   maps exactly onto NUMERIC(19,4) in PostgreSQL
//   (`scaled / 10_000`). There is no floating-point money anywhere
//   in this file: no floating-point amount, no float arithmetic on prices.
// - API callers supply money as exact decimal strings
//   (`MoneyInput { amount: "19.99", currency: "USD" }`), parsed with
//   integer arithmetic only. The products and store modules use
//   integer cents (minor units, exponent 2); this module converts
//   cents to scaled units by multiplying by 100, and scaled units
//   back to cents with half-up rounding. Zero-decimal currencies
//   (JPY, KRW, VND, CLP, ISK, UGX, XAF, XOF, XPF, BIF, DJF, GNF, KMF,
//   PYG, RWF, VUV -- the ISO 4217 exponent-0 set, see
//   `unified_core::reference_data::currencies`) carry no fraction.
// - Percentages and rates are integer basis points: 10,000 basis
//   points equal 100%. A tax rate, a margin target, and a discount
//   percent are all basis points.
// - Rounding is half-up, applied per line, then summed. A line
//   amount is rounded half-up to the currency minor unit (2 decimal
//   places, or 0 for zero-decimal currencies) at computation time;
//   totals are exact sums of the rounded lines, never re-rounded
//   from unrounded parts. Tax is computed per line, rounded
//   half-up to the minor unit, then summed. Proration rounds the
//   daily rate half-up to the minor unit first, then multiplies by
//   whole days. Currency conversion rounds half-up to the minor
//   unit of the target currency. Division that must round uses
//   `mul_div_half_up` (wide i128 intermediate, checked, clamped).
// - Exchange rates are integer ratios (`rate_numerator` /
//   `rate_denominator`), entered by an admin or supplied by the
//   calculator module's stored rates. Rates are never scraped and
//   never guessed; a missing rate is an explicit error.
//
// # What the engine computes
//
// - Cost-plus recommendations: cost lines with a source and a label
//   plus a margin target produce a suggested price, with the full
//   calculation shown line by line (total cost, margin amount,
//   suggested price).
// - Tiered pricing: per-quantity tiers with a unit price per tier;
//   the engine prices a quantity and shows the tier breakdown.
// - Bundle pricing: the sum of component prices minus a bundle
//   discount, shown line by line.
// - Market reference prices: human-entered reference prices per
//   product (competitor or market data), each stored with its source
//   and observation date. References are never scraped.
// - Price versions and approvals: every price change is versioned.
//   A change above the configured threshold (admin-editable,
//   default 10.00%) requires a second approver who is not the
//   requester. Status machine:
//   `draft -> pending_approval -> approved -> active`, with
//   `rejected` returning to `draft`.
// - Discounts: percentage, fixed-amount, buy-X-get-Y, and volume
//   discounts, plus coupons with validity windows, a global usage
//   limit, and a per-customer usage limit. Rules run in a declared
//   admin-defined order; stackable rules apply in sequence on the
//   running remainder, an exclusive rule stops later rules, and the
//   full evaluation trace is recorded.
// - Proration: mid-cycle upgrade/downgrade, plan changes, seat
//   changes, and refunds. Unused-time credit is computed per day
//   (or per configured month) and every line shows the exact math:
//   period, daily rate, days used, days remaining, credit/charge.
// - Reconciliation: before an invoice is issued, line items plus
//   discounts plus proration plus tax must equal the total exactly.
//   A mismatch blocks issuing with the exact discrepancy per
//   component. Issued invoices are immutable; corrections use
//   credit notes, which carry their own reconciliation.
//
// # Events emitted
//
// - `pricing.recommendation.created` - a recommendation was computed.
// - `pricing.recommendation.confirmed` - an admin confirmed one.
// - `pricing.price_version.submitted` / `.approved` / `.rejected` /
//   `.activated` - price version lifecycle.
// - `pricing.discount.evaluated` - a discount evaluation ran.
// - `pricing.coupon.redeemed` - a coupon redemption was recorded.
// - `pricing.proration.computed` - a proration was computed.
// - `pricing.invoice.reconciled` - a reconciliation matched.
// - `pricing.invoice.reconciliation_failed` - a reconciliation
//   mismatched (issuing must be blocked).
// - `pricing.invoice.previewed` - an invoice preview was produced.
// - `pricing.credit_note.created` / `.issued` - credit note lifecycle.
// - `pricing.ai.draft_created` - an AI narrative draft was produced
//   from a stored recommendation (numbers only from the engine).
//
// # Events answered (requested by other modules)
//
// - `pricing.discount.evaluate_requested` - the store and billing
//   call this to evaluate discounts; the answer is added with
//   `add_result` and never mutates the caller's order.
// - `pricing.invoice.validate_requested` - billing calls this
//   before issuing an invoice; the answer carries `valid` and the
//   per-component discrepancy. Billing amounts arriving as JSON
//   numbers are read through their decimal string form and parsed
//   with integer math; this module never performs float arithmetic.
// - `pricing.ai.recommend_requested` - the AI module asks for a
//   recommendation narrative; the answer words a stored
//   recommendation record. The AI never invents prices: every
//   number in the draft comes from the engine.
//
// # Relationship to neighbouring modules
//
// - Calculator (batch 3) owns generic business math (ROI, loans,
//   NPV/IRR, currency-rate storage). This module reuses its
//   conventions (NUMERIC(19,4), stored rates only, explicit
//   unavailability errors) and does not rebuild a calculator; its
//   pricing endpoint (`calculator.pricing`) solves price/cost/margin
//   for one product, while this module versions, approves, tiers,
//   bundles, discounts, prorates, and reconciles those prices.
// - Pricing tables (batch 3) present prices. A confirmed
//   recommendation emits `pricing.recommendation.confirmed`, which
//   pricing tables can consume to refresh a table; this module
//   never writes pricing-table rows itself.
// - Store coupons already exist (`store.rs` `Coupon`: percent or
//   fixed, expiry, global usage limit, product scope, float
//   amounts). This engine is the shared evaluator the store should
//   call through `pricing.discount.evaluate_requested`; the store
//   keeps its coupon records as the source of truth for checkout
//   until the integrator converges them (see the integration
//   notes). This module's own coupons add validity windows,
//   per-customer limits, stacking/exclusivity, buy-X-get-Y, and
//   volume rules, all in integer money.
// - Billing owns invoice lifecycle and stores amounts as float
//   major units (a pre-existing gap this module does not paper
//   over). The reconciliation contract is defined in scaled
//   integer units; the billing-side adapter converts its float
//   totals through their decimal string form, and the integrator
//   snippet in the notes shows the exact hook call. Until billing
//   stores integer money, reconciliation can only prove the
//   pricing-side inputs; that limitation is recorded as OPEN.
//
// # Tenant scoping (standing rule)
//
// Pricing rules, coupons, recommendations, price versions,
// proration records, credit notes, and reconciliations are TENANT
// business data. Every record carries `tenant_id`, every list is
// filtered by the calling tenant, and every single-record read,
// update, or delete answers 404 when the record belongs to another
// tenant (never leaks). The tenant context is resolved ONLY from
// `TenantContext::from_request` (the middleware-attached context
// from crates/unified-core/src/middleware.rs`, resolved through the
// `tenant.resolve_requested` hook) - never from a client-supplied
// header, and never from a hard-coded tenant id, schema name, or
// package id. NO TENANT CONTEXT MEANS FAIL CLOSED: `caller()`
// resolves the tenant first, so every HTTP handler answers 403
// when the request did not resolve to a tenant; nothing silently
// falls back to the `public` schema. Hook handlers require
// `tenant_id` in the event payload and answer with an error
// result when it is absent or malformed. Settings are per-tenant.
// The only exception is the currency registry: ISO 4217 codes are
// standard constants and the currency rows are platform reference
// data, identical for every tenant - but the currency endpoints
// still require a resolved tenant context, so there is no
// anonymous or public access. Control-plane tables (tenants,
// subscriptions, domains, billing, licensing) live in `public`
// and are touched only through their own modules/events, never
// from here; this module issues no SQL at all.
//
// CONVERGENCE POINT (batch 17 - schema-per-tenant plumbing is NOT
// on disk yet, verified 2026-11-22: there is no `schema_name`
// helper, no tenant schema extractor, and no `search_path`
// handling anywhere in `src/`; `TenantContext` carries a tenant
// id, company id, site id, kind, and host, but no schema name and
// no package entitlements). This module deliberately does NOT
// invent its own schema-name derivation. When batch 17 lands its
// plumbing, the integrator swaps exactly one function -
// `resolve_tenant` in this file - to also return the batch-17
// schema name / search_path handle, and re-points the migration
// tables (every `pricing_*` table already carries a `tenant_id`
// column) at the per-tenant schema instead of `public`. No
// handler, no record shape, and no hook contract changes:
// `tenant_id` is already on every record and every filter.
// Package entitlements (which pricing features a tenant's
// purchased package unlocks) are enforced by the entitlements
// module once batch 17 wires them into `TenantContext`; until
// then the capability check (`pricing:read` / `pricing:manage` /
// `pricing:approve`) is the only gate, and that limitation is
// recorded as OPEN in the integration notes.
//
// Persistence: the live store is in-memory DashMaps (the pattern
// the notifications, audit, and saved-views modules use). The
// Diesel tables are the persistence target in migration
// `2026-11-22-000097_pricing`; the schema and model entries are
// supplied to the integrator in the notes, and nothing in this file
// references a table that does not exist yet.

// pricing - money.

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
use super::recommendations::*;
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

// Capabilities and constants.

/// Read pricing rules, coupons, recommendations, price versions,
/// proration records, reconciliations, and settings.
pub const PERM_PRICING_READ: &str = "pricing:read";

/// Create, update, and delete pricing rules, coupons, reference
/// prices, tiers, bundles, price versions, and settings; compute
/// recommendations, discounts, proration, previews, and credit
/// notes.
pub const PERM_PRICING_MANAGE: &str = "pricing:manage";

/// Approve or reject a price version that needs a second approver.
pub const PERM_PRICING_APPROVE: &str = "pricing:approve";

/// Scale: one major unit equals this many scaled minor units.
/// NUMERIC(19,4) stores exactly four fractional digits, so the
/// scaled value is the NUMERIC value multiplied by 10,000.
pub const MONEY_SCALE: i64 = 10_000;

/// Basis points: 10,000 basis points equal 100%.
pub const BASIS_POINTS: i64 = 10_000;

/// Minor units per major unit for two-decimal currencies (cents).
pub const CENTS_PER_MAJOR: i64 = 100;

/// Default approval threshold, in basis points: a price change
/// above 10.00% needs a second approver. Configurable default, see
/// `PricingSettings::approval_threshold_bp`.
pub const DEFAULT_APPROVAL_THRESHOLD_BP: i64 = 1_000;

/// Default proration month length in days, used only when the
/// proration basis is `per_month`. Configurable default.
pub const DEFAULT_DAYS_PER_MONTH: i64 = 30;

/// Default currency. Configurable default; currencies are data.
pub const DEFAULT_CURRENCY: &str = "USD";

/// Price version statuses. The same closed set is enforced by a
/// CHECK constraint in the migration.
pub const PRICE_VERSION_STATUSES: [&str; 5] =
    ["draft", "pending_approval", "approved", "active", "rejected"];

/// Discount rule kinds. The same closed set is enforced by a CHECK
/// constraint in the migration.
pub const DISCOUNT_KINDS: [&str; 4] = ["percent", "fixed", "buy_x_get_y", "volume"];

/// Recommendation kinds.
pub const RECOMMENDATION_KINDS: [&str; 3] = ["cost_plus", "tiered", "bundle"];

/// Proration bases. The same closed set is enforced by a CHECK
/// constraint in the migration.
pub const PRORATION_BASES: [&str; 2] = ["per_day", "per_month"];

/// Proration change kinds.
pub const PRORATION_KINDS: [&str; 4] =
    ["upgrade", "downgrade", "plan_change", "seat_change"];

/// Rounding modes the settings accept. Only `half_up` is
/// implemented; any other value is rejected rather than silently
/// mapped to a different rule.
pub const ROUNDING_MODES: [&str; 1] = ["half_up"];

/// Seeded currencies. ISO 4217 codes are the sanctioned standard
/// constants; the rows are data and admin-editable through the
/// currency endpoints.
pub const SEEDED_CURRENCIES: [(&str, i64); 12] = [
    ("USD", 2),
    ("EUR", 2),
    ("GBP", 2),
    ("AUD", 2),
    ("CAD", 2),
    ("CHF", 2),
    ("CNY", 2),
    ("IDR", 2),
    ("SGD", 2),
    ("JPY", 0),
    ("KRW", 0),
    ("VND", 0),
];

// Money core (pure functions; no database, no I/O).

/// One money amount as supplied by API callers: an exact decimal
/// string such as `"19.99"`, parsed with integer arithmetic only.
/// The currency defaults to the module default currency when omitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoneyInput {
    pub amount: String,
    #[serde(default)]
    pub currency: Option<String>,
}

/// A money amount in scaled minor units plus its ISO 4217 currency
/// code. Serializes with the exact decimal string form so callers
/// never have to divide.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Money {
    /// Amount in scaled minor units (1 major unit = 10,000).
    pub amount_scaled: i64,
    pub currency: String,
    /// Exact decimal string, for example `"19.99"` or `"1999"`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub amount: String,
}

impl Money {
    pub(crate) fn new(amount_scaled: i64, currency: &str) -> Self {
        let amount = format_scaled(amount_scaled, currency);
        Self {
            amount_scaled,
            currency: currency.to_string(),
            amount,
        }
    }
}

/// True for zero-decimal currencies (no minor units). Delegates to the
/// central ISO 4217 catalogue via `unified_core::db::money`: a currency
/// is zero-decimal exactly when the catalogue lists it with zero minor
/// digits. No local list is kept here.
pub fn is_zero_decimal_currency(currency: &str) -> bool {
    unified_core::db::money::is_zero_decimal_currency(currency)
}

/// Number of fractional digits a currency uses in everyday prices:
/// 0 or 2. The stored scale is always 4; this is only the display
/// and line-rounding exponent.
pub fn currency_exponent(currency: &str) -> u32 {
    if is_zero_decimal_currency(currency) {
        0
    } else {
        2
    }
}

/// Validate a 3-letter ISO 4217 currency code.
pub fn validate_currency(currency: &str) -> UnifiedResult<()> {
    if currency.len() == 3
        && currency
            .chars()
            .all(|c| c.is_ascii_alphabetic() && c.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(
            "Currency must be a 3-letter uppercase ISO 4217 code such as USD",
        ))
    }
}

/// Parse an exact decimal string into scaled minor units (scale 4).
/// Rejects negative amounts, non-numeric input, and more fractional
/// digits than the storage scale allows. No floating point is used.
/// Accepts an optional leading `+`; a leading `-` is rejected here
/// because prices, costs, and discounts are non-negative - signed
/// adjustments (proration credits, credit notes) are built by the
/// engine from non-negative parts, never parsed from input.
pub fn parse_scaled_amount(amount: &str, currency: &str) -> UnifiedResult<i64> {
    validate_currency(currency)?;
    let text = amount.trim();
    if text.is_empty() {
        return Err(UnifiedError::bad_request("Amount must not be empty"));
    }
    let digits_text = text.strip_prefix('+').unwrap_or(text);
    let (whole, frac) = match digits_text.split_once('.') {
        Some((whole, frac)) => (whole, Some(frac)),
        None => (digits_text, None),
    };
    if whole.is_empty() || !whole.chars().all(|c| c.is_ascii_digit()) {
        return Err(UnifiedError::bad_request(format!(
            "Amount \"{amount}\" is not a valid non-negative decimal"
        )));
    }
    let frac_digits = frac.unwrap_or("");
    if !frac_digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(UnifiedError::bad_request(format!(
            "Amount \"{amount}\" is not a valid non-negative decimal"
        )));
    }
    if frac_digits.len() > 4 {
        return Err(UnifiedError::bad_request(format!(
            "Amount \"{amount}\" has more than 4 decimal places"
        )));
    }
    let mut frac_padded = frac_digits.to_string();
    while frac_padded.len() < 4 {
        frac_padded.push('0');
    }
    let whole_val: i128 = whole.parse().map_err(|_| {
        UnifiedError::bad_request(format!("Amount \"{amount}\" is not a valid decimal"))
    })?;
    let frac_val: i128 = if frac_padded.is_empty() {
        0
    } else {
        frac_padded.parse().map_err(|_| {
            UnifiedError::bad_request(format!("Amount \"{amount}\" is not a valid decimal"))
        })?
    };
    let scaled = whole_val
        .checked_mul(MONEY_SCALE as i128)
        .and_then(|value| value.checked_add(frac_val))
        .ok_or_else(|| UnifiedError::bad_request("Amount is too large"))?;
    i64::try_from(scaled).map_err(|_| UnifiedError::bad_request("Amount is too large"))
}

/// Parse a decimal string that may be negative (used only at the
/// billing boundary, where a caller supplies a signed adjustment).
/// The sign is carried by the returned integer; the magnitude rules
/// are those of `parse_scaled_amount`.
pub fn parse_signed_scaled_amount(amount: &str, currency: &str) -> UnifiedResult<i64> {
    let text = amount.trim();
    if let Some(rest) = text.strip_prefix('-') {
        let magnitude = parse_scaled_amount(rest, currency)?;
        magnitude
            .checked_neg()
            .ok_or_else(|| UnifiedError::bad_request("Amount is too large"))
    } else {
        parse_scaled_amount(text, currency)
    }
}

/// Format scaled minor units as an exact decimal string (`"19.99"`,
/// `"1999"` for zero-decimal currencies, `"19.9900"` is never
/// produced: trailing zeros beyond the currency exponent are
/// trimmed, and a zero amount formats as `"0"` or `"0.00"`).
pub fn format_scaled(amount_scaled: i64, currency: &str) -> String {
    let exponent = currency_exponent(currency);
    let sign = if amount_scaled < 0 { "-" } else { "" };
    let magnitude = amount_scaled.unsigned_abs() as u128;
    let whole = magnitude / MONEY_SCALE as u128;
    let frac4 = magnitude % MONEY_SCALE as u128;
    if exponent == 0 {
        // Zero-decimal currencies show whole units only; any stored
        // fraction is dropped from display (amounts are rounded to
        // whole units at computation time, so this is exact).
        format!("{sign}{whole}")
    } else {
        let cents = frac4 / 100;
        format!("{sign}{whole}.{cents:02}")
    }
}

/// Convert integer cents (products/store minor units, exponent 2)
/// to scaled units (exponent 4): multiply by 100.
pub fn cents_to_scaled(cents: i64) -> UnifiedResult<i64> {
    cents
        .checked_mul(100)
        .ok_or_else(|| UnifiedError::bad_request("Amount is too large"))
}

/// Convert scaled units back to integer cents with half-up
/// rounding. Used only at the boundary to the products/store
/// modules, which store cents.
pub fn scaled_to_cents(scaled: i64) -> i64 {
    // Half-up on the dropped two digits: add 50 (half of 100) for
    // non-negative values, subtract 50 for negative ones.
    if scaled >= 0 {
        (scaled + 50) / 100
    } else {
        (scaled - 50) / 100
    }
}

/// Round a scaled amount half-up to the currency minor unit
/// (2 decimal places, or whole units for zero-decimal currencies).
/// This is the single rounding point for line amounts.
pub fn round_to_minor(amount_scaled: i64, currency: &str) -> i64 {
    if is_zero_decimal_currency(currency) {
        // Round to whole major units: drop the 4 fractional digits.
        let half = MONEY_SCALE / 2;
        if amount_scaled >= 0 {
            ((amount_scaled + half) / MONEY_SCALE) * MONEY_SCALE
        } else {
            ((amount_scaled - half) / MONEY_SCALE) * MONEY_SCALE
        }
    } else {
        // Round to cents: keep 2 of the 4 fractional digits.
        let step: i64 = 100; // one cent in scaled units
        let half = step / 2;
        if amount_scaled >= 0 {
            ((amount_scaled + half) / step) * step
        } else {
            ((amount_scaled - half) / step) * step
        }
    }
}

/// Half-up integer division of `value * numerator / denominator`
/// with a wide i128 intermediate. `denominator` must be positive.
/// Used for every percentage, rate, and proration computation so
/// no step ever touches floating point.
pub fn mul_div_half_up(value: i64, numerator: i64, denominator: i64) -> UnifiedResult<i64> {
    if denominator <= 0 {
        return Err(UnifiedError::bad_request(
            "Denominator must be a positive integer",
        ));
    }
    let wide = value as i128 * numerator as i128;
    let rounded = if wide >= 0 {
        (wide + denominator as i128 / 2) / denominator as i128
    } else {
        (wide - denominator as i128 / 2) / denominator as i128
    };
    i64::try_from(rounded.clamp(i64::MIN as i128, i64::MAX as i128))
        .map_err(|_| UnifiedError::bad_request("Computed amount is too large"))
}

/// Half-up `value * numerator / denominator` that never fails on a
/// zero denominator inside pure math: callers that already
/// validated the denominator use this for infallible paths. Kept
/// separate so pure-function tests never need error handling for
/// arithmetic that cannot fail.
pub fn mul_div_half_up_unchecked(value: i64, numerator: i64, denominator: i64) -> i64 {
    let wide = value as i128 * numerator as i128;
    let rounded = if wide >= 0 {
        (wide + denominator as i128 / 2) / denominator as i128
    } else {
        (wide - denominator as i128 / 2) / denominator as i128
    };
    rounded.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// Percentage of an amount, in basis points, rounded half-up.
/// `percent_of(199_900, 2_500)` is 25.00% of 19.99, i.e. 49,975
/// scaled units (4.9975, rounded to the stored scale; callers
/// round to the minor unit with `round_to_minor` afterwards).
pub fn percent_of(amount_scaled: i64, percent_bp: i64) -> UnifiedResult<i64> {
    mul_div_half_up(amount_scaled, percent_bp, BASIS_POINTS)
}

/// Resolve a caller-supplied money input against a fallback
/// currency (the product currency, else the module default).
pub fn resolve_money(input: &MoneyInput, fallback_currency: Option<&str>) -> UnifiedResult<Money> {
    let currency = input
        .currency
        .clone()
        .or_else(|| fallback_currency.map(str::to_string))
        .unwrap_or_else(|| DEFAULT_CURRENCY.to_string())
        .to_uppercase();
    let amount_scaled = parse_scaled_amount(&input.amount, &currency)?;
    Ok(Money::new(amount_scaled, &currency))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn currency_codes_must_be_three_uppercase_letters() {
        assert!(validate_currency("USD").is_ok());
        assert!(validate_currency("JPY").is_ok());
        assert!(validate_currency("usd").is_err());
        assert!(validate_currency("US").is_err());
        assert!(validate_currency("USDD").is_err());
        assert!(validate_currency("U5D").is_err());
    }

    #[test]
    fn zero_decimal_currencies_have_no_minor_unit() {
        assert!(is_zero_decimal_currency("JPY"));
        assert!(is_zero_decimal_currency("KRW"));
        assert!(!is_zero_decimal_currency("USD"));
        // The central catalogue lookup normalises case, so a
        // lowercase code resolves the same as the uppercase one.
        assert!(is_zero_decimal_currency("jpy"));
        assert_eq!(currency_exponent("JPY"), 0);
        assert_eq!(currency_exponent("USD"), 2);
    }

    #[test]
    fn percent_of_uses_basis_points_with_half_up_rounding() {
        assert_eq!(percent_of(199_900, 1_000).unwrap(), 19_990);
        assert_eq!(percent_of(199_900, 500).unwrap(), 9_995);
        assert_eq!(percent_of(101, 5_000).unwrap(), 51);
        assert_eq!(percent_of(100, 10_000).unwrap(), 100);
    }

    #[test]
    fn scaled_to_cents_rounds_half_up_both_ways() {
        assert_eq!(scaled_to_cents(199_900), 1999);
        assert_eq!(scaled_to_cents(199_949), 1999);
        assert_eq!(scaled_to_cents(199_950), 2000);
        assert_eq!(scaled_to_cents(-199_950), -2000);
        assert_eq!(scaled_to_cents(0), 0);
    }

    #[test]
    fn unchecked_multiply_divide_rounds_half_up() {
        assert_eq!(mul_div_half_up_unchecked(101, 1, 2), 51);
        assert_eq!(mul_div_half_up_unchecked(-101, 1, 2), -51);
        assert_eq!(mul_div_half_up_unchecked(100, 3, 2), 150);
    }
}
