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
//   (JPY, KRW, VND, CLP, ISK, UGX, XAF, XOF, XPF) carry no fraction.
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
// - `pricing.recommendation.created` — a recommendation was computed.
// - `pricing.recommendation.confirmed` — an admin confirmed one.
// - `pricing.price_version.submitted` / `.approved` / `.rejected` /
//   `.activated` — price version lifecycle.
// - `pricing.discount.evaluated` — a discount evaluation ran.
// - `pricing.coupon.redeemed` — a coupon redemption was recorded.
// - `pricing.proration.computed` — a proration was computed.
// - `pricing.invoice.reconciled` — a reconciliation matched.
// - `pricing.invoice.reconciliation_failed` — a reconciliation
//   mismatched (issuing must be blocked).
// - `pricing.invoice.previewed` — an invoice preview was produced.
// - `pricing.credit_note.created` / `.issued` — credit note lifecycle.
// - `pricing.ai.draft_created` — an AI narrative draft was produced
//   from a stored recommendation (numbers only from the engine).
//
// # Events answered (requested by other modules)
//
// - `pricing.discount.evaluate_requested` — the store and billing
//   call this to evaluate discounts; the answer is added with
//   `add_result` and never mutates the caller's order.
// - `pricing.invoice.validate_requested` — billing calls this
//   before issuing an invoice; the answer carries `valid` and the
//   per-component discrepancy. Billing amounts arriving as JSON
//   numbers are read through their decimal string form and parsed
//   with integer math; this module never performs float arithmetic.
// - `pricing.ai.recommend_requested` — the AI module asks for a
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
// from `src/core/middleware.rs`, resolved through the
// `tenant.resolve_requested` hook) — never from a client-supplied
// header, and never from a hard-coded tenant id, schema name, or
// package id. NO TENANT CONTEXT MEANS FAIL CLOSED: `caller()`
// resolves the tenant first, so every HTTP handler answers 403
// when the request did not resolve to a tenant; nothing silently
// falls back to the `public` schema. Hook handlers require
// `tenant_id` in the event payload and answer with an error
// result when it is absent or malformed. Settings are per-tenant.
// The only exception is the currency registry: ISO 4217 codes are
// standard constants and the currency rows are platform reference
// data, identical for every tenant — but the currency endpoints
// still require a resolved tenant context, so there is no
// anonymous or public access. Control-plane tables (tenants,
// subscriptions, domains, billing, licensing) live in `public`
// and are touched only through their own modules/events, never
// from here; this module issues no SQL at all.
//
// CONVERGENCE POINT (batch 17 — schema-per-tenant plumbing is NOT
// on disk yet, verified 2026-11-22: there is no `schema_name`
// helper, no tenant schema extractor, and no `search_path`
// handling anywhere in `src/`; `TenantContext` carries a tenant
// id, company id, site id, kind, and host, but no schema name and
// no package entitlements). This module deliberately does NOT
// invent its own schema-name derivation. When batch 17 lands its
// plumbing, the integrator swaps exactly one function —
// `resolve_tenant` in this file — to also return the batch-17
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

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::middleware::TenantContext;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Capabilities and constants.
// ---------------------------------------------------------------------------

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

/// Zero-decimal currencies (ISO 4217 codes with exponent 0). These
/// codes are standard constants, not behaviour: the list itself is
/// data in the `pricing_currencies` table and an admin may extend
/// it, but the seeded set matches the products, store, and payments
/// modules.
pub const ZERO_DECIMAL_CURRENCIES: [&str; 9] =
    ["JPY", "KRW", "VND", "CLP", "ISK", "UGX", "XAF", "XOF", "XPF"];

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

// ---------------------------------------------------------------------------
// Money core (pure functions; no database, no I/O).
// ---------------------------------------------------------------------------

/// One money amount as supplied by API callers: an exact decimal
/// string such as `"19.99"`, parsed with integer arithmetic only.
/// The currency defaults to the module default currency when omitted.
#[derive(Debug, Clone, Deserialize)]
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
    fn new(amount_scaled: i64, currency: &str) -> Self {
        let amount = format_scaled(amount_scaled, currency);
        Self {
            amount_scaled,
            currency: currency.to_string(),
            amount,
        }
    }
}

/// True for zero-decimal currencies (no minor units).
pub fn is_zero_decimal_currency(currency: &str) -> bool {
    ZERO_DECIMAL_CURRENCIES.contains(&currency)
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
/// because prices, costs, and discounts are non-negative — signed
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
    fn new(label: &str, amount_scaled: i64, currency: &str, detail: Option<String>) -> Self {
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
/// 1, must not overlap, and must share one currency. A quantity
/// above the highest bounded tier is an error unless an unbounded
/// tier covers it.
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
        if let Some(prev) = previous_max {
            if tier.min_quantity <= prev {
                return Err(UnifiedError::bad_request(
                    "Tiers must not overlap: each tier must start above the previous tier maximum",
                ));
            }
        }
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
        let capacity = match tier.max_quantity {
            Some(max) => max.saturating_sub(tier.min_quantity) + 1,
            None => u32::MAX,
        };
        let units = remaining.min(capacity);
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

// ---------------------------------------------------------------------------
// Discount engine (pure).
// ---------------------------------------------------------------------------

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
fn discount_line_total(line: &DiscountLineInput, currency: &str) -> UnifiedResult<i64> {
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
fn rule_removal(
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
    // excludes the change day.
    let days_used = input
        .change_at
        .signed_duration_since(input.period_start)
        .num_days()
        .max(0);
    let days_remaining = (period_days - days_used).max(0);
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
    let net_due = new_charge - unused_credit;
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

// ---------------------------------------------------------------------------
// Invoice reconciliation (pure).
// ---------------------------------------------------------------------------

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
fn split_line_tax(
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

// ---------------------------------------------------------------------------
// Stored types.
// ---------------------------------------------------------------------------

/// Admin-editable module settings. Every value is a configurable
/// default: the code never hard-codes a threshold, a basis, or a
/// currency beyond the documented defaults below.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingSettings {
    /// Rounding rule. Only `half_up` is implemented.
    pub rounding_mode: String,
    /// `per_day` or `per_month`.
    pub proration_basis: String,
    /// Days in one month when the proration basis is `per_month`.
    pub days_per_month: i64,
    /// Price change above this many basis points needs a second
    /// approver.
    pub approval_threshold_bp: i64,
    /// Whether a second approver is required at all; when false,
    /// a price version above the threshold still records the
    /// threshold breach in its audit trail but may be approved by
    /// the requester.
    pub require_second_approver: bool,
    pub default_currency: String,
}

impl Default for PricingSettings {
    fn default() -> Self {
        Self {
            rounding_mode: "half_up".to_string(),
            proration_basis: "per_day".to_string(),
            days_per_month: DEFAULT_DAYS_PER_MONTH,
            approval_threshold_bp: DEFAULT_APPROVAL_THRESHOLD_BP,
            require_second_approver: true,
            default_currency: DEFAULT_CURRENCY.to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePricingSettings {
    #[serde(default)]
    pub rounding_mode: Option<String>,
    #[serde(default)]
    pub proration_basis: Option<String>,
    #[serde(default)]
    pub days_per_month: Option<i64>,
    #[serde(default)]
    pub approval_threshold_bp: Option<i64>,
    #[serde(default)]
    pub require_second_approver: Option<bool>,
    #[serde(default)]
    pub default_currency: Option<String>,
}

/// One currency the platform prices in. ISO 4217 codes are the
/// standard constants; the rows, the exponents, and the exchange
/// rate ratios are data. The rate is relative to the module
/// default currency and is entered by an admin only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingCurrency {
    pub code: String,
    /// Fractional digits in everyday prices: 0 or 2.
    pub exponent: i64,
    pub active: bool,
    /// Exchange-rate numerator relative to the default currency.
    #[serde(default)]
    pub rate_numerator: Option<i64>,
    /// Exchange-rate denominator relative to the default currency.
    #[serde(default)]
    pub rate_denominator: Option<i64>,
    #[serde(default)]
    pub rate_fetched_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePricingCurrency {
    pub code: String,
    #[serde(default)]
    pub exponent: Option<i64>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub rate_numerator: Option<i64>,
    #[serde(default)]
    pub rate_denominator: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePricingCurrency {
    #[serde(default)]
    pub exponent: Option<i64>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub rate_numerator: Option<i64>,
    #[serde(default)]
    pub rate_denominator: Option<i64>,
}

/// One discount rule. Rules are data: kind, order, scope, and
/// limits are admin-editable, never baked into code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingRule {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub name: String,
    /// `percent`, `fixed`, `buy_x_get_y`, or `volume`.
    pub kind: String,
    #[serde(default)]
    pub percent_bp: i64,
    /// Fixed amount in scaled minor units (kind `fixed` only).
    #[serde(default)]
    pub amount_scaled: i64,
    pub currency: String,
    #[serde(default)]
    pub buy_quantity: u32,
    #[serde(default)]
    pub get_quantity: u32,
    #[serde(default)]
    pub min_quantity: u32,
    /// Minimum order subtotal (scaled) for the rule to apply.
    #[serde(default)]
    pub min_subtotal_scaled: i64,
    /// Empty means the rule applies to every product.
    #[serde(default)]
    pub product_ids: Vec<String>,
    /// Declared evaluation order; lower runs first.
    pub evaluation_order: i64,
    pub stackable: bool,
    pub exclusive: bool,
    /// `active`, `draft`, or `archived`.
    pub status: String,
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_to: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePricingRule {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub percent_bp: i64,
    #[serde(default)]
    pub amount: Option<MoneyInput>,
    #[serde(default)]
    pub buy_quantity: u32,
    #[serde(default)]
    pub get_quantity: u32,
    #[serde(default)]
    pub min_quantity: u32,
    #[serde(default)]
    pub min_subtotal: Option<MoneyInput>,
    #[serde(default)]
    pub product_ids: Vec<String>,
    #[serde(default)]
    pub evaluation_order: i64,
    #[serde(default)]
    pub stackable: Option<bool>,
    #[serde(default)]
    pub exclusive: Option<bool>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_to: Option<DateTime<Utc>>,
    #[serde(default)]
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePricingRule {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub percent_bp: Option<i64>,
    #[serde(default)]
    pub amount: Option<MoneyInput>,
    #[serde(default)]
    pub buy_quantity: Option<u32>,
    #[serde(default)]
    pub get_quantity: Option<u32>,
    #[serde(default)]
    pub min_quantity: Option<u32>,
    #[serde(default)]
    pub min_subtotal: Option<MoneyInput>,
    #[serde(default)]
    pub product_ids: Option<Vec<String>>,
    #[serde(default)]
    pub evaluation_order: Option<i64>,
    #[serde(default)]
    pub stackable: Option<bool>,
    #[serde(default)]
    pub exclusive: Option<bool>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_to: Option<DateTime<Utc>>,
}

/// One coupon. Codes are stored uppercase and unique. A coupon is
/// a discount rule with a code, a validity window, a global usage
/// limit, and a per-customer usage limit; evaluation normalizes it
/// into rule shape so coupons and rules share one engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingCoupon {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub code: String,
    /// `percent`, `fixed`, or `buy_x_get_y`.
    pub kind: String,
    #[serde(default)]
    pub percent_bp: i64,
    /// Fixed amount in scaled minor units (kind `fixed` only).
    #[serde(default)]
    pub amount_scaled: i64,
    pub currency: String,
    #[serde(default)]
    pub buy_quantity: u32,
    #[serde(default)]
    pub get_quantity: u32,
    /// Empty means the coupon applies to every product.
    #[serde(default)]
    pub product_ids: Vec<String>,
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_to: Option<DateTime<Utc>>,
    /// Maximum redemptions across all customers; `None` is unlimited.
    #[serde(default)]
    pub usage_limit: Option<u32>,
    /// Maximum redemptions per customer; `None` is unlimited.
    #[serde(default)]
    pub per_customer_limit: Option<u32>,
    pub used_count: u32,
    pub stackable: bool,
    pub exclusive: bool,
    pub active: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePricingCoupon {
    pub code: String,
    pub kind: String,
    #[serde(default)]
    pub percent_bp: i64,
    #[serde(default)]
    pub amount: Option<MoneyInput>,
    #[serde(default)]
    pub buy_quantity: u32,
    #[serde(default)]
    pub get_quantity: u32,
    #[serde(default)]
    pub product_ids: Vec<String>,
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_to: Option<DateTime<Utc>>,
    #[serde(default)]
    pub usage_limit: Option<u32>,
    #[serde(default)]
    pub per_customer_limit: Option<u32>,
    #[serde(default)]
    pub stackable: Option<bool>,
    #[serde(default)]
    pub exclusive: Option<bool>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePricingCoupon {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub percent_bp: Option<i64>,
    #[serde(default)]
    pub amount: Option<MoneyInput>,
    #[serde(default)]
    pub buy_quantity: Option<u32>,
    #[serde(default)]
    pub get_quantity: Option<u32>,
    #[serde(default)]
    pub product_ids: Option<Vec<String>>,
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_to: Option<DateTime<Utc>>,
    #[serde(default)]
    pub usage_limit: Option<u32>,
    #[serde(default)]
    pub per_customer_limit: Option<u32>,
    #[serde(default)]
    pub stackable: Option<bool>,
    #[serde(default)]
    pub exclusive: Option<bool>,
    #[serde(default)]
    pub active: Option<bool>,
}

/// One customer's redemption count for one coupon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CouponRedemption {
    pub coupon_id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub customer_id: String,
    pub count: u32,
    pub last_redeemed_at: DateTime<Utc>,
}

/// One stored cost line of a recommendation (the durable form of
/// `CostLineInput`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingCostLine {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub recommendation_id: String,
    pub label: String,
    pub source: String,
    pub cost_scaled: i64,
    pub currency: String,
}

/// One price recommendation: a suggestion only. The engine never
/// sets a price; an admin confirms the suggestion, which creates
/// a price version in `draft`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingRecommendation {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub product_id: String,
    /// `cost_plus`, `tiered`, or `bundle`.
    pub kind: String,
    pub suggested_price_scaled: i64,
    pub currency: String,
    pub suggested_price: String,
    /// The full calculation, line by line.
    pub lines: Vec<CalculationLine>,
    /// The inputs that produced the recommendation, for audit.
    pub inputs: Value,
    /// `suggested`, `confirmed`, or `discarded`.
    pub status: String,
    pub created_by: String,
    #[serde(default)]
    pub confirmed_by: Option<String>,
    #[serde(default)]
    pub price_version_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCostPlusRecommendation {
    pub product_id: String,
    pub costs: Vec<CostLineInput>,
    pub margin_bp: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTieredRecommendation {
    pub product_id: String,
    pub tiers: Vec<TierInput>,
    pub quantity: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateBundleRecommendation {
    pub product_id: String,
    pub components: Vec<BundleComponentInput>,
    pub discount: BundleDiscountInput,
}

/// One stored tier of a tiered price definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingTier {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub product_id: String,
    pub min_quantity: u32,
    #[serde(default)]
    pub max_quantity: Option<u32>,
    pub unit_price_scaled: i64,
    pub currency: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePricingTier {
    pub product_id: String,
    pub min_quantity: u32,
    #[serde(default)]
    pub max_quantity: Option<u32>,
    pub unit_price: MoneyInput,
}

/// One bundle definition: a named set of products sold together.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingBundle {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub name: String,
    pub product_id: String,
    /// `percent` or `fixed`.
    pub discount_kind: String,
    #[serde(default)]
    pub discount_percent_bp: i64,
    #[serde(default)]
    pub discount_amount_scaled: i64,
    pub currency: String,
    pub active: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePricingBundle {
    pub name: String,
    pub product_id: String,
    pub discount_kind: String,
    #[serde(default)]
    pub discount_percent_bp: i64,
    #[serde(default)]
    pub discount_amount: Option<MoneyInput>,
    #[serde(default)]
    pub components: Vec<BundleComponentInput>,
    #[serde(default)]
    pub active: Option<bool>,
}

/// One component of a stored bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingBundleComponent {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub bundle_id: String,
    pub product_id: String,
    pub label: String,
    pub price_scaled: i64,
    pub currency: String,
}

/// One human-entered market reference price. The source and the
/// observation date are mandatory: a reference without provenance
/// is rejected. References are never scraped and never guessed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingReferencePrice {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub product_id: String,
    pub source: String,
    #[serde(default)]
    pub source_url: Option<String>,
    pub price_scaled: i64,
    pub currency: String,
    pub price: String,
    pub observed_at: DateTime<Utc>,
    #[serde(default)]
    pub note: String,
    pub entered_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateReferencePrice {
    pub product_id: String,
    pub source: String,
    #[serde(default)]
    pub source_url: Option<String>,
    pub price: MoneyInput,
    pub observed_at: DateTime<Utc>,
    #[serde(default)]
    pub note: Option<String>,
}

/// One versioned price for a product.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingPriceVersion {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub product_id: String,
    pub version: u32,
    pub price_scaled: i64,
    pub currency: String,
    pub price: String,
    /// Previous active price in scaled units; 0 when there was none.
    pub previous_price_scaled: i64,
    /// Change in basis points of the previous price; `None` when
    /// the previous price was zero and the new price is above zero
    /// (unmeasurable change: always needs a second approver).
    #[serde(default)]
    pub change_bp: Option<i64>,
    /// True when the change exceeded the approval threshold.
    pub above_threshold: bool,
    /// `draft`, `pending_approval`, `approved`, `active`, or `rejected`.
    pub status: String,
    pub requested_by: String,
    #[serde(default)]
    pub approved_by: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub recommendation_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePriceVersion {
    pub product_id: String,
    pub price: MoneyInput,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub recommendation_id: Option<String>,
}

/// One approval decision on a price version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingPriceApproval {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub price_version_id: String,
    pub approver: String,
    /// `approved` or `rejected`.
    pub decision: String,
    #[serde(default)]
    pub reason: Option<String>,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PriceApprovalInput {
    #[serde(default)]
    pub reason: Option<String>,
}

/// One computed proration record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingProration {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    #[serde(default)]
    pub subscription_id: Option<String>,
    #[serde(default)]
    pub customer_id: Option<String>,
    pub kind: String,
    pub basis: String,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub change_at: DateTime<Utc>,
    pub period_days: i64,
    pub days_used: i64,
    pub days_remaining: i64,
    pub old_daily_rate_scaled: i64,
    pub new_daily_rate_scaled: i64,
    pub unused_credit_scaled: i64,
    pub new_charge_scaled: i64,
    pub net_due_scaled: i64,
    pub currency: String,
    pub lines: Vec<CalculationLine>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ComputeProrationRequest {
    #[serde(default)]
    pub subscription_id: Option<String>,
    #[serde(default)]
    pub customer_id: Option<String>,
    pub kind: String,
    pub old_price: MoneyInput,
    pub new_price: MoneyInput,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub change_at: DateTime<Utc>,
    #[serde(default)]
    pub basis: Option<String>,
}

/// One credit note: the only way to correct an issued invoice.
/// Credit notes are immutable once issued.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingCreditNote {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    pub number: String,
    pub invoice_id: String,
    pub amount_scaled: i64,
    pub currency: String,
    pub amount: String,
    pub lines: Vec<ReconcileAdjustmentInput>,
    pub reason: String,
    /// `draft` or `issued`.
    pub status: String,
    /// True when the note's own reconciliation matched at issue time.
    pub reconciled: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCreditNote {
    pub invoice_id: String,
    pub amount: MoneyInput,
    pub lines: Vec<ReconcileAdjustmentInput>,
    pub reason: String,
    /// When true the note is issued immediately (and becomes
    /// immutable); otherwise it stays a draft.
    #[serde(default)]
    pub issue: Option<bool>,
}

/// One stored reconciliation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingReconciliation {
    pub id: String,
    /// Owning tenant (uuidv7), resolved from the tenant context.
    pub tenant_id: String,
    #[serde(default)]
    pub invoice_id: Option<String>,
    pub matched: bool,
    pub currency: String,
    pub components: Vec<ReconciliationComponent>,
    pub total_discrepancy_scaled: i64,
    pub checked_by: String,
    pub checked_at: DateTime<Utc>,
}

/// Request bodies for the discount, preview, and reconcile
/// endpoints.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscountEvaluateRequest {
    pub lines: Vec<DiscountLineRequest>,
    #[serde(default)]
    pub coupon_code: Option<String>,
    #[serde(default)]
    pub customer_id: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
    /// Extra rule ids to evaluate, in addition to every active
    /// rule. Active rules always run; listing one here is only
    /// needed when the caller wants a draft rule evaluated.
    #[serde(default)]
    pub rule_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscountLineRequest {
    pub product_id: String,
    pub unit_price: MoneyInput,
    pub quantity: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InvoicePreviewRequest {
    #[serde(default)]
    pub invoice_id: Option<String>,
    pub lines: Vec<ReconcileLineRequest>,
    #[serde(default)]
    pub discounts: Vec<ReconcileAdjustmentInput>,
    #[serde(default)]
    pub proration_lines: Vec<ReconcileAdjustmentInput>,
    pub claimed: ReconcileClaimRequest,
    #[serde(default)]
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReconcileLineRequest {
    pub name: String,
    pub quantity: u32,
    pub unit_price: MoneyInput,
    #[serde(default)]
    pub tax_bp: i64,
    #[serde(default)]
    pub tax_inclusive: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReconcileClaimRequest {
    pub subtotal: MoneyInput,
    pub discounts: MoneyInput,
    pub proration: MoneyInput,
    pub tax_total: MoneyInput,
    pub total: MoneyInput,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConvertCurrencyRequest {
    pub amount: MoneyInput,
    pub target_currency: String,
}

/// Response of the AI narrative draft endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiDraftResponse {
    pub recommendation_id: String,
    pub draft: String,
    /// The numbers the draft uses, so a caller can verify that the
    /// text matches the engine output exactly.
    pub numbers: Value,
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

pub struct PricingState {
    hooks: RwLock<Arc<HookBus>>,
    /// Pricing settings, one row per tenant, keyed by tenant id.
    settings: DashMap<String, PricingSettings>,
    currencies: DashMap<String, PricingCurrency>,
    rules: DashMap<String, PricingRule>,
    coupons: DashMap<String, PricingCoupon>,
    redemptions: DashMap<String, CouponRedemption>,
    recommendations: DashMap<String, PricingRecommendation>,
    cost_lines: DashMap<String, PricingCostLine>,
    tiers: DashMap<String, PricingTier>,
    bundles: DashMap<String, PricingBundle>,
    bundle_components: DashMap<String, PricingBundleComponent>,
    reference_prices: DashMap<String, PricingReferencePrice>,
    price_versions: DashMap<String, PricingPriceVersion>,
    price_approvals: DashMap<String, PricingPriceApproval>,
    prorations: DashMap<String, PricingProration>,
    credit_notes: DashMap<String, PricingCreditNote>,
    reconciliations: DashMap<String, PricingReconciliation>,
    credit_seq: RwLock<u64>,
}

impl PricingState {
    fn new() -> Self {
        let state = Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            settings: DashMap::new(),
            currencies: DashMap::new(),
            rules: DashMap::new(),
            coupons: DashMap::new(),
            redemptions: DashMap::new(),
            recommendations: DashMap::new(),
            cost_lines: DashMap::new(),
            tiers: DashMap::new(),
            bundles: DashMap::new(),
            bundle_components: DashMap::new(),
            reference_prices: DashMap::new(),
            price_versions: DashMap::new(),
            price_approvals: DashMap::new(),
            prorations: DashMap::new(),
            credit_notes: DashMap::new(),
            reconciliations: DashMap::new(),
            credit_seq: RwLock::new(0),
        };
        for (code, exponent) in SEEDED_CURRENCIES {
            let now = Utc::now();
            state.currencies.insert(
                code.to_string(),
                PricingCurrency {
                    code: code.to_string(),
                    exponent,
                    active: true,
                    rate_numerator: if code == DEFAULT_CURRENCY { Some(1) } else { None },
                    rate_denominator: if code == DEFAULT_CURRENCY { Some(1) } else { None },
                    rate_fetched_at: None,
                    created_at: now,
                    updated_at: now,
                },
            );
        }
        state
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Settings for one tenant, lazily seeded with the
    /// configurable defaults on first use. There is no global
    /// settings row: a tenant without a context never reaches
    /// this function, because `caller()` fails closed first.
    fn settings_for(&self, tenant_id: &str) -> PricingSettings {
        self.settings
            .entry(tenant_id.to_string())
            .or_insert_with(PricingSettings::default)
            .clone()
    }

    fn next_credit_number(&self) -> String {
        let mut seq = self.credit_seq.write().expect("credit sequence lock poisoned");
        *seq += 1;
        format!("CN-{:06}", *seq)
    }

    /// Active discount rules for one tenant, in declared
    /// evaluation order. Rules of other tenants are never returned.
    fn active_rules(&self, tenant_id: &str) -> Vec<DiscountRuleInput> {
        let now = Utc::now();
        let mut rules: Vec<DiscountRuleInput> = self
            .rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.tenant_id == tenant_id)
            .filter(|rule| rule.status == "active")
            .filter(|rule| {
                rule.valid_from.is_none_or(|from| now >= from)
                    && rule.valid_to.is_none_or(|to| now <= to)
            })
            .map(|rule| rule_to_engine(&rule))
            .collect();
        rules.sort_by(|a, b| {
            a.evaluation_order
                .cmp(&b.evaluation_order)
                .then(a.rule_id.cmp(&b.rule_id))
        });
        rules
    }

    /// The next version number for a product's price in one tenant.
    fn next_price_version(&self, tenant_id: &str, product_id: &str) -> u32 {
        self.price_versions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|version| version.tenant_id == tenant_id && version.product_id == product_id)
            .map(|version| version.version)
            .max()
            .unwrap_or(0)
            + 1
    }

    /// The currently active price for a product in one tenant, if any.
    fn active_price_for(&self, tenant_id: &str, product_id: &str) -> Option<PricingPriceVersion> {
        self.price_versions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|version| {
                version.tenant_id == tenant_id
                    && version.product_id == product_id
                    && version.status == "active"
            })
            .max_by_key(|version| version.version)
    }

    fn record_reconciliation(
        &self,
        tenant_id: &str,
        invoice_id: Option<String>,
        result: &ReconciliationResult,
        actor: &str,
    ) -> PricingReconciliation {
        let record = PricingReconciliation {
            id: new_id(),
            tenant_id: tenant_id.to_string(),
            invoice_id,
            matched: result.matched,
            currency: result.currency.clone(),
            components: result.components.clone(),
            total_discrepancy_scaled: result.total_discrepancy_scaled,
            checked_by: actor.to_string(),
            checked_at: Utc::now(),
        };
        self.reconciliations
            .insert(record.id.clone(), record.clone());
        record
    }
}

/// Normalize a stored rule into the shape the pure engine takes.
fn rule_to_engine(rule: &PricingRule) -> DiscountRuleInput {
    DiscountRuleInput {
        rule_id: rule.id.clone(),
        name: rule.name.clone(),
        kind: rule.kind.clone(),
        percent_bp: rule.percent_bp,
        amount_scaled: rule.amount_scaled,
        buy_quantity: rule.buy_quantity,
        get_quantity: rule.get_quantity,
        min_quantity: rule.min_quantity,
        min_subtotal_scaled: rule.min_subtotal_scaled,
        product_ids: rule.product_ids.clone(),
        evaluation_order: rule.evaluation_order,
        stackable: rule.stackable,
        exclusive: rule.exclusive,
    }
}

/// Normalize a coupon into rule shape. Coupons always evaluate
/// after stored rules (evaluation order 1,000,000 plus the coupon
/// creation order is not tracked, so coupons share one order and
/// are tied by rule id).
fn coupon_to_engine(coupon: &PricingCoupon) -> DiscountRuleInput {
    DiscountRuleInput {
        rule_id: format!("coupon:{}", coupon.id),
        name: format!("Coupon {}", coupon.code),
        kind: coupon.kind.clone(),
        percent_bp: coupon.percent_bp,
        amount_scaled: coupon.amount_scaled,
        buy_quantity: coupon.buy_quantity,
        get_quantity: coupon.get_quantity,
        min_quantity: 0,
        min_subtotal_scaled: 0,
        product_ids: coupon.product_ids.clone(),
        evaluation_order: 1_000_000,
        stackable: coupon.stackable,
        exclusive: coupon.exclusive,
    }
}

/// Validate a coupon for one customer at one moment: active,
/// inside its validity window, under its global limit, and under
/// the customer's limit. Returns the reason when unusable.
fn coupon_usability(
    state: &PricingState,
    coupon: &PricingCoupon,
    customer_id: Option<&str>,
    now: DateTime<Utc>,
) -> Option<String> {
    if !coupon.active {
        return Some("Coupon is not active".to_string());
    }
    if let Some(from) = coupon.valid_from {
        if now < from {
            return Some("Coupon is not yet valid".to_string());
        }
    }
    if let Some(to) = coupon.valid_to {
        if now > to {
            return Some("Coupon has expired".to_string());
        }
    }
    if let Some(limit) = coupon.usage_limit {
        if coupon.used_count >= limit {
            return Some("Coupon usage limit has been reached".to_string());
        }
    }
    if let (Some(limit), Some(customer)) = (coupon.per_customer_limit, customer_id) {
        let key = format!("{}:{}:{}", coupon.tenant_id, coupon.id, customer);
        if let Some(entry) = state.redemptions.get(&key) {
            if entry.count >= limit {
                return Some("Per-customer coupon usage limit has been reached".to_string());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// The caller, from the headers every module in this codebase uses:
/// `X-User-Id` identifies the actor, `X-Role` carries the role slug
/// or id the capability check runs against.
struct Caller {
    actor: String,
    role: String,
    /// Tenant id (uuidv7) from the middleware-attached context.
    /// Every handler scopes its reads and writes to this tenant.
    tenant_id: String,
}

/// Resolve the tenant context for a request. This is the single
/// choke point for tenant scoping: it reads ONLY the
/// middleware-attached `TenantContext` (resolved through the
/// `tenant.resolve_requested` hook), never a client header, and
/// never a hard-coded id. No tenant context means fail closed
/// (403): pricing data is tenant business data and must never
/// fall back to the `public` schema.
///
/// CONVERGENCE POINT (batch 17): when the schema-per-tenant
/// plumbing lands, this is the one function the integrator swaps
/// to also return the batch-17 schema name / search_path handle.
/// No handler changes: every handler already takes its tenant
/// from the `Caller` this function feeds.
fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<TenantContext> {
    TenantContext::from_request(req).ok_or_else(|| {
        UnifiedError::forbidden(
            "No tenant context resolved for this request: pricing data is tenant-scoped and the request host did not resolve to a tenant",
        )
    })
}

/// Validate a tenant id carried in a hook payload: a uuidv7 in
/// canonical form (36 characters, hyphens at 8/13/18/23, version
/// nibble `7`). The id itself is never hard-coded; this only
/// checks the shape so a malformed payload fails closed instead
/// of scoping reads to a nonsense tenant.
pub fn validate_tenant_id(value: &str) -> UnifiedResult<String> {
    let tenant_id = value.trim().to_string();
    let chars: Vec<char> = tenant_id.chars().collect();
    let well_formed = chars.len() == 36
        && chars[8] == '-'
        && chars[13] == '-'
        && chars[18] == '-'
        && chars[23] == '-'
        && chars[14] == '7'
        && chars
            .iter()
            .enumerate()
            .all(|(index, c)| {
                index == 8 || index == 13 || index == 18 || index == 23 || c.is_ascii_hexdigit()
            });
    if well_formed {
        Ok(tenant_id)
    } else {
        Err(UnifiedError::bad_request(
            "tenant_id must be a uuidv7 in canonical form",
        ))
    }
}

/// Read the tenant id from a hook payload, failing closed when it
/// is absent or malformed.
fn hook_tenant_id(ctx: &HookContext) -> UnifiedResult<String> {
    match ctx.get("tenant_id").and_then(|value| value.as_str()) {
        Some(value) => validate_tenant_id(value),
        None => Err(UnifiedError::bad_request(
            "Hook payload must carry tenant_id: pricing data is tenant-scoped",
        )),
    }
}

/// Ownership check for a single record: a record owned by another
/// tenant answers 404 (not found), never leaks its existence.
fn require_same_tenant(record_tenant_id: &str, tenant_id: &str) -> UnifiedResult<()> {
    if record_tenant_id == tenant_id {
        Ok(())
    } else {
        Err(UnifiedError::not_found("Record not found"))
    }
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let tenant = resolve_tenant(req)?;
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller {
        actor,
        role,
        tenant_id: tenant.tenant_id,
    })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.chars().count() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn require_currency_known(state: &PricingState, currency: &str) -> UnifiedResult<()> {
    validate_currency(currency)?;
    match state.currencies.get(currency) {
        Some(entry) if entry.active => Ok(()),
        _ => Err(UnifiedError::bad_request(format!(
            "Currency {currency} is not an active pricing currency"
        ))),
    }
}

fn normalize_code(code: &str) -> UnifiedResult<String> {
    let normalized = code.trim().to_uppercase();
    if normalized.is_empty() || normalized.chars().count() > 64 {
        return Err(UnifiedError::bad_request(
            "Coupon code must be 1 to 64 characters",
        ));
    }
    if !normalized
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(UnifiedError::bad_request(
            "Coupon code may only contain letters, digits, hyphens, and underscores",
        ));
    }
    Ok(normalized)
}

/// Read a JSON value that may be an integer, a decimal string, or
/// a JSON number, and parse it as scaled minor units without ever
/// using float arithmetic. JSON numbers are read through their
/// decimal string form (`Value::to_string`), so a billing float
/// such as 19.99 becomes the string "19.99" and is parsed with
/// integer math. Values with more than 4 fractional digits are
/// rejected rather than silently rounded.
fn value_to_scaled(value: &Value, currency: &str, field: &str) -> UnifiedResult<i64> {
    match value {
        Value::String(text) => parse_signed_scaled_amount(text, currency),
        Value::Number(number) => {
            let text = number.to_string();
            parse_signed_scaled_amount(&text, currency)
        }
        Value::Null => Err(UnifiedError::bad_request(format!(
            "{field} must not be null"
        ))),
        _ => Err(UnifiedError::bad_request(format!(
            "{field} must be a number or a decimal string"
        ))),
    }
}

/// Build the AI narrative draft for a recommendation. The text is
/// assembled from the stored recommendation record only: every
/// number in the draft is copied from the engine output. The AI
/// (or the admin reading the draft) never invents a price.
fn build_ai_narrative(recommendation: &PricingRecommendation) -> String {
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

// ---------------------------------------------------------------------------
// Shared evaluation (used by the HTTP endpoint and the hook).
// ---------------------------------------------------------------------------

/// Evaluate discounts for a caller that supplies lines, an
/// optional coupon code, and an optional customer id. Active rules
/// always run; the coupon runs last when usable. Returns the
/// evaluation plus, when a coupon was requested but unusable, the
/// reason (the evaluation still runs without it).
fn evaluate_for_caller(
    state: &PricingState,
    tenant_id: &str,
    lines: &[DiscountLineInput],
    coupon_code: Option<&str>,
    customer_id: Option<&str>,
    currency: &str,
    extra_rule_ids: &[String],
) -> UnifiedResult<(DiscountEvaluation, Option<String>)> {
    let mut rules = state.active_rules(tenant_id);
    for rule_id in extra_rule_ids {
        if let Some(rule) = state.rules.get(rule_id) {
            if rule.tenant_id != tenant_id {
                continue;
            }
            let engine_rule = rule_to_engine(&rule);
            if !rules.iter().any(|item| item.rule_id == engine_rule.rule_id) {
                rules.push(engine_rule);
            }
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
fn discount_lines_from_request(
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

// ---------------------------------------------------------------------------
// Hook handlers.
// ---------------------------------------------------------------------------

/// Answer `pricing.discount.evaluate_requested`: the store and
/// billing call this to evaluate discounts without touching their
/// own order. The request payload carries `lines` (each with
/// `product_id`, `unit_price` as a decimal string or scaled
/// integer, and `quantity`), plus optional `coupon_code`,
/// `customer_id`, and `currency`. The answer is added as a result
/// and never mutates the caller's order. A missing or invalid
/// payload is answered with an error object so the caller can
/// fail closed instead of guessing.
fn on_discount_evaluate_requested(state: Arc<PricingState>, ctx: &mut HookContext) {
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
        // number; all three are parsed with integer math.
        let unit_price = match item.get("unit_price_scaled") {
            Some(value) => value.as_i64().unwrap_or(0),
            None => match item.get("unit_price") {
                Some(value) => match value_to_scaled(value, &currency, "unit_price") {
                    Ok(scaled) => scaled,
                    Err(error) => {
                        ctx.add_result(json!({
                            "valid": false,
                            "error": error.to_string(),
                        }));
                        return;
                    }
                },
                None => 0,
            },
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
            let mut answer = serde_json::to_value(&evaluation).unwrap_or(Value::Null);
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
fn on_invoice_validate_requested(state: Arc<PricingState>, ctx: &mut HookContext) {
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
            let unit_price = match item.get("unit_price_scaled") {
                Some(value) => value.as_i64().unwrap_or(0),
                None => match item.get("unit_price") {
                    Some(value) => value_to_scaled(value, &currency, "unit_price")
                        .map_err(|error| error.to_string())?,
                    None => 0,
                },
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
            let amount = match item.get("amount_scaled") {
                Some(value) => value.as_i64().unwrap_or(0),
                None => match item.get("amount") {
                    Some(value) => value_to_scaled(value, &currency, "amount")
                        .map_err(|error| error.to_string())?,
                    None => 0,
                },
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
            let mut answer = serde_json::to_value(&result).unwrap_or(Value::Null);
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
/// built from the engine record only — the AI words the summary,
/// it never invents prices. When the recommendation does not
/// exist, no result is added, so the caller can report the
/// recommendation as unavailable.
fn on_ai_recommend_requested(state: Arc<PricingState>, ctx: &mut HookContext) {
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

// ---------------------------------------------------------------------------
// HTTP handlers: recommendations.
// ---------------------------------------------------------------------------

fn store_recommendation(
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

async fn create_cost_plus_recommendation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreateCostPlusRecommendation>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingRecommendation> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let product_id = require_text(&input.product_id, "product_id", 128)?;
        let computed = cost_plus_recommendation(&input.costs, input.margin_bp)?;
        let inputs = serde_json::to_value(&input).unwrap_or(Value::Null);
        Ok(store_recommendation(
            &state,
            &call.tenant_id,
            &product_id,
            "cost_plus",
            computed.suggested_price_scaled,
            &computed.currency,
            computed.lines,
            inputs,
            &call.actor,
            &input.costs,
        ))
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_tiered_recommendation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreateTieredRecommendation>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingRecommendation> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let product_id = require_text(&input.product_id, "product_id", 128)?;
        let computed = tiered_price(&input.tiers, input.quantity)?;
        let inputs = serde_json::to_value(&input).unwrap_or(Value::Null);
        Ok(store_recommendation(
            &state,
            &call.tenant_id,
            &product_id,
            "tiered",
            computed.total_scaled,
            &computed.currency,
            computed.lines,
            inputs,
            &call.actor,
            &[],
        ))
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_bundle_recommendation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreateBundleRecommendation>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingRecommendation> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let product_id = require_text(&input.product_id, "product_id", 128)?;
        let computed = bundle_price(&input.components, &input.discount)?;
        let inputs = serde_json::to_value(&input).unwrap_or(Value::Null);
        Ok(store_recommendation(
            &state,
            &call.tenant_id,
            &product_id,
            "bundle",
            computed.total_scaled,
            &computed.currency,
            computed.lines,
            inputs,
            &call.actor,
            &[],
        ))
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_recommendations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let tenant_id = match caller(&req).and_then(|call| {
        require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call.tenant_id)
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let mut items: Vec<PricingRecommendation> = state
        .recommendations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && matches_search(&format!("{} {} {}", item.product_id, item.kind, item.status), &params)
                && params
                    .filters
                    .get("product_id")
                    .is_none_or(|value| item.product_id == *value)
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|value| item.kind == *value)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|value| item.status == *value)
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

async fn get_recommendation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PricingRecommendation> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        state
            .recommendations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Recommendation not found"))
            .and_then(|item| require_same_tenant(&item.tenant_id, &call.tenant_id).map(|_| item))
    })();
    json::respond(result, ctx.is_debug())
}

/// Confirm a recommendation: the admin accepts the suggestion.
/// Confirming creates a price version in `draft` (the approval
/// machine then decides whether a second approver is needed) and
/// marks the recommendation confirmed. The price is never applied
/// silently: activation is a separate, explicit step.
async fn confirm_recommendation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingRecommendation> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let mut recommendation = state
            .recommendations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Recommendation not found"))?;
        require_same_tenant(&recommendation.tenant_id, &call.tenant_id)?;
        if recommendation.status != "suggested" {
            return Err(UnifiedError::unprocessable(
                "Only a suggested recommendation can be confirmed",
            ));
        }
        let settings = state.settings_for(&call.tenant_id);
        let previous = state
            .active_price_for(&call.tenant_id, &recommendation.product_id)
            .map(|version| version.price_scaled)
            .unwrap_or(0);
        let change_bp = price_change_bp(previous, recommendation.suggested_price_scaled);
        let above_threshold = needs_second_approver(
            previous,
            recommendation.suggested_price_scaled,
            settings.approval_threshold_bp,
        );
        let version = PricingPriceVersion {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            product_id: recommendation.product_id.clone(),
            version: state.next_price_version(&call.tenant_id, &recommendation.product_id),
            price_scaled: recommendation.suggested_price_scaled,
            currency: recommendation.currency.clone(),
            price: recommendation.suggested_price.clone(),
            previous_price_scaled: previous,
            change_bp,
            above_threshold,
            status: "draft".to_string(),
            requested_by: call.actor.clone(),
            approved_by: None,
            reason: Some("Confirmed from a price recommendation".to_string()),
            recommendation_id: Some(recommendation.id.clone()),
            created_at: now(),
            updated_at: now(),
        };
        state
            .price_versions
            .insert(version.id.clone(), version.clone());
        recommendation.status = "confirmed".to_string();
        recommendation.confirmed_by = Some(call.actor.clone());
        recommendation.price_version_id = Some(version.id.clone());
        recommendation.updated_at = now();
        state
            .recommendations
            .insert(recommendation.id.clone(), recommendation.clone());
        state.emit(
            "pricing.recommendation.confirmed",
            json!({
                "tenant_id": call.tenant_id,
                "recommendation_id": recommendation.id,
                "product_id": recommendation.product_id,
                "price_version_id": version.id,
                "suggested_price_scaled": recommendation.suggested_price_scaled,
                "currency": recommendation.currency,
            }),
        );
        Ok(recommendation)
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_recommendation_narrative(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<AiDraftResponse> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        let recommendation = state
            .recommendations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Recommendation not found"))?;
        require_same_tenant(&recommendation.tenant_id, &call.tenant_id)?;
        let draft = build_ai_narrative(&recommendation);
        state.emit(
            "pricing.ai.draft_created",
            json!({
                "tenant_id": call.tenant_id,
                "recommendation_id": recommendation.id,
                "currency": recommendation.currency,
            }),
        );
        Ok(AiDraftResponse {
            recommendation_id: recommendation.id.clone(),
            draft,
            numbers: json!({
                "suggested_price_scaled": recommendation.suggested_price_scaled,
                "suggested_price": recommendation.suggested_price,
                "currency": recommendation.currency,
                "lines": recommendation.lines,
            }),
        })
    })();
    json::respond(result, ctx.is_debug())
}

// ---------------------------------------------------------------------------
// HTTP handlers: discount rules.
// ---------------------------------------------------------------------------

fn rule_from_create(
    state: &PricingState,
    input: CreatePricingRule,
    actor: &str,
    tenant_id: &str,
) -> UnifiedResult<PricingRule> {
    let name = require_text(&input.name, "name", 128)?;
    if !DISCOUNT_KINDS.contains(&input.kind.as_str()) {
        return Err(UnifiedError::bad_request(
            "Rule kind must be \"percent\", \"fixed\", \"buy_x_get_y\", or \"volume\"",
        ));
    }
    let currency = input
        .currency
        .unwrap_or_else(|| state.settings_for(tenant_id).default_currency)
        .to_uppercase();
    require_currency_known(state, &currency)?;
    if input.kind == "percent" || input.kind == "volume" {
        if !(0..=BASIS_POINTS).contains(&input.percent_bp) {
            return Err(UnifiedError::bad_request(
                "Discount percent must be between 0 and 10000 basis points",
            ));
        }
    }
    if input.kind == "volume" && input.min_quantity == 0 {
        return Err(UnifiedError::bad_request(
            "A volume rule needs a minimum quantity above zero",
        ));
    }
    if input.kind == "buy_x_get_y" && (input.buy_quantity == 0 || input.get_quantity == 0) {
        return Err(UnifiedError::bad_request(
            "A buy-X-get-Y rule needs a buy quantity and a get quantity above zero",
        ));
    }
    let amount_scaled = match &input.amount {
        Some(amount) => {
            let money = resolve_money(amount, Some(&currency))?;
            if money.currency != currency {
                return Err(UnifiedError::bad_request(
                    "Rule amount currency must match the rule currency",
                ));
            }
            round_to_minor(money.amount_scaled, &currency)
        }
        None => 0,
    };
    if input.kind == "fixed" && amount_scaled <= 0 {
        return Err(UnifiedError::bad_request(
            "A fixed rule needs an amount above zero",
        ));
    }
    let min_subtotal_scaled = match &input.min_subtotal {
        Some(amount) => {
            let money = resolve_money(amount, Some(&currency))?;
            if money.currency != currency {
                return Err(UnifiedError::bad_request(
                    "Minimum subtotal currency must match the rule currency",
                ));
            }
            round_to_minor(money.amount_scaled, &currency)
        }
        None => 0,
    };
    if let (Some(from), Some(to)) = (input.valid_from, input.valid_to) {
        if to <= from {
            return Err(UnifiedError::bad_request(
                "valid_to must be after valid_from",
            ));
        }
    }
    let status = input.status.unwrap_or_else(|| "active".to_string());
    if !["active", "draft", "archived"].contains(&status.as_str()) {
        return Err(UnifiedError::bad_request(
            "Rule status must be \"active\", \"draft\", or \"archived\"",
        ));
    }
    let now = now();
    Ok(PricingRule {
        id: new_id(),
        tenant_id: tenant_id.to_string(),
        name,
        kind: input.kind,
        percent_bp: input.percent_bp,
        amount_scaled,
        currency,
        buy_quantity: input.buy_quantity,
        get_quantity: input.get_quantity,
        min_quantity: input.min_quantity,
        min_subtotal_scaled,
        product_ids: input.product_ids,
        evaluation_order: input.evaluation_order,
        stackable: input.stackable.unwrap_or(true),
        exclusive: input.exclusive.unwrap_or(false),
        status,
        valid_from: input.valid_from,
        valid_to: input.valid_to,
        created_by: actor.to_string(),
        created_at: now,
        updated_at: now,
    })
}

async fn list_rules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let tenant_id = match caller(&req).and_then(|call| {
        require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call.tenant_id)
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let mut items: Vec<PricingRule> = state
        .rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && matches_search(&format!("{} {} {}", item.name, item.kind, item.status), &params)
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|value| item.kind == *value)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|value| item.status == *value)
                && params
                    .filters
                    .get("product_id")
                    .is_none_or(|value| item.product_ids.iter().any(|id| id == value))
        })
        .collect();
    items.sort_by(|a, b| {
        a.evaluation_order
            .cmp(&b.evaluation_order)
            .then(a.created_at.cmp(&b.created_at))
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

async fn create_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreatePricingRule>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingRule> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let rule = rule_from_create(&state, body.into_inner(), &call.actor, &call.tenant_id)?;
        state.rules.insert(rule.id.clone(), rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PricingRule> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        state
            .rules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing rule not found"))
            .and_then(|item| require_same_tenant(&item.tenant_id, &call.tenant_id).map(|_| item))
    })();
    json::respond(result, ctx.is_debug())
}

async fn update_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePricingRule>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingRule> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let mut rule = state
            .rules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing rule not found"))?;
        require_same_tenant(&rule.tenant_id, &call.tenant_id)?;
        let input = body.into_inner();
        if let Some(name) = input.name {
            rule.name = require_text(&name, "name", 128)?;
        }
        if let Some(percent_bp) = input.percent_bp {
            if !(0..=BASIS_POINTS).contains(&percent_bp) {
                return Err(UnifiedError::bad_request(
                    "Discount percent must be between 0 and 10000 basis points",
                ));
            }
            rule.percent_bp = percent_bp;
        }
        if let Some(amount) = input.amount {
            let money = resolve_money(&amount, Some(&rule.currency))?;
            if money.currency != rule.currency {
                return Err(UnifiedError::bad_request(
                    "Rule amount currency must match the rule currency",
                ));
            }
            rule.amount_scaled = round_to_minor(money.amount_scaled, &rule.currency);
        }
        if let Some(value) = input.buy_quantity {
            rule.buy_quantity = value;
        }
        if let Some(value) = input.get_quantity {
            rule.get_quantity = value;
        }
        if let Some(value) = input.min_quantity {
            rule.min_quantity = value;
        }
        if let Some(amount) = input.min_subtotal {
            let money = resolve_money(&amount, Some(&rule.currency))?;
            rule.min_subtotal_scaled = round_to_minor(money.amount_scaled, &rule.currency);
        }
        if let Some(ids) = input.product_ids {
            rule.product_ids = ids;
        }
        if let Some(order) = input.evaluation_order {
            rule.evaluation_order = order;
        }
        if let Some(stackable) = input.stackable {
            rule.stackable = stackable;
        }
        if let Some(exclusive) = input.exclusive {
            rule.exclusive = exclusive;
        }
        if let Some(status) = input.status {
            if !["active", "draft", "archived"].contains(&status.as_str()) {
                return Err(UnifiedError::bad_request(
                    "Rule status must be \"active\", \"draft\", or \"archived\"",
                ));
            }
            rule.status = status;
        }
        if let Some(from) = input.valid_from {
            rule.valid_from = Some(from);
        }
        if let Some(to) = input.valid_to {
            rule.valid_to = Some(to);
        }
        if let (Some(from), Some(to)) = (rule.valid_from, rule.valid_to) {
            if to <= from {
                return Err(UnifiedError::bad_request(
                    "valid_to must be after valid_from",
                ));
            }
        }
        rule.updated_at = now();
        state.rules.insert(rule.id.clone(), rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn delete_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let existing = state
            .rules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing rule not found"))?;
        require_same_tenant(&existing.tenant_id, &call.tenant_id)?;
        state
            .rules
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Pricing rule not found"))
    })();
    json::respond(result, ctx.is_debug())
}

// ---------------------------------------------------------------------------
// HTTP handlers: coupons and discount evaluation.
// ---------------------------------------------------------------------------

async fn list_coupons(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let tenant_id = match caller(&req).and_then(|call| {
        require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call.tenant_id)
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let mut items: Vec<PricingCoupon> = state
        .coupons
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && matches_search(&format!("{} {}", item.code, item.kind), &params)
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|value| item.kind == *value)
                && params
                    .filters
                    .get("active")
                    .is_none_or(|value| item.active == (*value == "true" || *value == "1"))
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

async fn create_coupon(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreatePricingCoupon>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingCoupon> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let code = normalize_code(&input.code)?;
        if state
            .coupons
            .iter()
            .any(|entry| entry.value().code == code)
        {
            return Err(UnifiedError::conflict(format!(
                "Coupon code {code} already exists"
            )));
        }
        if !["percent", "fixed", "buy_x_get_y"].contains(&input.kind.as_str()) {
            return Err(UnifiedError::bad_request(
                "Coupon kind must be \"percent\", \"fixed\", or \"buy_x_get_y\"",
            ));
        }
        let currency = input
            .currency
            .unwrap_or_else(|| state.settings_for(&call.tenant_id).default_currency)
            .to_uppercase();
        require_currency_known(&state, &currency)?;
        if input.kind == "percent" && !(0..=BASIS_POINTS).contains(&input.percent_bp) {
            return Err(UnifiedError::bad_request(
                "Coupon percent must be between 0 and 10000 basis points",
            ));
        }
        if input.kind == "buy_x_get_y" && (input.buy_quantity == 0 || input.get_quantity == 0) {
            return Err(UnifiedError::bad_request(
                "A buy-X-get-Y coupon needs a buy quantity and a get quantity above zero",
            ));
        }
        let amount_scaled = match &input.amount {
            Some(amount) => {
                let money = resolve_money(amount, Some(&currency))?;
                if money.currency != currency {
                    return Err(UnifiedError::bad_request(
                        "Coupon amount currency must match the coupon currency",
                    ));
                }
                round_to_minor(money.amount_scaled, &currency)
            }
            None => 0,
        };
        if input.kind == "fixed" && amount_scaled <= 0 {
            return Err(UnifiedError::bad_request(
                "A fixed coupon needs an amount above zero",
            ));
        }
        if let (Some(from), Some(to)) = (input.valid_from, input.valid_to) {
            if to <= from {
                return Err(UnifiedError::bad_request(
                    "valid_to must be after valid_from",
                ));
            }
        }
        let now = now();
        let coupon = PricingCoupon {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            code,
            kind: input.kind,
            percent_bp: input.percent_bp,
            amount_scaled,
            currency,
            buy_quantity: input.buy_quantity,
            get_quantity: input.get_quantity,
            product_ids: input.product_ids,
            valid_from: input.valid_from,
            valid_to: input.valid_to,
            usage_limit: input.usage_limit,
            per_customer_limit: input.per_customer_limit,
            used_count: 0,
            stackable: input.stackable.unwrap_or(false),
            exclusive: input.exclusive.unwrap_or(false),
            active: input.active.unwrap_or(true),
            created_by: call.actor,
            created_at: now,
            updated_at: now,
        };
        state.coupons.insert(coupon.id.clone(), coupon.clone());
        Ok(coupon)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_coupon(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PricingCoupon> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        state
            .coupons
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Coupon not found"))
            .and_then(|item| require_same_tenant(&item.tenant_id, &call.tenant_id).map(|_| item))
    })();
    json::respond(result, ctx.is_debug())
}

async fn update_coupon(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePricingCoupon>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingCoupon> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let mut coupon = state
            .coupons
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Coupon not found"))?;
        require_same_tenant(&coupon.tenant_id, &call.tenant_id)?;
        let input = body.into_inner();
        if let Some(kind) = input.kind {
            if !["percent", "fixed", "buy_x_get_y"].contains(&kind.as_str()) {
                return Err(UnifiedError::bad_request(
                    "Coupon kind must be \"percent\", \"fixed\", or \"buy_x_get_y\"",
                ));
            }
            coupon.kind = kind;
        }
        if let Some(percent_bp) = input.percent_bp {
            if !(0..=BASIS_POINTS).contains(&percent_bp) {
                return Err(UnifiedError::bad_request(
                    "Coupon percent must be between 0 and 10000 basis points",
                ));
            }
            coupon.percent_bp = percent_bp;
        }
        if let Some(amount) = input.amount {
            let money = resolve_money(&amount, Some(&coupon.currency))?;
            coupon.amount_scaled = round_to_minor(money.amount_scaled, &coupon.currency);
        }
        if let Some(value) = input.buy_quantity {
            coupon.buy_quantity = value;
        }
        if let Some(value) = input.get_quantity {
            coupon.get_quantity = value;
        }
        if let Some(ids) = input.product_ids {
            coupon.product_ids = ids;
        }
        if let Some(from) = input.valid_from {
            coupon.valid_from = Some(from);
        }
        if let Some(to) = input.valid_to {
            coupon.valid_to = Some(to);
        }
        if let (Some(from), Some(to)) = (coupon.valid_from, coupon.valid_to) {
            if to <= from {
                return Err(UnifiedError::bad_request(
                    "valid_to must be after valid_from",
                ));
            }
        }
        if let Some(limit) = input.usage_limit {
            coupon.usage_limit = Some(limit);
        }
        if let Some(limit) = input.per_customer_limit {
            coupon.per_customer_limit = Some(limit);
        }
        if let Some(stackable) = input.stackable {
            coupon.stackable = stackable;
        }
        if let Some(exclusive) = input.exclusive {
            coupon.exclusive = exclusive;
        }
        if let Some(active) = input.active {
            coupon.active = active;
        }
        coupon.updated_at = now();
        state.coupons.insert(coupon.id.clone(), coupon.clone());
        Ok(coupon)
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn delete_coupon(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let existing = state
            .coupons
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Coupon not found"))?;
        require_same_tenant(&existing.tenant_id, &call.tenant_id)?;
        state
            .coupons
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Coupon not found"))
    })();
    json::respond(result, ctx.is_debug())
}

/// Record one coupon redemption: the store calls this after an
/// order that used the coupon is confirmed. Limits are checked
/// again here, so a race between evaluation and confirmation
/// cannot overspend a coupon: the redemption is rejected when the
/// limit is already reached.
async fn redeem_coupon(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<Value>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingCoupon> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let customer_id = body
            .get("customer_id")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| UnifiedError::bad_request("customer_id is required"))?;
        let mut coupon = state
            .coupons
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Coupon not found"))?;
        require_same_tenant(&coupon.tenant_id, &call.tenant_id)?;
        if let Some(reason) = coupon_usability(&state, &coupon, Some(&customer_id), now()) {
            return Err(UnifiedError::unprocessable(reason));
        }
        coupon.used_count += 1;
        coupon.updated_at = now();
        state.coupons.insert(coupon.id.clone(), coupon.clone());
        let key = format!("{}:{}:{}", coupon.tenant_id, coupon.id, customer_id);
        let mut redemption = state
            .redemptions
            .get(&key)
            .map(|entry| entry.value().clone())
            .unwrap_or(CouponRedemption {
                coupon_id: coupon.id.clone(),
                tenant_id: coupon.tenant_id.clone(),
                customer_id: customer_id.clone(),
                count: 0,
                last_redeemed_at: now(),
            });
        redemption.count += 1;
        redemption.last_redeemed_at = now();
        state.redemptions.insert(key, redemption);
        state.emit(
            "pricing.coupon.redeemed",
            json!({
                "tenant_id": coupon.tenant_id,
                "coupon_id": coupon.id,
                "code": coupon.code,
                "customer_id": customer_id,
                "used_count": coupon.used_count,
            }),
        );
        Ok(coupon)
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_coupon_redemptions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let tenant_id = match caller(&req).and_then(|call| {
        require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call.tenant_id)
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return json::error_response(&error, debug),
    };
    match state.coupons.get(path.as_str()) {
        Some(entry) if entry.tenant_id == tenant_id => {}
        _ => {
            return json::error_response(&UnifiedError::not_found("Coupon not found"), debug);
        }
    }
    let mut items: Vec<CouponRedemption> = state
        .redemptions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| item.coupon_id == path.as_str() && item.tenant_id == tenant_id)
        .filter(|item| {
            matches_search(&item.customer_id, &params)
                && params
                    .filters
                    .get("customer_id")
                    .is_none_or(|value| item.customer_id == *value)
        })
        .collect();
    items.sort_by(|a, b| b.last_redeemed_at.cmp(&a.last_redeemed_at));
    json::success_response(paginate(items, &params), 200)
}

async fn evaluate_discounts_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<DiscountEvaluateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let currency = input
            .currency
            .clone()
            .unwrap_or_else(|| state.settings_for(&call.tenant_id).default_currency)
            .to_uppercase();
        require_currency_known(&state, &currency)?;
        let lines = discount_lines_from_request(&input, &currency)?;
        let (evaluation, coupon_problem) = evaluate_for_caller(
            &state,
            &call.tenant_id,
            &lines,
            input.coupon_code.as_deref(),
            input.customer_id.as_deref(),
            &currency,
            &input.rule_ids,
        )?;
        state.emit(
            "pricing.discount.evaluated",
            json!({
                "tenant_id": call.tenant_id,
                "subtotal_scaled": evaluation.subtotal_scaled,
                "total_discount_scaled": evaluation.total_discount_scaled,
                "total_scaled": evaluation.total_scaled,
                "currency": evaluation.currency,
                "coupon_code": input.coupon_code,
                "customer_id": input.customer_id,
            }),
        );
        let mut value = serde_json::to_value(&evaluation).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut value {
            if let Some(problem) = coupon_problem {
                map.insert("coupon_problem".to_string(), Value::String(problem));
            }
        }
        Ok(value)
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers: market reference prices.
// ---------------------------------------------------------------------------

async fn list_reference_prices(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let tenant_id = match caller(&req).and_then(|call| {
        require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call.tenant_id)
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let mut items: Vec<PricingReferencePrice> = state
        .reference_prices
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && matches_search(&format!("{} {} {}", item.product_id, item.source, item.note), &params)
                && params
                    .filters
                    .get("product_id")
                    .is_none_or(|value| item.product_id == *value)
                && params
                    .filters
                    .get("source")
                    .is_none_or(|value| item.source == *value)
        })
        .collect();
    items.sort_by(|a, b| b.observed_at.cmp(&a.observed_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

async fn create_reference_price(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreateReferencePrice>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingReferencePrice> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let product_id = require_text(&input.product_id, "product_id", 128)?;
        let source = require_text(&input.source, "source", 128)?;
        let money = resolve_money(&input.price, None)?;
        require_currency_known(&state, &money.currency)?;
        if let Some(url) = &input.source_url {
            let trimmed = url.trim();
            if !trimmed.is_empty()
                && !trimmed.starts_with("http://")
                && !trimmed.starts_with("https://")
            {
                return Err(UnifiedError::bad_request(
                    "source_url must start with http:// or https://",
                ));
            }
        }
        let now = now();
        let record = PricingReferencePrice {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            product_id,
            source,
            source_url: input.source_url.map(|url| url.trim().to_string()).filter(|url| !url.is_empty()),
            price_scaled: round_to_minor(money.amount_scaled, &money.currency),
            currency: money.currency.clone(),
            price: format_scaled(round_to_minor(money.amount_scaled, &money.currency), &money.currency),
            observed_at: input.observed_at,
            note: input.note.unwrap_or_default().trim().to_string(),
            entered_by: call.actor,
            created_at: now,
        };
        state
            .reference_prices
            .insert(record.id.clone(), record.clone());
        Ok(record)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_reference_price(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PricingReferencePrice> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        state
            .reference_prices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Reference price not found"))
            .and_then(|item| require_same_tenant(&item.tenant_id, &call.tenant_id).map(|_| item))
    })();
    json::respond(result, ctx.is_debug())
}

async fn delete_reference_price(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let existing = state
            .reference_prices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Reference price not found"))?;
        require_same_tenant(&existing.tenant_id, &call.tenant_id)?;
        state
            .reference_prices
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Reference price not found"))
    })();
    json::respond(result, ctx.is_debug())
}

/// Summarize the human-entered reference prices for one product:
/// count, lowest, highest, and median. References are data, never
/// scraped; a product with no references answers with a count of
/// zero rather than an invented figure.
async fn summarize_reference_prices(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let tenant_id = match caller(&req).and_then(|call| {
        require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call.tenant_id)
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let product_id = params.filters.get("product_id").cloned().unwrap_or_default();
    if product_id.trim().is_empty() {
        return json::error_response(
            &UnifiedError::bad_request("Query parameter product_id is required"),
            debug,
        );
    }
    let mut by_currency: Vec<(String, Vec<i64>)> = Vec::new();
    for item in state
        .reference_prices
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| item.tenant_id == tenant_id && item.product_id == product_id)
    {
        match by_currency.iter_mut().find(|(currency, _)| *currency == item.currency) {
            Some((_, prices)) => prices.push(item.price_scaled),
            None => by_currency.push((item.currency, vec![item.price_scaled])),
        }
    }
    by_currency.sort_by(|a, b| a.0.cmp(&b.0));
    // One summary per currency: references in different currencies
    // are never averaged together.
    let mut summaries: Vec<Value> = Vec::new();
    for (currency, prices) in by_currency {
        if let Some((lowest, highest, median, count)) = summarize_references(&prices) {
            summaries.push(json!({
                "currency": currency,
                "count": count,
                "lowest_scaled": lowest,
                "lowest": format_scaled(lowest, &currency),
                "highest_scaled": highest,
                "highest": format_scaled(highest, &currency),
                "median_scaled": median,
                "median": format_scaled(median, &currency),
            }));
        }
    }
    json::success_response(
        json!({ "product_id": product_id, "summaries": summaries }),
        200,
    )
}

// ---------------------------------------------------------------------------
// HTTP handlers: price versions and approvals.
// ---------------------------------------------------------------------------

async fn list_price_versions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let tenant_id = match caller(&req).and_then(|call| {
        require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call.tenant_id)
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let mut items: Vec<PricingPriceVersion> = state
        .price_versions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && matches_search(&format!("{} {}", item.product_id, item.status), &params)
                && params
                    .filters
                    .get("product_id")
                    .is_none_or(|value| item.product_id == *value)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|value| item.status == *value)
        })
        .collect();
    items.sort_by(|a, b| {
        b.product_id
            .cmp(&a.product_id)
            .then(b.version.cmp(&a.version))
    });
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

async fn create_price_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreatePriceVersion>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingPriceVersion> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let product_id = require_text(&input.product_id, "product_id", 128)?;
        let money = resolve_money(&input.price, None)?;
        require_currency_known(&state, &money.currency)?;
        let price_scaled = round_to_minor(money.amount_scaled, &money.currency);
        if let Some(recommendation_id) = &input.recommendation_id {
            match state.recommendations.get(recommendation_id.as_str()) {
                Some(entry) if entry.tenant_id == call.tenant_id => {}
                _ => {
                    return Err(UnifiedError::not_found(
                        "Recommendation not found",
                    ));
                }
            }
        }
        let settings = state.settings_for(&call.tenant_id);
        let previous = state
            .active_price_for(&call.tenant_id, &product_id)
            .map(|version| version.price_scaled)
            .unwrap_or(0);
        let change_bp = price_change_bp(previous, price_scaled);
        let above_threshold =
            needs_second_approver(previous, price_scaled, settings.approval_threshold_bp);
        let now = now();
        let version = PricingPriceVersion {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            product_id,
            version: state.next_price_version(&call.tenant_id, &product_id),
            price_scaled,
            currency: money.currency.clone(),
            price: format_scaled(price_scaled, &money.currency),
            previous_price_scaled: previous,
            change_bp,
            above_threshold,
            status: "draft".to_string(),
            requested_by: call.actor,
            approved_by: None,
            reason: input.reason.map(|reason| reason.trim().to_string()).filter(|reason| !reason.is_empty()),
            recommendation_id: input.recommendation_id,
            created_at: now,
            updated_at: now,
        };
        state
            .price_versions
            .insert(version.id.clone(), version.clone());
        Ok(version)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_price_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PricingPriceVersion> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        state
            .price_versions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Price version not found"))
            .and_then(|item| require_same_tenant(&item.tenant_id, &call.tenant_id).map(|_| item))
    })();
    json::respond(result, ctx.is_debug())
}

fn fetch_price_version(
    state: &PricingState,
    id: &str,
    tenant_id: &str,
) -> UnifiedResult<PricingPriceVersion> {
    let version = state
        .price_versions
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Price version not found"))?;
    require_same_tenant(&version.tenant_id, tenant_id)?;
    Ok(version)
}

fn save_price_version(
    state: &PricingState,
    mut version: PricingPriceVersion,
    event: &str,
) -> PricingPriceVersion {
    version.updated_at = now();
    state
        .price_versions
        .insert(version.id.clone(), version.clone());
    state.emit(
        event,
        json!({
            "tenant_id": version.tenant_id,
            "price_version_id": version.id,
            "product_id": version.product_id,
            "version": version.version,
            "status": version.status,
            "price_scaled": version.price_scaled,
            "currency": version.currency,
        }),
    );
    version
}

/// Submit a draft: below the threshold it moves straight to
/// `approved`; above the threshold it moves to
/// `pending_approval` and waits for a second approver.
async fn submit_price_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingPriceVersion> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let mut version = fetch_price_version(&state, path.as_str(), &call.tenant_id)?;
        if version.status != "draft" {
            return Err(UnifiedError::unprocessable(
                "Only a draft price version can be submitted",
            ));
        }
        version.status = if version.above_threshold {
            "pending_approval".to_string()
        } else {
            "approved".to_string()
        };
        Ok(save_price_version(&state, version, "pricing.price_version.submitted"))
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// Approve a pending version. The approver must hold
/// `pricing:approve` and must not be the requester when the
/// settings require a second approver: the second pair of eyes
/// is the point of the threshold rule.
async fn approve_price_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PriceApprovalInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingPriceVersion> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_APPROVE)?;
        let mut version = fetch_price_version(&state, path.as_str(), &call.tenant_id)?;
        if version.status != "pending_approval" && version.status != "draft" {
            return Err(UnifiedError::unprocessable(
                "Only a pending or draft price version can be approved",
            ));
        }
        let settings = state.settings_for(&call.tenant_id);
        if settings.require_second_approver && version.above_threshold && version.requested_by == call.actor {
            return Err(UnifiedError::forbidden(
                "The requester cannot approve their own price change above the threshold: a second approver is required",
            ));
        }
        version.status = "approved".to_string();
        version.approved_by = Some(call.actor.clone());
        let approval = PricingPriceApproval {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            price_version_id: version.id.clone(),
            approver: call.actor.clone(),
            decision: "approved".to_string(),
            reason: body.reason.clone().map(|reason| reason.trim().to_string()).filter(|reason| !reason.is_empty()),
            decided_at: now(),
        };
        state
            .price_approvals
            .insert(approval.id.clone(), approval);
        Ok(save_price_version(&state, version, "pricing.price_version.approved"))
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// Reject a pending version: it returns to `draft` so the
/// requester can revise and resubmit it.
async fn reject_price_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PriceApprovalInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingPriceVersion> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_APPROVE)?;
        let mut version = fetch_price_version(&state, path.as_str(), &call.tenant_id)?;
        if version.status != "pending_approval" && version.status != "draft" {
            return Err(UnifiedError::unprocessable(
                "Only a pending or draft price version can be rejected",
            ));
        }
        version.status = "draft".to_string();
        let approval = PricingPriceApproval {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            price_version_id: version.id.clone(),
            approver: call.actor.clone(),
            decision: "rejected".to_string(),
            reason: body.reason.clone().map(|reason| reason.trim().to_string()).filter(|reason| !reason.is_empty()),
            decided_at: now(),
        };
        state
            .price_approvals
            .insert(approval.id.clone(), approval);
        Ok(save_price_version(&state, version, "pricing.price_version.rejected"))
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// Activate an approved version: it becomes the product's current
/// price, and any other active version for the same product in
/// the same tenant is superseded back to `approved`.
async fn activate_price_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingPriceVersion> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let mut version = fetch_price_version(&state, path.as_str(), &call.tenant_id)?;
        if version.status != "approved" {
            return Err(UnifiedError::unprocessable(
                "Only an approved price version can be activated",
            ));
        }
        for mut entry in state.price_versions.iter_mut() {
            let other = entry.value_mut();
            if other.tenant_id == call.tenant_id
                && other.product_id == version.product_id
                && other.id != version.id
                && other.status == "active"
            {
                other.status = "approved".to_string();
                other.updated_at = now();
            }
        }
        version.status = "active".to_string();
        Ok(save_price_version(&state, version, "pricing.price_version.activated"))
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_price_approvals(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let tenant_id = match caller(&req).and_then(|call| {
        require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call.tenant_id)
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return json::error_response(&error, debug),
    };
    match fetch_price_version(&state, path.as_str(), &tenant_id) {
        Ok(_) => {}
        Err(error) => return json::error_response(&error, debug),
    }
    let mut items: Vec<PricingPriceApproval> = state
        .price_approvals
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && item.price_version_id == path.as_str()
                && params
                    .filters
                    .get("decision")
                    .is_none_or(|value| item.decision == *value)
        })
        .collect();
    items.sort_by(|a, b| b.decided_at.cmp(&a.decided_at));
    json::success_response(paginate(items, &params), 200)
}

// ---------------------------------------------------------------------------
// HTTP handlers: tiers and bundles.
// ---------------------------------------------------------------------------

async fn list_tiers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let tenant_id = match caller(&req).and_then(|call| {
        require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call.tenant_id)
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let mut items: Vec<PricingTier> = state
        .tiers
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && params
                    .filters
                    .get("product_id")
                    .is_none_or(|value| item.product_id == *value)
        })
        .collect();
    items.sort_by(|a, b| {
        a.product_id
            .cmp(&b.product_id)
            .then(a.min_quantity.cmp(&b.min_quantity))
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

async fn create_tier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreatePricingTier>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTier> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let product_id = require_text(&input.product_id, "product_id", 128)?;
        if input.min_quantity == 0 {
            return Err(UnifiedError::bad_request(
                "Tier minimum quantity must be at least 1",
            ));
        }
        if let Some(max) = input.max_quantity {
            if max < input.min_quantity {
                return Err(UnifiedError::bad_request(
                    "Tier maximum quantity must not be below its minimum",
                ));
            }
        }
        let money = resolve_money(&input.unit_price, None)?;
        require_currency_known(&state, &money.currency)?;
        // Overlap check against the tenant's tiers for this product.
        for entry in state.tiers.iter() {
            let other = entry.value();
            if other.tenant_id != call.tenant_id || other.product_id != product_id {
                continue;
            }
            let other_max = other.max_quantity.unwrap_or(u32::MAX);
            let new_max = input.max_quantity.unwrap_or(u32::MAX);
            if input.min_quantity <= other_max && other.min_quantity <= new_max {
                return Err(UnifiedError::conflict(
                    "Tier overlaps an existing tier for this product",
                ));
            }
        }
        let now = now();
        let tier = PricingTier {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            product_id,
            min_quantity: input.min_quantity,
            max_quantity: input.max_quantity,
            unit_price_scaled: round_to_minor(money.amount_scaled, &money.currency),
            currency: money.currency,
            created_at: now,
            updated_at: now,
        };
        state.tiers.insert(tier.id.clone(), tier.clone());
        Ok(tier)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn delete_tier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let existing = state
            .tiers
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tier not found"))?;
        require_same_tenant(&existing.tenant_id, &call.tenant_id)?;
        state
            .tiers
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Tier not found"))
    })();
    json::respond(result, ctx.is_debug())
}

/// Price a quantity against the tenant's stored tiers for a
/// product, using the same pure engine as the recommendation.
async fn price_with_stored_tiers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let result: UnifiedResult<TieredPriceResult> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        let product_id = params
            .filters
            .get("product_id")
            .cloned()
            .unwrap_or_default();
        if product_id.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Query parameter product_id is required",
            ));
        }
        let quantity: u32 = params
            .filters
            .get("quantity")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        if quantity == 0 {
            return Err(UnifiedError::bad_request(
                "Query parameter quantity must be at least 1",
            ));
        }
        let mut tiers: Vec<TierInput> = state
            .tiers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|tier| tier.tenant_id == call.tenant_id && tier.product_id == product_id)
            .map(|tier| TierInput {
                min_quantity: tier.min_quantity,
                max_quantity: tier.max_quantity,
                unit_price: MoneyInput {
                    amount: format_scaled(tier.unit_price_scaled, &tier.currency),
                    currency: Some(tier.currency.clone()),
                },
            })
            .collect();
        tiers.sort_by_key(|tier| tier.min_quantity);
        tiered_price(&tiers, quantity)
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_bundles(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let tenant_id = match caller(&req).and_then(|call| {
        require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call.tenant_id)
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let mut items: Vec<PricingBundle> = state
        .bundles
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && matches_search(&format!("{} {}", item.name, item.product_id), &params)
                && params
                    .filters
                    .get("product_id")
                    .is_none_or(|value| item.product_id == *value)
                && params
                    .filters
                    .get("active")
                    .is_none_or(|value| item.active == (*value == "true" || *value == "1"))
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

async fn create_bundle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreatePricingBundle>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingBundle> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 128)?;
        let product_id = require_text(&input.product_id, "product_id", 128)?;
        if !["percent", "fixed"].contains(&input.discount_kind.as_str()) {
            return Err(UnifiedError::bad_request(
                "Bundle discount kind must be \"percent\" or \"fixed\"",
            ));
        }
        if input.discount_kind == "percent"
            && !(0..=BASIS_POINTS).contains(&input.discount_percent_bp)
        {
            return Err(UnifiedError::bad_request(
                "Bundle discount percent must be between 0 and 10000 basis points",
            ));
        }
        let mut currency: Option<String> = None;
        let mut components: Vec<PricingBundleComponent> = Vec::new();
        let bundle_id = new_id();
        for component in &input.components {
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
            components.push(PricingBundleComponent {
                id: new_id(),
                tenant_id: call.tenant_id.clone(),
                bundle_id: bundle_id.clone(),
                product_id: require_text(&component.product_id, "component product_id", 128)?,
                label: component.label.trim().to_string(),
                price_scaled: round_to_minor(money.amount_scaled, &money.currency),
                currency: money.currency,
            });
        }
        let currency = currency.unwrap_or_else(|| state.settings_for(&call.tenant_id).default_currency);
        require_currency_known(&state, &currency)?;
        let discount_amount_scaled = match &input.discount_amount {
            Some(amount) => {
                let money = resolve_money(amount, Some(&currency))?;
                if money.currency != currency {
                    return Err(UnifiedError::bad_request(
                        "Bundle discount currency must match the component currency",
                    ));
                }
                round_to_minor(money.amount_scaled, &currency)
            }
            None => 0,
        };
        if input.discount_kind == "fixed" && discount_amount_scaled <= 0 {
            return Err(UnifiedError::bad_request(
                "A fixed bundle discount needs an amount above zero",
            ));
        }
        let now = now();
        let bundle = PricingBundle {
            id: bundle_id,
            tenant_id: call.tenant_id.clone(),
            name,
            product_id,
            discount_kind: input.discount_kind,
            discount_percent_bp: input.discount_percent_bp,
            discount_amount_scaled,
            currency,
            active: input.active.unwrap_or(true),
            created_by: call.actor,
            created_at: now,
            updated_at: now,
        };
        for component in components {
            state
                .bundle_components
                .insert(component.id.clone(), component);
        }
        state.bundles.insert(bundle.id.clone(), bundle.clone());
        Ok(bundle)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_bundle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        let bundle = state
            .bundles
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Bundle not found"))?;
        require_same_tenant(&bundle.tenant_id, &call.tenant_id)?;
        let components: Vec<PricingBundleComponent> = state
            .bundle_components
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|component| {
                component.tenant_id == call.tenant_id && component.bundle_id == bundle.id
            })
            .collect();
        Ok(json!({ "bundle": bundle, "components": components }))
    })();
    json::respond(result, ctx.is_debug())
}

/// Price a stored bundle with the pure bundle engine.
async fn price_bundle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BundlePriceResult> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        let bundle = state
            .bundles
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Bundle not found"))?;
        require_same_tenant(&bundle.tenant_id, &call.tenant_id)?;
        let components: Vec<BundleComponentInput> = state
            .bundle_components
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|component| {
                component.tenant_id == call.tenant_id && component.bundle_id == bundle.id
            })
            .map(|component| BundleComponentInput {
                product_id: component.product_id,
                label: component.label,
                price: MoneyInput {
                    amount: format_scaled(component.price_scaled, &component.currency),
                    currency: Some(component.currency),
                },
            })
            .collect();
        let discount_kind = bundle.discount_kind.clone();
        let discount = BundleDiscountInput {
            kind: discount_kind.clone(),
            percent_bp: bundle.discount_percent_bp,
            amount: if discount_kind == "fixed" {
                Some(MoneyInput {
                    amount: format_scaled(bundle.discount_amount_scaled, &bundle.currency),
                    currency: Some(bundle.currency.clone()),
                })
            } else {
                None
            },
        };
        bundle_price(&components, &discount)
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers: proration.
// ---------------------------------------------------------------------------

async fn compute_proration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<ComputeProrationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingProration> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings_for(&call.tenant_id);
        let old_price = resolve_money(&input.old_price, None)?;
        let new_price = resolve_money(&input.new_price, Some(&old_price.currency))?;
        if old_price.currency != new_price.currency {
            return Err(UnifiedError::bad_request(
                "Old and new prices must use the same currency",
            ));
        }
        require_currency_known(&state, &old_price.currency)?;
        let basis = input
            .basis
            .unwrap_or_else(|| settings.proration_basis.clone());
        let computed = prorate(&ProrationInput {
            kind: input.kind,
            old_price_scaled: round_to_minor(old_price.amount_scaled, &old_price.currency),
            new_price_scaled: round_to_minor(new_price.amount_scaled, &new_price.currency),
            period_start: input.period_start,
            period_end: input.period_end,
            change_at: input.change_at,
            basis,
            days_per_month: settings.days_per_month,
            currency: old_price.currency.clone(),
        })?;
        let record = PricingProration {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            subscription_id: input.subscription_id.map(|id| id.trim().to_string()).filter(|id| !id.is_empty()),
            customer_id: input.customer_id.map(|id| id.trim().to_string()).filter(|id| !id.is_empty()),
            kind: computed.kind,
            basis: computed.basis,
            period_start: input.period_start,
            period_end: input.period_end,
            change_at: input.change_at,
            period_days: computed.period_days,
            days_used: computed.days_used,
            days_remaining: computed.days_remaining,
            old_daily_rate_scaled: computed.old_daily_rate_scaled,
            new_daily_rate_scaled: computed.new_daily_rate_scaled,
            unused_credit_scaled: computed.unused_credit_scaled,
            new_charge_scaled: computed.new_charge_scaled,
            net_due_scaled: computed.net_due_scaled,
            currency: computed.currency,
            lines: computed.lines,
            created_by: call.actor,
            created_at: now(),
        };
        state.prorations.insert(record.id.clone(), record.clone());
        state.emit(
            "pricing.proration.computed",
            json!({
                "tenant_id": record.tenant_id,
                "proration_id": record.id,
                "subscription_id": record.subscription_id,
                "kind": record.kind,
                "net_due_scaled": record.net_due_scaled,
                "currency": record.currency,
            }),
        );
        Ok(record)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_prorations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let tenant_id = match caller(&req).and_then(|call| {
        require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call.tenant_id)
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let mut items: Vec<PricingProration> = state
        .prorations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && params
                    .filters
                    .get("subscription_id")
                    .is_none_or(|value| item.subscription_id.as_deref() == Some(value.as_str()))
                && params
                    .filters
                    .get("customer_id")
                    .is_none_or(|value| item.customer_id.as_deref() == Some(value.as_str()))
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|value| item.kind == *value)
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

async fn get_proration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PricingProration> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        state
            .prorations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Proration not found"))
            .and_then(|item| require_same_tenant(&item.tenant_id, &call.tenant_id).map(|_| item))
    })();
    json::respond(result, ctx.is_debug())
}

// ---------------------------------------------------------------------------
// HTTP handlers: invoice preview, reconciliation, credit notes.
// ---------------------------------------------------------------------------

/// Resolve an invoice preview/reconcile request into engine inputs.
fn reconcile_inputs_from_request(
    state: &PricingState,
    tenant_id: &str,
    request: &InvoicePreviewRequest,
) -> UnifiedResult<(Vec<ReconcileLineInput>, ReconcileClaim, String)> {
    let currency = request
        .currency
        .clone()
        .unwrap_or_else(|| state.settings_for(tenant_id).default_currency)
        .to_uppercase();
    require_currency_known(state, &currency)?;
    let mut lines = Vec::new();
    for line in &request.lines {
        let money = resolve_money(&line.unit_price, Some(&currency))?;
        if money.currency != currency {
            return Err(UnifiedError::bad_request(
                "All invoice lines must use the request currency",
            ));
        }
        lines.push(ReconcileLineInput {
            name: line.name.trim().to_string(),
            quantity: line.quantity,
            unit_price_scaled: round_to_minor(money.amount_scaled, &currency),
            tax_bp: line.tax_bp,
            tax_inclusive: line.tax_inclusive,
        });
    }
    let claim_money = |input: &MoneyInput| -> UnifiedResult<i64> {
        let money = resolve_money(input, Some(&currency))?;
        if money.currency != currency {
            return Err(UnifiedError::bad_request(
                "All claimed totals must use the request currency",
            ));
        }
        Ok(round_to_minor(money.amount_scaled, &currency))
    };
    let claim = ReconcileClaim {
        subtotal_scaled: claim_money(&request.claimed.subtotal)?,
        discounts_scaled: claim_money(&request.claimed.discounts)?,
        proration_scaled: claim_money(&request.claimed.proration)?,
        tax_total_scaled: claim_money(&request.claimed.tax_total)?,
        total_scaled: claim_money(&request.claimed.total)?,
    };
    Ok((lines, claim, currency))
}

/// Preview an invoice: recompute it and return the exact invoice
/// together with the reconciliation result. The admin sees the
/// exact figures before anything is issued; billing must call
/// `pricing.invoice.validate_requested` (or this endpoint) and
/// refuse to issue when `reconciliation.matched` is false.
async fn preview_invoice(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<InvoicePreviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        let input = body.into_inner();
        let (lines, claim, currency) =
            reconcile_inputs_from_request(&state, &call.tenant_id, &input)?;
        let reconciliation = reconcile_invoice(
            &lines,
            &input.discounts,
            &input.proration_lines,
            &claim,
            &currency,
        )?;
        let record = state.record_reconciliation(
            &call.tenant_id,
            input.invoice_id.clone(),
            &reconciliation,
            &call.actor,
        );
        state.emit(
            "pricing.invoice.previewed",
            json!({
                "tenant_id": call.tenant_id,
                "invoice_id": input.invoice_id,
                "reconciliation_id": record.id,
                "matched": reconciliation.matched,
                "currency": currency,
            }),
        );
        if reconciliation.matched {
            state.emit(
                "pricing.invoice.reconciled",
                json!({
                    "tenant_id": call.tenant_id,
                    "invoice_id": input.invoice_id,
                    "reconciliation_id": record.id,
                    "currency": currency,
                }),
            );
        } else {
            state.emit(
                "pricing.invoice.reconciliation_failed",
                json!({
                    "tenant_id": call.tenant_id,
                    "invoice_id": input.invoice_id,
                    "reconciliation_id": record.id,
                    "currency": currency,
                    "total_discrepancy_scaled": reconciliation.total_discrepancy_scaled,
                    "components": reconciliation.components,
                }),
            );
        }
        Ok(json!({
            "invoice_id": input.invoice_id,
            "currency": currency,
            "reconciliation_id": record.id,
            "reconciliation": reconciliation,
            "can_issue": reconciliation.matched,
        }))
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn reconcile_invoice_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<InvoicePreviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let (lines, claim, currency) =
            reconcile_inputs_from_request(&state, &call.tenant_id, &input)?;
        let reconciliation = reconcile_invoice(
            &lines,
            &input.discounts,
            &input.proration_lines,
            &claim,
            &currency,
        )?;
        let record = state.record_reconciliation(
            &call.tenant_id,
            input.invoice_id.clone(),
            &reconciliation,
            &call.actor,
        );
        if reconciliation.matched {
            state.emit(
                "pricing.invoice.reconciled",
                json!({
                    "tenant_id": call.tenant_id,
                    "invoice_id": input.invoice_id,
                    "reconciliation_id": record.id,
                    "currency": currency,
                }),
            );
        } else {
            state.emit(
                "pricing.invoice.reconciliation_failed",
                json!({
                    "tenant_id": call.tenant_id,
                    "invoice_id": input.invoice_id,
                    "reconciliation_id": record.id,
                    "currency": currency,
                    "total_discrepancy_scaled": reconciliation.total_discrepancy_scaled,
                    "components": reconciliation.components,
                }),
            );
        }
        let mut value = serde_json::to_value(&reconciliation).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut value {
            map.insert("valid".to_string(), Value::Bool(reconciliation.matched));
            map.insert(
                "reconciliation_id".to_string(),
                Value::String(record.id.clone()),
            );
        }
        Ok(value)
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_reconciliations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let tenant_id = match caller(&req).and_then(|call| {
        require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call.tenant_id)
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let mut items: Vec<PricingReconciliation> = state
        .reconciliations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && params
                    .filters
                    .get("invoice_id")
                    .is_none_or(|value| item.invoice_id.as_deref() == Some(value.as_str()))
                && params
                    .filters
                    .get("matched")
                    .is_none_or(|value| item.matched == (*value == "true" || *value == "1"))
        })
        .collect();
    items.sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

async fn get_reconciliation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PricingReconciliation> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        state
            .reconciliations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Reconciliation not found"))
            .and_then(|item| require_same_tenant(&item.tenant_id, &call.tenant_id).map(|_| item))
    })();
    json::respond(result, ctx.is_debug())
}

async fn list_credit_notes(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let tenant_id = match caller(&req).and_then(|call| {
        require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call.tenant_id)
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let mut items: Vec<PricingCreditNote> = state
        .credit_notes
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && matches_search(&format!("{} {} {}", item.number, item.invoice_id, item.status), &params)
                && params
                    .filters
                    .get("invoice_id")
                    .is_none_or(|value| item.invoice_id == *value)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|value| item.status == *value)
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

async fn create_credit_note(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreateCreditNote>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingCreditNote> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let invoice_id = require_text(&input.invoice_id, "invoice_id", 128)?;
        let reason = require_text(&input.reason, "reason", 512)?;
        let money = resolve_money(&input.amount, None)?;
        require_currency_known(&state, &money.currency)?;
        let amount_scaled = round_to_minor(money.amount_scaled, &money.currency);
        let lines: Vec<ReconcileAdjustmentInput> = input
            .lines
            .iter()
            .map(|line| ReconcileAdjustmentInput {
                label: line.label.trim().to_string(),
                amount_scaled: line.amount_scaled,
            })
            .collect();
        // A credit note is only created when its own lines add up
        // to the credit amount exactly.
        let check = reconcile_credit_note(&lines, amount_scaled, &money.currency)?;
        let issue = input.issue.unwrap_or(false);
        if issue && !check.matched {
            return Err(UnifiedError::unprocessable(format!(
                "Credit note lines do not add up to the credit amount (discrepancy {})",
                check.total_discrepancy
            )));
        }
        let now = now();
        let note = PricingCreditNote {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            number: state.next_credit_number(),
            invoice_id,
            amount_scaled,
            currency: money.currency.clone(),
            amount: format_scaled(amount_scaled, &money.currency),
            lines,
            reason,
            status: if issue { "issued".to_string() } else { "draft".to_string() },
            reconciled: check.matched,
            created_by: call.actor,
            created_at: now,
            updated_at: now,
        };
        state
            .credit_notes
            .insert(note.id.clone(), note.clone());
        state.emit(
            if issue {
                "pricing.credit_note.issued"
            } else {
                "pricing.credit_note.created"
            },
            json!({
                "tenant_id": note.tenant_id,
                "credit_note_id": note.id,
                "number": note.number,
                "invoice_id": note.invoice_id,
                "amount_scaled": note.amount_scaled,
                "currency": note.currency,
                "status": note.status,
            }),
        );
        Ok(note)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_credit_note(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PricingCreditNote> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        state
            .credit_notes
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Credit note not found"))
            .and_then(|item| require_same_tenant(&item.tenant_id, &call.tenant_id).map(|_| item))
    })();
    json::respond(result, ctx.is_debug())
}

/// Reconcile an issued or draft credit note on demand: the lines
/// must add up to the credit amount exactly.
async fn reconcile_credit_note_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ReconciliationResult> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        let note = state
            .credit_notes
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Credit note not found"))?;
        require_same_tenant(&note.tenant_id, &call.tenant_id)?;
        reconcile_credit_note(&note.lines, note.amount_scaled, &note.currency)
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers: settings, currencies, conversion.
// ---------------------------------------------------------------------------

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<PricingSettings> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        Ok(state.settings_for(&call.tenant_id))
    })();
    json::respond(result, ctx.is_debug())
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<UpdatePricingSettings>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingSettings> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings_for(&call.tenant_id);
        if let Some(mode) = input.rounding_mode {
            if !ROUNDING_MODES.contains(&mode.as_str()) {
                return Err(UnifiedError::bad_request(
                    "rounding_mode must be \"half_up\" (the only implemented rounding rule)",
                ));
            }
            settings.rounding_mode = mode;
        }
        if let Some(basis) = input.proration_basis {
            if !PRORATION_BASES.contains(&basis.as_str()) {
                return Err(UnifiedError::bad_request(
                    "proration_basis must be \"per_day\" or \"per_month\"",
                ));
            }
            settings.proration_basis = basis;
        }
        if let Some(days) = input.days_per_month {
            if days <= 0 || days > 366 {
                return Err(UnifiedError::bad_request(
                    "days_per_month must be between 1 and 366",
                ));
            }
            settings.days_per_month = days;
        }
        if let Some(threshold) = input.approval_threshold_bp {
            if !(0..=BASIS_POINTS).contains(&threshold) {
                return Err(UnifiedError::bad_request(
                    "approval_threshold_bp must be between 0 and 10000",
                ));
            }
            settings.approval_threshold_bp = threshold;
        }
        if let Some(require) = input.require_second_approver {
            settings.require_second_approver = require;
        }
        if let Some(currency) = input.default_currency {
            let upper = currency.to_uppercase();
            require_currency_known(&state, &upper)?;
            settings.default_currency = upper;
        }
        state
            .settings
            .insert(call.tenant_id.clone(), settings.clone());
        Ok(settings)
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_currencies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let auth = caller(&req)
        .and_then(|call| require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call));
    if let Err(error) = auth {
        return json::error_response(&error, debug);
    }
    let mut items: Vec<PricingCurrency> = state
        .currencies
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            matches_search(&item.code, &params)
                && params
                    .filters
                    .get("active")
                    .is_none_or(|value| item.active == (*value == "true" || *value == "1"))
        })
        .collect();
    items.sort_by(|a, b| a.code.cmp(&b.code));
    json::success_response(paginate(items, &params), 200)
}

async fn create_currency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreatePricingCurrency>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingCurrency> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let code = input.code.trim().to_uppercase();
        validate_currency(&code)?;
        if state.currencies.contains_key(&code) {
            return Err(UnifiedError::conflict(format!(
                "Currency {code} already exists"
            )));
        }
        let exponent = input.exponent.unwrap_or_else(|| {
            if is_zero_decimal_currency(&code) {
                0
            } else {
                2
            }
        });
        if exponent != 0 && exponent != 2 {
            return Err(UnifiedError::bad_request(
                "Currency exponent must be 0 or 2",
            ));
        }
        if let (Some(num), Some(den)) = (input.rate_numerator, input.rate_denominator) {
            if num <= 0 || den <= 0 {
                return Err(UnifiedError::bad_request(
                    "Exchange rate numerator and denominator must be positive",
                ));
            }
        }
        let now = now();
        let currency = PricingCurrency {
            code,
            exponent,
            active: input.active.unwrap_or(true),
            rate_numerator: input.rate_numerator,
            rate_denominator: input.rate_denominator,
            rate_fetched_at: None,
            created_at: now,
            updated_at: now,
        };
        state
            .currencies
            .insert(currency.code.clone(), currency.clone());
        Ok(currency)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn update_currency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePricingCurrency>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingCurrency> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let code = path.as_str().to_uppercase();
        let mut currency = state
            .currencies
            .get(&code)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Currency not found"))?;
        let input = body.into_inner();
        if let Some(exponent) = input.exponent {
            if exponent != 0 && exponent != 2 {
                return Err(UnifiedError::bad_request(
                    "Currency exponent must be 0 or 2",
                ));
            }
            currency.exponent = exponent;
        }
        if let Some(active) = input.active {
            currency.active = active;
        }
        if let Some(num) = input.rate_numerator {
            if num <= 0 {
                return Err(UnifiedError::bad_request(
                    "Exchange rate numerator must be positive",
                ));
            }
            currency.rate_numerator = Some(num);
        }
        if let Some(den) = input.rate_denominator {
            if den <= 0 {
                return Err(UnifiedError::bad_request(
                    "Exchange rate denominator must be positive",
                ));
            }
            currency.rate_denominator = Some(den);
        }
        if input.rate_numerator.is_some() || input.rate_denominator.is_some() {
            currency.rate_fetched_at = Some(now());
        }
        currency.updated_at = now();
        state
            .currencies
            .insert(currency.code.clone(), currency.clone());
        Ok(currency)
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// Convert an amount with a stored integer rate ratio only. A
/// missing rate is an explicit error, never a guessed conversion.
async fn convert_currency_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<ConvertCurrencyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        let input = body.into_inner();
        let from = resolve_money(&input.amount, None)?;
        require_currency_known(&state, &from.currency)?;
        let target = input.target_currency.trim().to_uppercase();
        require_currency_known(&state, &target)?;
        if from.currency == target {
            return Ok(json!({
                "amount_scaled": from.amount_scaled,
                "amount": format_scaled(from.amount_scaled, &target),
                "currency": target,
                "rate_numerator": 1,
                "rate_denominator": 1,
                "stale": false,
            }));
        }
        let currency = state
            .currencies
            .get(&target)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Target currency not found"))?;
        let (num, den) = match (currency.rate_numerator, currency.rate_denominator) {
            (Some(num), Some(den)) => (num, den),
            _ => {
                return Err(UnifiedError::unprocessable(format!(
                    "No stored exchange rate for {target}: enter a rate before converting"
                )));
            }
        };
        let converted = convert_currency(from.amount_scaled, num, den, &target)?;
        Ok(json!({
            "amount_scaled": converted,
            "amount": format_scaled(converted, &target),
            "currency": target,
            "rate_numerator": num,
            "rate_denominator": den,
            "stale": false,
        }))
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module.
// ---------------------------------------------------------------------------

pub struct PricingModule {
    state: Arc<PricingState>,
}

impl PricingModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(PricingState::new()),
        }
    }
}

impl Default for PricingModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PricingModule {
    fn id(&self) -> &'static str {
        "pricing"
    }

    fn name(&self) -> &'static str {
        "Smart Pricing"
    }

    fn description(&self) -> &'static str {
        "Price recommendations (cost-plus, tiered, bundle), discount and coupon engine with declared evaluation order, per-day proration, and server-side invoice reconciliation with credit notes. All money is integer scaled minor units (NUMERIC(19,4)); the engine never sets a price silently and never mutates an order or an invoice."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn available_as_product(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["billing", "store", "products"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "pricing.discount.evaluate_requested",
            "pricing",
            Arc::new(move |ctx: &mut HookContext| {
                on_discount_evaluate_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "pricing.invoice.validate_requested",
            "pricing",
            Arc::new(move |ctx: &mut HookContext| {
                on_invoice_validate_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "pricing.ai.recommend_requested",
            "pricing",
            Arc::new(move |ctx: &mut HookContext| {
                on_ai_recommend_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/pricing")
                // Recommendations (suggestions only; confirm creates
                // a draft price version, never an active price).
                .route("/recommendations/cost-plus", web::post().to(create_cost_plus_recommendation))
                .route("/recommendations/tiered", web::post().to(create_tiered_recommendation))
                .route("/recommendations/bundle", web::post().to(create_bundle_recommendation))
                .route("/recommendations", web::get().to(list_recommendations))
                .route("/recommendations/{id}", web::get().to(get_recommendation))
                .route("/recommendations/{id}/confirm", web::post().to(confirm_recommendation))
                .route("/recommendations/{id}/narrative", web::get().to(get_recommendation_narrative))
                // Discount rules.
                .route("/rules", web::get().to(list_rules))
                .route("/rules", web::post().to(create_rule))
                .route("/rules/{id}", web::get().to(get_rule))
                .route("/rules/{id}", web::put().to(update_rule))
                .route("/rules/{id}", web::delete().to(delete_rule))
                // Coupons.
                .route("/coupons", web::get().to(list_coupons))
                .route("/coupons", web::post().to(create_coupon))
                .route("/coupons/{id}", web::get().to(get_coupon))
                .route("/coupons/{id}", web::put().to(update_coupon))
                .route("/coupons/{id}", web::delete().to(delete_coupon))
                .route("/coupons/{id}/redeem", web::post().to(redeem_coupon))
                .route("/coupons/{id}/redemptions", web::get().to(list_coupon_redemptions))
                // Discount evaluation.
                .route("/discounts/evaluate", web::post().to(evaluate_discounts_endpoint))
                // Market reference prices.
                .route("/reference-prices", web::get().to(list_reference_prices))
                .route("/reference-prices", web::post().to(create_reference_price))
                .route("/reference-prices/summary", web::get().to(summarize_reference_prices))
                .route("/reference-prices/{id}", web::get().to(get_reference_price))
                .route("/reference-prices/{id}", web::delete().to(delete_reference_price))
                // Price versions and approvals.
                .route("/price-versions", web::get().to(list_price_versions))
                .route("/price-versions", web::post().to(create_price_version))
                .route("/price-versions/{id}", web::get().to(get_price_version))
                .route("/price-versions/{id}/submit", web::post().to(submit_price_version))
                .route("/price-versions/{id}/approve", web::post().to(approve_price_version))
                .route("/price-versions/{id}/reject", web::post().to(reject_price_version))
                .route("/price-versions/{id}/activate", web::post().to(activate_price_version))
                .route("/price-versions/{id}/approvals", web::get().to(list_price_approvals))
                // Tiers.
                .route("/tiers", web::get().to(list_tiers))
                .route("/tiers", web::post().to(create_tier))
                .route("/tiers/price", web::get().to(price_with_stored_tiers))
                .route("/tiers/{id}", web::delete().to(delete_tier))
                // Bundles.
                .route("/bundles", web::get().to(list_bundles))
                .route("/bundles", web::post().to(create_bundle))
                .route("/bundles/{id}", web::get().to(get_bundle))
                .route("/bundles/{id}/price", web::post().to(price_bundle))
                // Proration.
                .route("/proration/compute", web::post().to(compute_proration))
                .route("/proration", web::get().to(list_prorations))
                .route("/proration/{id}", web::get().to(get_proration))
                // Invoice preview, reconciliation, credit notes.
                .route("/invoices/preview", web::post().to(preview_invoice))
                .route("/invoices/reconcile", web::post().to(reconcile_invoice_endpoint))
                .route("/reconciliations", web::get().to(list_reconciliations))
                .route("/reconciliations/{id}", web::get().to(get_reconciliation))
                .route("/credit-notes", web::get().to(list_credit_notes))
                .route("/credit-notes", web::post().to(create_credit_note))
                .route("/credit-notes/{id}", web::get().to(get_credit_note))
                .route("/credit-notes/{id}/reconcile", web::post().to(reconcile_credit_note_endpoint))
                // Settings, currencies, conversion.
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/currencies", web::get().to(list_currencies))
                .route("/currencies", web::post().to(create_currency))
                .route("/currencies/{code}", web::put().to(update_currency))
                .route("/convert", web::post().to(convert_currency_endpoint)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "POST", path: "/api/v1/pricing/recommendations/cost-plus", owner: "pricing", description: "Compute a cost-plus price recommendation (suggestion only; shows total cost, margin, suggested price)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/recommendations/tiered", owner: "pricing", description: "Compute a tiered price recommendation for a quantity, with the tier breakdown." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/recommendations/bundle", owner: "pricing", description: "Compute a bundle price recommendation, line by line." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/recommendations", owner: "pricing", description: "List price recommendations (paginated; filter by product_id, kind, status)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/recommendations/{id}", owner: "pricing", description: "Get one price recommendation." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/recommendations/{id}/confirm", owner: "pricing", description: "Confirm a recommendation: creates a draft price version, never an active price." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/recommendations/{id}/narrative", owner: "pricing", description: "AI narrative draft for a recommendation, built from the engine numbers only." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/rules", owner: "pricing", description: "List discount rules (paginated; filter by kind, status, product_id)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/rules", owner: "pricing", description: "Create a discount rule with a declared evaluation order." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/rules/{id}", owner: "pricing", description: "Get one discount rule." },
            RouteInfo { method: "PUT", path: "/api/v1/pricing/rules/{id}", owner: "pricing", description: "Update a discount rule." },
            RouteInfo { method: "DELETE", path: "/api/v1/pricing/rules/{id}", owner: "pricing", description: "Delete a discount rule." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/coupons", owner: "pricing", description: "List coupons (paginated; filter by kind, active)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/coupons", owner: "pricing", description: "Create a coupon with validity window and usage limits." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/coupons/{id}", owner: "pricing", description: "Get one coupon." },
            RouteInfo { method: "PUT", path: "/api/v1/pricing/coupons/{id}", owner: "pricing", description: "Update a coupon." },
            RouteInfo { method: "DELETE", path: "/api/v1/pricing/coupons/{id}", owner: "pricing", description: "Delete a coupon." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/coupons/{id}/redeem", owner: "pricing", description: "Record one coupon redemption for a customer (re-checks limits)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/coupons/{id}/redemptions", owner: "pricing", description: "List redemptions of one coupon (paginated; filter by customer_id)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/discounts/evaluate", owner: "pricing", description: "Evaluate discounts for order lines in declared rule order, with the full trace." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/reference-prices", owner: "pricing", description: "List human-entered market reference prices (paginated; filter by product_id, source)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/reference-prices", owner: "pricing", description: "Record a market reference price with its source and observation date." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/reference-prices/summary", owner: "pricing", description: "Summarize reference prices for one product (count, lowest, highest, median per currency)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/reference-prices/{id}", owner: "pricing", description: "Get one reference price." },
            RouteInfo { method: "DELETE", path: "/api/v1/pricing/reference-prices/{id}", owner: "pricing", description: "Delete a reference price." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/price-versions", owner: "pricing", description: "List versioned prices (paginated; filter by product_id, status)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/price-versions", owner: "pricing", description: "Create a draft price version." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/price-versions/{id}", owner: "pricing", description: "Get one price version." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/price-versions/{id}/submit", owner: "pricing", description: "Submit a draft price version (above threshold goes to pending_approval)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/price-versions/{id}/approve", owner: "pricing", description: "Approve a price version (second approver when above the threshold)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/price-versions/{id}/reject", owner: "pricing", description: "Reject a price version back to draft." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/price-versions/{id}/activate", owner: "pricing", description: "Activate an approved price version (supersedes the previous active one)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/price-versions/{id}/approvals", owner: "pricing", description: "List approval decisions for one price version." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/tiers", owner: "pricing", description: "List pricing tiers (paginated; filter by product_id)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/tiers", owner: "pricing", description: "Create a pricing tier (overlap with an existing tier is rejected)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/tiers/price", owner: "pricing", description: "Price a quantity against the stored tiers for a product." },
            RouteInfo { method: "DELETE", path: "/api/v1/pricing/tiers/{id}", owner: "pricing", description: "Delete a pricing tier." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/bundles", owner: "pricing", description: "List bundles (paginated; filter by product_id, active)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/bundles", owner: "pricing", description: "Create a bundle with components and a bundle discount." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/bundles/{id}", owner: "pricing", description: "Get one bundle with its components." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/bundles/{id}/price", owner: "pricing", description: "Price a stored bundle, line by line." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/proration/compute", owner: "pricing", description: "Compute proration for a plan or seat change, with the exact daily math." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/proration", owner: "pricing", description: "List proration records (paginated; filter by subscription_id, customer_id, kind)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/proration/{id}", owner: "pricing", description: "Get one proration record." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/invoices/preview", owner: "pricing", description: "Preview an invoice with its reconciliation result (admin sees exact figures first)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/invoices/reconcile", owner: "pricing", description: "Reconcile an invoice before issuing: every component must match exactly." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/reconciliations", owner: "pricing", description: "List reconciliation runs (paginated; filter by invoice_id, matched)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/reconciliations/{id}", owner: "pricing", description: "Get one reconciliation run." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/credit-notes", owner: "pricing", description: "List credit notes (paginated; filter by invoice_id, status)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/credit-notes", owner: "pricing", description: "Create a credit note (lines must add up to the credit amount; issued notes are immutable)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/credit-notes/{id}", owner: "pricing", description: "Get one credit note." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/credit-notes/{id}/reconcile", owner: "pricing", description: "Reconcile a credit note: its lines must equal its amount exactly." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/settings", owner: "pricing", description: "Get the tenant pricing settings." },
            RouteInfo { method: "PUT", path: "/api/v1/pricing/settings", owner: "pricing", description: "Update the tenant pricing settings." },
            RouteInfo { method: "GET", path: "/api/v1/pricing/currencies", owner: "pricing", description: "List pricing currencies (paginated; filter by active)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/currencies", owner: "pricing", description: "Add a pricing currency with an optional stored exchange-rate ratio." },
            RouteInfo { method: "PUT", path: "/api/v1/pricing/currencies/{code}", owner: "pricing", description: "Update a pricing currency or its stored exchange rate." },
            RouteInfo { method: "POST", path: "/api/v1/pricing/convert", owner: "pricing", description: "Convert an amount with a stored exchange-rate ratio only (missing rate is an error)." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PricingModule::new()));
}

// ---------------------------------------------------------------------------
// Unit tests (pure functions only; no database, no network).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
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
}
