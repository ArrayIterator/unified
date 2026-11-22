// Calculator Enterprise module.
//
// A business and economics calculation toolkit, a SUBMODULE of
// Company Management (`requires = ["company"]`). This is NOT a
// standard calculator: every endpoint runs a business calculation
// server-side (return on investment, break-even point, loan
// amortisation, depreciation, net present value / internal rate of
// return, cash-flow projection, pricing and margin, salary and
// payroll cost, currency conversion, and tax estimation) and returns
// the result together with the exact inputs that produced it.
//
// Persistence model: the in-memory DashMap store is authoritative
// (see the module header of `crate::modules::departments` for the
// pattern); the migration `2026-11-22-000100_calculator` is the
// persistence target for saved calculations, shareable reports,
// calculation history, and configured currency rates.
//
// Events emitted:
//   - `calculator.calculation_saved` — a calculation was saved
//     (`{"calculation_id", "company_id", "calculator_type",
//     "actor"}`).
//   - `calculator.report_shared` — a shareable report was created
//     (`{"report_id", "calculation_id", "company_id",
//     "share_token", "visibility", "actor"}`).
//   - `calculator.calculation_deleted` — a saved calculation was
//     deleted (`{"calculation_id", "company_id", "actor"}`).
//
// Events consumed (outgoing request only):
//   - `tax.estimate_requested` — tax estimation is delegated to the
//     tax module through this event. This module never computes a tax
//     figure itself: when no handler answers, the endpoint returns an
//     explicit `tax_estimate_unavailable` error instead of a guessed
//     number.
//   - `calculator.rate_requested` — answered by this module: other
//     modules may ask for a stored currency rate
//     (`{"base_currency", "target_currency"}`) and receive
//     `{"base_currency", "target_currency", "rate", "provider",
//     "stale"}` when a rate is stored.
//   - `manufacturing.cost.requested` — answered by this module:
//     manufacturing asks for per-product unit costs
//     (`{"items": [{product_id, quantity, unit}], "currency"}`)
//     and receives `{"unit_costs": {product_id: cost}, "gaps":
//     [...], "partial": true/false}`. A unit cost comes only from
//     the latest saved `pricing` calculation whose inputs name
//     that product (its solved `cost`); products with no such
//     calculation are listed in `gaps` with `partial: true` — a
//     cost is never fabricated, and an unpriced product is never
//     answered with zero.
//
// Currency conversion: rates come only from the rates stored through
// this module (configured under a provider settings key, see
// `CalculatorSettings`). No exchange rate is hardcoded anywhere. A
// rate whose `fetched_at` timestamp is older than its configured
// `max_age_seconds` is reported with `stale: true`.
//
// Money: monetary results are rounded to four decimals by the module
// (matching NUMERIC(19,4) in SQL); exchange rates and percentages
// are ratios, not money, and are stored as DOUBLE PRECISION with
// that justification in the migration.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext, HookHandler};
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
// Permissions
// ---------------------------------------------------------------------------

/// Run calculations, convert currency, request a tax estimate, and
/// read saved calculations, reports, history, and rates.
pub const PERM_CALCULATOR_USE: &str = "calculator:use";
/// Create, update, and delete saved calculations, reports, rates,
/// and settings.
pub const PERM_CALCULATOR_MANAGE: &str = "calculator:manage";

/// Calculator kinds this module supports. The same closed set is
/// enforced by a CHECK constraint in the migration.
pub const CALCULATOR_TYPES: [&str; 10] = [
    "roi",
    "break_even",
    "loan",
    "depreciation",
    "npv_irr",
    "cash_flow",
    "pricing",
    "salary",
    "currency",
    "tax_estimate",
];

/// Report visibility values. The same closed set is enforced by a
/// CHECK constraint in the migration.
pub const REPORT_VISIBILITIES: [&str; 3] = ["private", "shared", "public"];

/// Upper bounds that keep responses bounded: amortisation schedules
/// and cash-flow projections are capped, and IRR bisection is
/// bounded so it can never loop forever.
pub const MAX_LOAN_MONTHS: u32 = 600;
pub const MAX_PROJECTION_MONTHS: u32 = 120;
pub const MAX_DEPRECIATION_YEARS: u32 = 100;
pub const IRR_MAX_ITERATIONS: u32 = 200;
pub const IRR_TOLERANCE: f64 = 1e-7;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One calculation a company saved for later reuse.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedCalculation {
    pub id: String,
    pub company_id: String,
    pub calculator_type: String,
    pub title: String,
    pub inputs: Value,
    pub results: Value,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateSavedCalculation {
    pub company_id: String,
    pub calculator_type: String,
    pub title: String,
    #[serde(default)]
    pub inputs: Value,
    #[serde(default)]
    pub results: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSavedCalculation {
    pub title: Option<String>,
    pub inputs: Option<Value>,
    pub results: Option<Value>,
}

/// One shareable report built on a saved calculation. The random
/// `share_token` is the only way to fetch a report through the
/// shared link; it is never derived from the report id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalculationReport {
    pub id: String,
    pub calculation_id: String,
    pub company_id: String,
    pub title: String,
    pub share_token: String,
    pub visibility: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateReport {
    pub calculation_id: String,
    pub company_id: String,
    pub title: String,
    pub visibility: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateReport {
    pub title: Option<String>,
    pub visibility: Option<String>,
}

/// One append-only history entry: every calculation run through this
/// module is recorded here, whether or not it was saved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalculationHistoryEntry {
    pub id: String,
    pub company_id: String,
    pub calculator_type: String,
    pub inputs: Value,
    pub results: Value,
    pub actor: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One stored exchange rate. The rate is a ratio between two
/// currencies, not a money amount, so it is a DOUBLE PRECISION value
/// in SQL (see the migration comment).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrencyRate {
    pub id: String,
    pub base_currency: String,
    pub target_currency: String,
    pub rate: f64,
    pub provider: String,
    pub fetched_at: DateTime<Utc>,
    pub max_age_seconds: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCurrencyRate {
    pub base_currency: String,
    pub target_currency: String,
    pub rate: f64,
    pub provider: Option<String>,
    pub fetched_at: Option<DateTime<Utc>>,
    pub max_age_seconds: Option<u64>,
}

/// Provider settings for currency conversion: rates are only ever
/// read from the provider named here (never from a hardcoded table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalculatorSettings {
    pub rate_provider: String,
    pub rate_max_age_seconds: u64,
}

impl Default for CalculatorSettings {
    fn default() -> Self {
        Self {
            rate_provider: "manual".to_string(),
            rate_max_age_seconds: 3600,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateCalculatorSettings {
    pub rate_provider: Option<String>,
    pub rate_max_age_seconds: Option<u64>,
}

// --- Calculation inputs ----------------------------------------------------
// Input structs derive Serialize as well as Deserialize: handlers echo
// the exact inputs back with `serde_json::to_value`, which needs
// Serialize.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoiInput {
    pub gain: f64,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreakEvenInput {
    pub fixed_costs: f64,
    pub price: f64,
    pub variable_cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoanInput {
    pub principal: f64,
    pub annual_rate: f64,
    pub term_months: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepreciationInput {
    pub cost: f64,
    pub salvage_value: f64,
    pub useful_life_years: u32,
    /// `straight_line` or `declining_balance`.
    pub method: String,
    /// Declining-balance multiplier (2.0 = double-declining).
    /// Ignored for straight-line. Defaults to 2.0.
    pub multiplier: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpvIrrInput {
    pub initial_investment: f64,
    pub cash_flows: Vec<f64>,
    pub discount_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CashFlowInput {
    pub starting_balance: f64,
    pub monthly_inflow: f64,
    pub monthly_outflow: f64,
    pub months: u32,
    /// Optional monthly growth of inflows, in percent. Defaults to 0.
    pub inflow_growth_percent: Option<f64>,
    /// Optional monthly growth of outflows, in percent. Defaults to 0.
    pub outflow_growth_percent: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingInput {
    /// Any two of the three fields solve the third. The missing one
    /// must be absent or null.
    pub price: Option<f64>,
    pub cost: Option<f64>,
    /// Margin as a percent of price (0 <= margin < 100).
    pub margin_percent: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SalaryInput {
    pub gross: f64,
    /// Employer contribution rate, in percent of gross.
    pub employer_contribution_percent: f64,
    /// Employee deduction rate, in percent of gross. Defaults to 0.
    pub employee_deduction_percent: Option<f64>,
    /// `monthly` or `annual`.
    pub period: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrencyConvertInput {
    pub amount: f64,
    pub from_currency: String,
    pub to_currency: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxEstimateInput {
    pub company_id: String,
    /// The tax inputs forwarded to the tax module (income, period,
    /// jurisdiction, and anything else the tax handler expects).
    pub inputs: Value,
    pub jurisdiction: Option<String>,
    pub tax_year: Option<u32>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct CalculatorState {
    pub calculations: DashMap<String, SavedCalculation>,
    pub reports: DashMap<String, CalculationReport>,
    pub history: DashMap<String, CalculationHistoryEntry>,
    pub rates: DashMap<String, CurrencyRate>,
    pub settings: RwLock<CalculatorSettings>,
}

impl CalculatorState {
    fn new() -> Self {
        Self {
            calculations: DashMap::new(),
            reports: DashMap::new(),
            history: DashMap::new(),
            rates: DashMap::new(),
            settings: RwLock::new(CalculatorSettings::default()),
        }
    }

    fn settings_snapshot(&self) -> CalculatorSettings {
        self.settings
            .read()
            .expect("calculator settings lock poisoned")
            .clone()
    }

    /// Append one history entry for a completed calculation.
    fn record_history(
        &self,
        company_id: &str,
        calculator_type: &str,
        actor: &str,
        inputs: Value,
        results: Value,
    ) -> CalculationHistoryEntry {
        let entry = CalculationHistoryEntry {
            id: new_id(),
            company_id: company_id.to_string(),
            calculator_type: calculator_type.to_string(),
            inputs,
            results,
            actor: actor.to_string(),
            created_at: now(),
            updated_at: now(),
        };
        self.history.insert(entry.id.clone(), entry.clone());
        entry
    }

    /// The stored rate for a currency pair, preferring the provider
    /// named in settings, otherwise the most recently fetched rate
    /// for the pair. Returns `None` when no rate is stored: callers
    /// must report the rate as unavailable, never invent one.
    fn rate_for(&self, base: &str, target: &str) -> Option<CurrencyRate> {
        if base == target {
            return None;
        }
        let preferred = self.settings_snapshot().rate_provider;
        let mut best: Option<CurrencyRate> = None;
        for entry in self.rates.iter() {
            let rate = entry.value();
            if rate.base_currency != base || rate.target_currency != target {
                continue;
            }
            let better = match &best {
                None => true,
                Some(current) => {
                    (rate.provider == preferred && current.provider != preferred)
                        || (rate.provider == current.provider
                            && rate.fetched_at > current.fetched_at)
                }
            };
            if better {
                best = Some(rate.clone());
            }
        }
        best
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn new_share_token() -> String {
    Uuid::new_v4().simple().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

/// Round a money amount to four decimals (NUMERIC(19,4)).
fn round_money(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Round a ratio or percent to six decimals for stable output.
fn round_ratio(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
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
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
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
    Ok(Caller { actor, role })
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
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

/// A number that must be finite (rejects NaN and infinity, which
/// JSON cannot represent and which would poison every downstream
/// result).
fn require_finite(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite number"
        )));
    }
    Ok(value)
}

fn require_non_negative(value: f64, field: &str) -> UnifiedResult<f64> {
    let value = require_finite(value, field)?;
    if value < 0.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be negative"
        )));
    }
    Ok(value)
}

fn require_positive(value: f64, field: &str) -> UnifiedResult<f64> {
    let value = require_finite(value, field)?;
    if value <= 0.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be greater than zero"
        )));
    }
    Ok(value)
}

/// A three-letter uppercase ISO 4217 currency code.
fn valid_currency_code(code: &str) -> bool {
    code.len() == 3
        && code
            .chars()
            .all(|ch| ch.is_ascii_uppercase() && ch.is_ascii_alphabetic())
}

/// Wrap a calculation result with the exact inputs that produced it.
fn calculation_response(calculator_type: &str, inputs: Value, results: Value) -> Value {
    json!({
        "calculator": calculator_type,
        "inputs": inputs,
        "results": results,
    })
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>, debug: bool) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Calculation engines (pure functions; every one validates its inputs
// and returns the result together with the exact inputs used).
// ---------------------------------------------------------------------------

/// Return on investment: ROI percent = (gain - cost) / cost * 100.
/// Cost must be greater than zero (division by zero is rejected).
fn compute_roi(input: &RoiInput) -> UnifiedResult<Value> {
    let gain = require_non_negative(input.gain, "gain")?;
    let cost = require_positive(input.cost, "cost")?;
    let net_gain = gain - cost;
    let roi_percent = net_gain / cost * 100.0;
    Ok(json!({
        "gain": round_money(gain),
        "cost": round_money(cost),
        "net_gain": round_money(net_gain),
        "roi_percent": round_ratio(roi_percent),
    }))
}

/// Break-even point in units: fixed costs / (price - variable cost).
/// Price must exceed variable cost, otherwise the break-even point
/// does not exist.
fn compute_break_even(input: &BreakEvenInput) -> UnifiedResult<Value> {
    let fixed = require_non_negative(input.fixed_costs, "fixed_costs")?;
    let price = require_positive(input.price, "price")?;
    let variable = require_non_negative(input.variable_cost, "variable_cost")?;
    if price <= variable {
        return Err(UnifiedError::bad_request(
            "price must be greater than variable_cost: otherwise the break-even point does not exist",
        ));
    }
    let units = fixed / (price - variable);
    Ok(json!({
        "fixed_costs": round_money(fixed),
        "price": round_money(price),
        "variable_cost": round_money(variable),
        "contribution_margin_per_unit": round_money(price - variable),
        "break_even_units": round_ratio(units),
        "break_even_revenue": round_money(units * price),
    }))
}

/// One row of a loan amortisation schedule.
#[derive(Debug, Clone, Serialize)]
struct AmortisationRow {
    period: u32,
    payment: f64,
    principal: f64,
    interest: f64,
    balance: f64,
}

/// Loan / mortgage amortisation: full per-period schedule with
/// principal, interest, and remaining balance.
fn compute_loan(input: &LoanInput) -> UnifiedResult<Value> {
    let principal = require_positive(input.principal, "principal")?;
    let annual_rate = require_non_negative(input.annual_rate, "annual_rate")?;
    if input.term_months < 1 || input.term_months > MAX_LOAN_MONTHS {
        return Err(UnifiedError::bad_request(format!(
            "term_months must be between 1 and {MAX_LOAN_MONTHS}"
        )));
    }
    let months = input.term_months;
    let monthly_rate = annual_rate / 100.0 / 12.0;
    let payment = if monthly_rate == 0.0 {
        principal / months as f64
    } else {
        principal * monthly_rate / (1.0 - (1.0 + monthly_rate).powf(-(months as f64)))
    };
    if !payment.is_finite() || payment <= 0.0 {
        return Err(UnifiedError::bad_request(
            "loan payment could not be computed from these inputs",
        ));
    }
    let mut balance = principal;
    let mut schedule: Vec<AmortisationRow> = Vec::with_capacity(months as usize);
    let mut total_interest = 0.0;
    for period in 1..=months {
        let interest = balance * monthly_rate;
        let mut principal_part = payment - interest;
        // The final payment clears the remaining balance exactly,
        // so rounding never leaves a residual balance behind.
        if period == months {
            principal_part = balance;
        }
        if principal_part < 0.0 {
            return Err(UnifiedError::bad_request(
                "loan payment does not cover the interest: increase the payment or lower the rate",
            ));
        }
        balance -= principal_part;
        total_interest += interest;
        schedule.push(AmortisationRow {
            period,
            payment: round_money(if period == months {
                principal_part + interest
            } else {
                payment
            }),
            principal: round_money(principal_part),
            interest: round_money(interest),
            balance: round_money(balance.max(0.0)),
        });
    }
    Ok(json!({
        "principal": round_money(principal),
        "annual_rate": round_ratio(annual_rate),
        "term_months": months,
        "monthly_payment": round_money(payment),
        "total_paid": round_money(payment * (months - 1) as f64 + schedule.last().map(|row| row.payment).unwrap_or(0.0)),
        "total_interest": round_money(total_interest),
        "schedule": schedule,
    }))
}

/// One row of a depreciation schedule.
#[derive(Debug, Clone, Serialize)]
struct DepreciationRow {
    year: u32,
    depreciation: f64,
    accumulated_depreciation: f64,
    book_value: f64,
}

/// Depreciation: straight-line and declining balance. Cost must
/// exceed the salvage value and the useful life must be at least
/// one year.
fn compute_depreciation(input: &DepreciationInput) -> UnifiedResult<Value> {
    let cost = require_positive(input.cost, "cost")?;
    let salvage = require_non_negative(input.salvage_value, "salvage_value")?;
    if cost <= salvage {
        return Err(UnifiedError::bad_request(
            "cost must be greater than salvage_value",
        ));
    }
    if input.useful_life_years < 1 || input.useful_life_years > MAX_DEPRECIATION_YEARS {
        return Err(UnifiedError::bad_request(format!(
            "useful_life_years must be between 1 and {MAX_DEPRECIATION_YEARS}"
        )));
    }
    let method = input.method.as_str();
    if !matches!(method, "straight_line" | "declining_balance") {
        return Err(UnifiedError::bad_request(
            "method must be one of: straight_line, declining_balance",
        ));
    }
    let life = input.useful_life_years;
    let depreciable = cost - salvage;
    let mut schedule: Vec<DepreciationRow> = Vec::with_capacity(life as usize);
    let mut book_value = cost;
    let mut accumulated = 0.0;
    let annual_straight_line = depreciable / life as f64;
    let multiplier = match input.multiplier {
        Some(value) => {
            let value = require_finite(value, "multiplier")?;
            if !(1.0..=3.0).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "multiplier must be between 1.0 and 3.0",
                ));
            }
            value
        }
        None => 2.0,
    };
    let declining_rate = multiplier / life as f64;
    for year in 1..=life {
        let mut depreciation = match method {
            "straight_line" => annual_straight_line,
            _ => book_value * declining_rate,
        };
        // Never depreciate below the salvage value; the final year
        // clears exactly the remaining depreciable amount.
        let remaining = book_value - salvage;
        if depreciation > remaining || year == life {
            depreciation = remaining;
        }
        if depreciation < 0.0 {
            depreciation = 0.0;
        }
        book_value -= depreciation;
        accumulated += depreciation;
        schedule.push(DepreciationRow {
            year,
            depreciation: round_money(depreciation),
            accumulated_depreciation: round_money(accumulated),
            book_value: round_money(book_value),
        });
    }
    Ok(json!({
        "cost": round_money(cost),
        "salvage_value": round_money(salvage),
        "useful_life_years": life,
        "method": method,
        "multiplier": if method == "declining_balance" { Some(round_ratio(multiplier)) } else { None },
        "annual_depreciation": if method == "straight_line" { Some(round_money(annual_straight_line)) } else { None },
        "total_depreciation": round_money(accumulated),
        "schedule": schedule,
    }))
}

/// Net present value of a cash-flow series at a discount rate
/// (percent). Cash flows are end-of-period, starting in year 1.
fn npv_at_rate(initial_investment: f64, cash_flows: &[f64], rate: f64) -> f64 {
    let mut npv = -initial_investment;
    for (index, flow) in cash_flows.iter().enumerate() {
        npv += flow / (1.0 + rate).powf((index + 1) as f64);
    }
    npv
}

/// Net present value and internal rate of return. IRR is found by
/// bounded bisection (at most `IRR_MAX_ITERATIONS` iterations) over
/// the rate interval [-0.9, 10.0]. When NPV has the same sign at both
/// bounds there is no IRR to find, and when bisection does not
/// converge within the bound the error says so explicitly — the
/// loop is bounded, never infinite.
fn compute_npv_irr(input: &NpvIrrInput) -> UnifiedResult<Value> {
    let initial = require_non_negative(input.initial_investment, "initial_investment")?;
    let discount_rate = require_finite(input.discount_rate, "discount_rate")?;
    if discount_rate < -99.0 {
        return Err(UnifiedError::bad_request(
            "discount_rate must be greater than -99 percent",
        ));
    }
    if input.cash_flows.is_empty() || input.cash_flows.len() > 360 {
        return Err(UnifiedError::bad_request(
            "cash_flows must contain between 1 and 360 entries",
        ));
    }
    let mut flows: Vec<f64> = Vec::with_capacity(input.cash_flows.len());
    for flow in &input.cash_flows {
        flows.push(require_finite(*flow, "cash_flows entry")?);
    }
    let rate = discount_rate / 100.0;
    let npv = npv_at_rate(initial, &flows, rate);
    // IRR by bisection between -90% and +1000%.
    let mut low: f64 = -0.9;
    let mut high: f64 = 10.0;
    let npv_low = npv_at_rate(initial, &flows, low);
    let npv_high = npv_at_rate(initial, &flows, high);
    if !npv_low.is_finite() || !npv_high.is_finite() {
        return Err(UnifiedError::bad_request(
            "IRR cannot be computed from these cash flows",
        ));
    }
    let irr_percent: Option<f64> = if npv_low == 0.0 {
        Some(low * 100.0)
    } else if npv_high == 0.0 {
        Some(high * 100.0)
    } else if npv_low.signum() == npv_high.signum() {
        None
    } else {
        let mut result: Option<f64> = None;
        let mut iterations = 0;
        while iterations < IRR_MAX_ITERATIONS {
            iterations += 1;
            let mid = (low + high) / 2.0;
            let npv_mid = npv_at_rate(initial, &flows, mid);
            if !npv_mid.is_finite() {
                break;
            }
            if npv_mid.abs() < IRR_TOLERANCE || (high - low) < IRR_TOLERANCE {
                result = Some(mid * 100.0);
                break;
            }
            if npv_mid.signum() == npv_low.signum() {
                low = mid;
            } else {
                high = mid;
            }
        }
        match result {
            Some(value) => Some(value),
            None => {
                return Err(UnifiedError::unprocessable(
                    "IRR did not converge within the iteration limit for these cash flows",
                )
                .with_metadata("error_code", json!("irr_not_converged"))
                .with_metadata("max_iterations", json!(IRR_MAX_ITERATIONS)));
            }
        }
    };
    let irr = match irr_percent {
        Some(value) => json!(round_ratio(value)),
        None => Value::Null,
    };
    let mut response = json!({
        "initial_investment": round_money(initial),
        "cash_flows": flows.iter().map(|flow| round_money(*flow)).collect::<Vec<f64>>(),
        "discount_rate": round_ratio(discount_rate),
        "npv": round_money(npv),
        "irr_percent": irr,
    });
    if irr.is_null() {
        response["irr_error"] = json!(
            "No internal rate of return exists for these cash flows: NPV has the same sign at both search bounds"
        );
    }
    Ok(response)
}

/// One month of a cash-flow projection.
#[derive(Debug, Clone, Serialize)]
struct CashFlowRow {
    month: u32,
    inflow: f64,
    outflow: f64,
    net: f64,
    balance: f64,
}

/// Cash-flow projection: monthly inflows and outflows over N months,
/// with optional monthly growth rates for each side.
fn compute_cash_flow(input: &CashFlowInput) -> UnifiedResult<Value> {
    let starting = require_finite(input.starting_balance, "starting_balance")?;
    let inflow = require_non_negative(input.monthly_inflow, "monthly_inflow")?;
    let outflow = require_non_negative(input.monthly_outflow, "monthly_outflow")?;
    let inflow_growth = require_finite(
        input.inflow_growth_percent.unwrap_or(0.0),
        "inflow_growth_percent",
    )?;
    let outflow_growth = require_finite(
        input.outflow_growth_percent.unwrap_or(0.0),
        "outflow_growth_percent",
    )?;
    if inflow_growth < -100.0 || outflow_growth < -100.0 {
        return Err(UnifiedError::bad_request(
            "growth rates must not be below -100 percent",
        ));
    }
    if input.months < 1 || input.months > MAX_PROJECTION_MONTHS {
        return Err(UnifiedError::bad_request(format!(
            "months must be between 1 and {MAX_PROJECTION_MONTHS}"
        )));
    }
    let mut balance = starting;
    let mut rows: Vec<CashFlowRow> = Vec::with_capacity(input.months as usize);
    let mut total_inflow = 0.0;
    let mut total_outflow = 0.0;
    let mut lowest_balance = starting;
    for month in 1..=input.months {
        let month_inflow = inflow * (1.0 + inflow_growth / 100.0).powf((month - 1) as f64);
        let month_outflow = outflow * (1.0 + outflow_growth / 100.0).powf((month - 1) as f64);
        let net = month_inflow - month_outflow;
        balance += net;
        total_inflow += month_inflow;
        total_outflow += month_outflow;
        if balance < lowest_balance {
            lowest_balance = balance;
        }
        rows.push(CashFlowRow {
            month,
            inflow: round_money(month_inflow),
            outflow: round_money(month_outflow),
            net: round_money(net),
            balance: round_money(balance),
        });
    }
    Ok(json!({
        "starting_balance": round_money(starting),
        "monthly_inflow": round_money(inflow),
        "monthly_outflow": round_money(outflow),
        "months": input.months,
        "inflow_growth_percent": round_ratio(inflow_growth),
        "outflow_growth_percent": round_ratio(outflow_growth),
        "total_inflow": round_money(total_inflow),
        "total_outflow": round_money(total_outflow),
        "total_net": round_money(total_inflow - total_outflow),
        "ending_balance": round_money(balance),
        "lowest_balance": round_money(lowest_balance),
        "projection": rows,
    }))
}

/// Pricing, margin, and markup: any two of price, cost, and margin
/// solve the third. Margin is a percent of price and must stay
/// below 100.
fn compute_pricing(input: &PricingInput) -> UnifiedResult<Value> {
    let price = match input.price {
        Some(value) => Some(require_positive(value, "price")?),
        None => None,
    };
    let cost = match input.cost {
        Some(value) => Some(require_non_negative(value, "cost")?),
        None => None,
    };
    let margin = match input.margin_percent {
        Some(value) => {
            let value = require_finite(value, "margin_percent")?;
            if !(0.0..100.0).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "margin_percent must be between 0 (inclusive) and 100 (exclusive)",
                ));
            }
            Some(value)
        }
        None => None,
    };
    let provided = [price.is_some(), cost.is_some(), margin.is_some()]
        .iter()
        .filter(|present| **present)
        .count();
    if provided != 2 {
        return Err(UnifiedError::bad_request(
            "Provide exactly two of price, cost, and margin_percent; the third is solved",
        ));
    }
    let (price, cost, margin) = match (price, cost, margin) {
        (Some(price), Some(cost), None) => {
            if price < cost {
                return Err(UnifiedError::bad_request(
                    "price must not be below cost when solving for margin",
                ));
            }
            let margin = (price - cost) / price * 100.0;
            (price, cost, margin)
        }
        (Some(price), None, Some(margin)) => {
            let cost = price * (1.0 - margin / 100.0);
            (price, cost, margin)
        }
        (None, Some(cost), Some(margin)) => {
            let price = cost / (1.0 - margin / 100.0);
            (price, cost, margin)
        }
        _ => {
            return Err(UnifiedError::bad_request(
                "Provide exactly two of price, cost, and margin_percent; the third is solved",
            ));
        }
    };
    let markup = if cost == 0.0 {
        0.0
    } else {
        (price - cost) / cost * 100.0
    };
    Ok(json!({
        "price": round_money(price),
        "cost": round_money(cost),
        "margin_percent": round_ratio(margin),
        "markup_percent": round_ratio(markup),
        "gross_profit": round_money(price - cost),
    }))
}

/// Salary and payroll cost: gross pay plus employer contributions
/// and employee deductions, for a monthly or annual period.
fn compute_salary(input: &SalaryInput) -> UnifiedResult<Value> {
    let gross = require_positive(input.gross, "gross")?;
    let employer_rate = require_non_negative(
        input.employer_contribution_percent,
        "employer_contribution_percent",
    )?;
    let employee_rate = require_non_negative(
        input.employee_deduction_percent.unwrap_or(0.0),
        "employee_deduction_percent",
    )?;
    if !matches!(input.period.as_str(), "monthly" | "annual") {
        return Err(UnifiedError::bad_request(
            "period must be one of: monthly, annual",
        ));
    }
    let employer_contribution = gross * employer_rate / 100.0;
    let employee_deduction = gross * employee_rate / 100.0;
    let total_employer_cost = gross + employer_contribution;
    let net_pay = gross - employee_deduction;
    let (monthly_gross, annual_gross) = if input.period == "monthly" {
        (gross, gross * 12.0)
    } else {
        (gross / 12.0, gross)
    };
    Ok(json!({
        "gross": round_money(gross),
        "period": input.period,
        "employer_contribution_percent": round_ratio(employer_rate),
        "employer_contribution": round_money(employer_contribution),
        "total_employer_cost": round_money(total_employer_cost),
        "employee_deduction_percent": round_ratio(employee_rate),
        "employee_deduction": round_money(employee_deduction),
        "net_pay": round_money(net_pay),
        "monthly_gross": round_money(monthly_gross),
        "annual_gross": round_money(annual_gross),
        "monthly_total_employer_cost": round_money(if input.period == "monthly" { total_employer_cost } else { total_employer_cost / 12.0 }),
        "annual_total_employer_cost": round_money(if input.period == "annual" { total_employer_cost } else { total_employer_cost * 12.0 }),
    }))
}

// ---------------------------------------------------------------------------
// Handlers: calculations
// ---------------------------------------------------------------------------

/// Run one calculation, record it in history, and return the result
/// with the exact inputs used. `company_id` is taken from the
/// `X-Company-Id` header when present, otherwise from the body
/// field of the same name; history always records a company id.
fn history_company_id(req: &HttpRequest, fallback: &str) -> String {
    req.headers()
        .get("x-company-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| fallback.to_string())
}

async fn calculate_roi(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<RoiInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = compute_roi(&input)?;
        let company_id = history_company_id(&req, "");
        state.record_history(&company_id, "roi", &caller.actor, inputs.clone(), results.clone());
        Ok(calculation_response("roi", inputs, results))
    })();
    json::respond(result, debug)
}

async fn calculate_break_even(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<BreakEvenInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = compute_break_even(&input)?;
        let company_id = history_company_id(&req, "");
        state.record_history(
            &company_id,
            "break_even",
            &caller.actor,
            inputs.clone(),
            results.clone(),
        );
        Ok(calculation_response("break_even", inputs, results))
    })();
    json::respond(result, debug)
}

async fn calculate_loan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<LoanInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = compute_loan(&input)?;
        let company_id = history_company_id(&req, "");
        state.record_history(&company_id, "loan", &caller.actor, inputs.clone(), results.clone());
        Ok(calculation_response("loan", inputs, results))
    })();
    json::respond(result, debug)
}

async fn calculate_depreciation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<DepreciationInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = compute_depreciation(&input)?;
        let company_id = history_company_id(&req, "");
        state.record_history(
            &company_id,
            "depreciation",
            &caller.actor,
            inputs.clone(),
            results.clone(),
        );
        Ok(calculation_response("depreciation", inputs, results))
    })();
    json::respond(result, debug)
}

async fn calculate_npv_irr(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<NpvIrrInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = compute_npv_irr(&input)?;
        let company_id = history_company_id(&req, "");
        state.record_history(
            &company_id,
            "npv_irr",
            &caller.actor,
            inputs.clone(),
            results.clone(),
        );
        Ok(calculation_response("npv_irr", inputs, results))
    })();
    json::respond(result, debug)
}

async fn calculate_cash_flow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<CashFlowInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = compute_cash_flow(&input)?;
        let company_id = history_company_id(&req, "");
        state.record_history(
            &company_id,
            "cash_flow",
            &caller.actor,
            inputs.clone(),
            results.clone(),
        );
        Ok(calculation_response("cash_flow", inputs, results))
    })();
    json::respond(result, debug)
}

async fn calculate_pricing(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<PricingInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = compute_pricing(&input)?;
        let company_id = history_company_id(&req, "");
        state.record_history(
            &company_id,
            "pricing",
            &caller.actor,
            inputs.clone(),
            results.clone(),
        );
        Ok(calculation_response("pricing", inputs, results))
    })();
    json::respond(result, debug)
}

async fn calculate_salary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<SalaryInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = compute_salary(&input)?;
        let company_id = history_company_id(&req, "");
        state.record_history(
            &company_id,
            "salary",
            &caller.actor,
            inputs.clone(),
            results.clone(),
        );
        Ok(calculation_response("salary", inputs, results))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: currency conversion
// ---------------------------------------------------------------------------

async fn convert_currency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<CurrencyConvertInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let amount = require_positive(input.amount, "amount")?;
        let from = input.from_currency.trim().to_string();
        let to = input.to_currency.trim().to_string();
        if !valid_currency_code(&from) {
            return Err(UnifiedError::bad_request(
                "from_currency must be a 3-letter uppercase ISO 4217 code",
            ));
        }
        if !valid_currency_code(&to) {
            return Err(UnifiedError::bad_request(
                "to_currency must be a 3-letter uppercase ISO 4217 code",
            ));
        }
        if from == to {
            let results = json!({
                "amount": round_money(amount),
                "from_currency": from,
                "to_currency": to,
                "rate": 1.0,
                "converted_amount": round_money(amount),
                "provider": null,
                "stale": false,
                "rate_age_seconds": null,
            });
            let inputs = serde_json::to_value(&input)
                .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
            let company_id = history_company_id(&req, "");
            state.record_history(
                &company_id,
                "currency",
                &caller.actor,
                inputs.clone(),
                results.clone(),
            );
            return Ok(calculation_response("currency", inputs, results));
        }
        let rate = state.rate_for(&from, &to).ok_or_else(|| {
            UnifiedError::not_found(format!(
                "No stored exchange rate for {from}/{to}: configure a rate first"
            ))
            .with_metadata("error_code", json!("currency_rate_unavailable"))
            .with_metadata("base_currency", json!(from))
            .with_metadata("target_currency", json!(to))
        })?;
        let age_seconds = now()
            .signed_duration_since(rate.fetched_at)
            .num_seconds()
            .max(0) as u64;
        let stale = age_seconds > rate.max_age_seconds;
        let converted = amount * rate.rate;
        if !converted.is_finite() {
            return Err(UnifiedError::bad_request(
                "converted amount could not be computed from the stored rate",
            ));
        }
        let results = json!({
            "amount": round_money(amount),
            "from_currency": from,
            "to_currency": to,
            "rate": round_ratio(rate.rate),
            "converted_amount": round_money(converted),
            "provider": rate.provider,
            "rate_fetched_at": rate.fetched_at,
            "rate_age_seconds": age_seconds,
            "rate_max_age_seconds": rate.max_age_seconds,
            "stale": stale,
        });
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let company_id = history_company_id(&req, "");
        state.record_history(
            &company_id,
            "currency",
            &caller.actor,
            inputs.clone(),
            results.clone(),
        );
        Ok(calculation_response("currency", inputs, results))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: tax estimation (delegated, never computed here)
// ---------------------------------------------------------------------------

async fn estimate_tax(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<TaxEstimateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let input = body.into_inner();
        let company_id = require_text(&input.company_id, "company_id", 64)?;
        if !input.inputs.is_object() {
            return Err(UnifiedError::bad_request(
                "inputs must be a JSON object",
            ));
        }
        // Delegate to the tax module through the hook bus. This module
        // holds no tax rules and never guesses a tax figure.
        let hook = ctx.hooks.emit(
            "tax.estimate_requested",
            json!({
                "company_id": company_id,
                "inputs": input.inputs,
                "jurisdiction": input.jurisdiction,
                "tax_year": input.tax_year,
                "actor": caller.actor,
            }),
        );
        let mut answer: Option<Value> = None;
        for result in &hook.results {
            if result.get("estimated_tax").and_then(|v| v.as_f64()).is_some()
                || result.get("tax_amount").and_then(|v| v.as_f64()).is_some()
            {
                answer = Some(result.clone());
                break;
            }
        }
        let Some(answer) = answer else {
            return Err(UnifiedError::unprocessable(
                "Tax estimate is unavailable: no tax handler answered tax.estimate_requested",
            )
            .with_metadata("error_code", json!("tax_estimate_unavailable"))
            .with_metadata("company_id", json!(company_id)));
        };
        let estimated_tax = answer
            .get("estimated_tax")
            .or_else(|| answer.get("tax_amount"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        if !estimated_tax.is_finite() || estimated_tax < 0.0 {
            return Err(UnifiedError::unprocessable(
                "Tax estimate is unavailable: the tax handler returned an invalid estimate",
            )
            .with_metadata("error_code", json!("tax_estimate_unavailable")));
        }
        let inputs = serde_json::to_value(&input)
            .map_err(|error| UnifiedError::bad_request(format!("Invalid input: {error}")))?;
        let results = json!({
            "company_id": company_id,
            "estimated_tax": round_money(estimated_tax),
            "currency": answer.get("currency").and_then(|v| v.as_str()).unwrap_or(""),
            "jurisdiction": input.jurisdiction,
            "tax_year": input.tax_year,
            "breakdown": answer.get("breakdown").cloned().unwrap_or(Value::Null),
            "provider": answer.get("provider").and_then(|v| v.as_str()).unwrap_or(""),
        });
        state.record_history(
            &company_id,
            "tax_estimate",
            &caller.actor,
            inputs.clone(),
            results.clone(),
        );
        Ok(calculation_response("tax_estimate", inputs, results))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: saved calculations
// ---------------------------------------------------------------------------

fn valid_calculator_type(value: &str) -> bool {
    CALCULATOR_TYPES.contains(&value)
}

async fn list_calculations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<SavedCalculation>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let params = query.into_inner();
        let mut items: Vec<SavedCalculation> = state
            .calculations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                matches_search(
                    &format!("{} {} {}", item.title, item.calculator_type, item.company_id),
                    &params,
                ) && params
                    .filters
                    .get("company_id")
                    .is_none_or(|company| item.company_id == *company)
                    && params
                        .filters
                        .get("calculator_type")
                        .is_none_or(|kind| item.calculator_type == *kind)
                    && params
                        .filters
                        .get("created_by")
                        .is_none_or(|actor| item.created_by == *actor)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_calculation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<CreateSavedCalculation>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SavedCalculation> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let input = body.into_inner();
        let company_id = require_text(&input.company_id, "company_id", 64)?;
        let title = require_text(&input.title, "title", 200)?;
        if !valid_calculator_type(&input.calculator_type) {
            return Err(UnifiedError::bad_request(format!(
                "calculator_type must be one of: {}",
                CALCULATOR_TYPES.join(", ")
            )));
        }
        if !input.inputs.is_object() || !input.results.is_object() {
            return Err(UnifiedError::bad_request(
                "inputs and results must be JSON objects",
            ));
        }
        let calculation = SavedCalculation {
            id: new_id(),
            company_id: company_id.clone(),
            calculator_type: input.calculator_type.clone(),
            title,
            inputs: input.inputs,
            results: input.results,
            created_by: caller.actor.clone(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .calculations
            .insert(calculation.id.clone(), calculation.clone());
        ctx.hooks.emit(
            "calculator.calculation_saved",
            json!({
                "calculation_id": calculation.id,
                "company_id": company_id,
                "calculator_type": calculation.calculator_type,
                "actor": caller.actor,
            }),
        );
        Ok(calculation)
    })();
    respond_created(result, debug)
}

async fn get_calculation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SavedCalculation> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        state
            .calculations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Saved calculation not found"))
    })();
    json::respond(result, debug)
}

async fn update_calculation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSavedCalculation>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SavedCalculation> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let input = body.into_inner();
        if let Some(title) = &input.title {
            require_text(title, "title", 200)?;
        }
        if let Some(inputs) = &input.inputs {
            if !inputs.is_object() {
                return Err(UnifiedError::bad_request("inputs must be a JSON object"));
            }
        }
        if let Some(results) = &input.results {
            if !results.is_object() {
                return Err(UnifiedError::bad_request("results must be a JSON object"));
            }
        }
        let mut calculation = state
            .calculations
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Saved calculation not found"))?;
        if let Some(title) = input.title {
            calculation.title = title.trim().to_string();
        }
        if let Some(inputs) = input.inputs {
            calculation.inputs = inputs;
        }
        if let Some(results) = input.results {
            calculation.results = results;
        }
        calculation.updated_at = now();
        let updated = calculation.clone();
        drop(calculation);
        let _ = caller;
        Ok(updated)
    })();
    json::respond(result, debug)
}

async fn delete_calculation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let calculation_id = path.as_str().to_string();
        let removed = state
            .calculations
            .remove(&calculation_id)
            .map(|(_, item)| item);
        let Some(removed) = removed else {
            return Err(UnifiedError::not_found("Saved calculation not found"));
        };
        // Reports built on the deleted calculation go with it.
        state
            .reports
            .retain(|_, report| report.calculation_id != calculation_id);
        ctx.hooks.emit(
            "calculator.calculation_deleted",
            json!({
                "calculation_id": calculation_id,
                "company_id": removed.company_id,
                "actor": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "id": calculation_id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: shareable reports
// ---------------------------------------------------------------------------

async fn list_reports(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<CalculationReport>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let params = query.into_inner();
        let mut items: Vec<CalculationReport> = state
            .reports
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                matches_search(
                    &format!("{} {} {}", item.title, item.calculation_id, item.company_id),
                    &params,
                ) && params
                    .filters
                    .get("company_id")
                    .is_none_or(|company| item.company_id == *company)
                    && params
                        .filters
                        .get("visibility")
                        .is_none_or(|visibility| item.visibility == *visibility)
                    && params
                        .filters
                        .get("calculation_id")
                        .is_none_or(|calculation| item.calculation_id == *calculation)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<CreateReport>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalculationReport> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let input = body.into_inner();
        let company_id = require_text(&input.company_id, "company_id", 64)?;
        let title = require_text(&input.title, "title", 200)?;
        let visibility = input.visibility.unwrap_or_else(|| "private".to_string());
        if !REPORT_VISIBILITIES.contains(&visibility.as_str()) {
            return Err(UnifiedError::bad_request(
                "visibility must be one of: private, shared, public",
            ));
        }
        if !state.calculations.contains_key(input.calculation_id.as_str()) {
            return Err(UnifiedError::not_found(
                "Saved calculation not found for this report",
            ));
        }
        let report = CalculationReport {
            id: new_id(),
            calculation_id: input.calculation_id.clone(),
            company_id: company_id.clone(),
            title,
            share_token: new_share_token(),
            visibility: visibility.clone(),
            created_by: caller.actor.clone(),
            created_at: now(),
            updated_at: now(),
        };
        state.reports.insert(report.id.clone(), report.clone());
        ctx.hooks.emit(
            "calculator.report_shared",
            json!({
                "report_id": report.id,
                "calculation_id": report.calculation_id,
                "company_id": company_id,
                "share_token": report.share_token,
                "visibility": visibility,
                "actor": caller.actor,
            }),
        );
        Ok(report)
    })();
    respond_created(result, debug)
}

async fn get_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalculationReport> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let report = state
            .reports
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Report not found"))?;
        if report.visibility == "private" && report.created_by != caller.actor {
            return Err(UnifiedError::forbidden(
                "This report is private: only its creator may read it",
            ));
        }
        Ok(report)
    })();
    json::respond(result, debug)
}

async fn update_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateReport>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalculationReport> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let input = body.into_inner();
        if let Some(title) = &input.title {
            require_text(title, "title", 200)?;
        }
        if let Some(visibility) = &input.visibility {
            if !REPORT_VISIBILITIES.contains(&visibility.as_str()) {
                return Err(UnifiedError::bad_request(
                    "visibility must be one of: private, shared, public",
                ));
            }
        }
        let mut report = state
            .reports
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Report not found"))?;
        if let Some(title) = input.title {
            report.title = title.trim().to_string();
        }
        if let Some(visibility) = input.visibility {
            report.visibility = visibility;
        }
        report.updated_at = now();
        let updated = report.clone();
        drop(report);
        let _ = caller;
        Ok(updated)
    })();
    json::respond(result, debug)
}

async fn delete_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let _ = caller;
        state
            .reports
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Report not found"))
    })();
    json::respond(result, debug)
}

/// Fetch a report through its share token. The token is random and
/// unguessable, but the endpoint still requires an authenticated
/// caller with `calculator:use`; private reports additionally
/// require the caller to be the creator.
async fn get_shared_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let token = path.as_str();
        let report = state
            .reports
            .iter()
            .find(|entry| entry.value().share_token == token)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Shared report not found"))?;
        if report.visibility == "private" && report.created_by != caller.actor {
            return Err(UnifiedError::forbidden(
                "This report is private: only its creator may read it",
            ));
        }
        let calculation = state
            .calculations
            .get(report.calculation_id.as_str())
            .map(|entry| entry.value().clone());
        Ok(json!({
            "report": report,
            "calculation": calculation,
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: history (read-only audit of every calculation run)
// ---------------------------------------------------------------------------

async fn list_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<CalculationHistoryEntry>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let params = query.into_inner();
        let mut items: Vec<CalculationHistoryEntry> = state
            .history
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                matches_search(
                    &format!("{} {} {}", item.calculator_type, item.company_id, item.actor),
                    &params,
                ) && params
                    .filters
                    .get("company_id")
                    .is_none_or(|company| item.company_id == *company)
                    && params
                        .filters
                        .get("calculator_type")
                        .is_none_or(|kind| item.calculator_type == *kind)
                    && params
                        .filters
                        .get("actor")
                        .is_none_or(|actor| item.actor == *actor)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_history_entry(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalculationHistoryEntry> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        state
            .history
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("History entry not found"))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: currency rates and settings
// ---------------------------------------------------------------------------

async fn list_rates(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<CurrencyRate>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        let params = query.into_inner();
        let mut items: Vec<CurrencyRate> = state
            .rates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                matches_search(
                    &format!("{} {} {}", item.base_currency, item.target_currency, item.provider),
                    &params,
                ) && params
                    .filters
                    .get("base_currency")
                    .is_none_or(|base| item.base_currency == *base)
                    && params
                        .filters
                        .get("target_currency")
                        .is_none_or(|target| item.target_currency == *target)
                    && params
                        .filters
                        .get("provider")
                        .is_none_or(|provider| item.provider == *provider)
            })
            .collect();
        items.sort_by(|a, b| {
            a.base_currency
                .cmp(&b.base_currency)
                .then(a.target_currency.cmp(&b.target_currency))
        });
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_rate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<CreateCurrencyRate>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CurrencyRate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let input = body.into_inner();
        let base = input.base_currency.trim().to_string();
        let target = input.target_currency.trim().to_string();
        if !valid_currency_code(&base) {
            return Err(UnifiedError::bad_request(
                "base_currency must be a 3-letter uppercase ISO 4217 code",
            ));
        }
        if !valid_currency_code(&target) {
            return Err(UnifiedError::bad_request(
                "target_currency must be a 3-letter uppercase ISO 4217 code",
            ));
        }
        if base == target {
            return Err(UnifiedError::bad_request(
                "base_currency and target_currency must differ",
            ));
        }
        let rate = require_finite(input.rate, "rate")?;
        if rate <= 0.0 {
            return Err(UnifiedError::bad_request("rate must be greater than zero"));
        }
        let provider = match input.provider {
            Some(value) => require_text(&value, "provider", 120)?,
            None => state.settings_snapshot().rate_provider,
        };
        let max_age = match input.max_age_seconds {
            Some(seconds) if seconds >= 1 => seconds,
            Some(_) => {
                return Err(UnifiedError::bad_request(
                    "max_age_seconds must be at least 1",
                ));
            }
            None => state.settings_snapshot().rate_max_age_seconds,
        };
        // One stored rate per (base, target, provider): a new rate
        // for the same triple replaces the old one.
        state.rates.retain(|_, existing| {
            !(existing.base_currency == base
                && existing.target_currency == target
                && existing.provider == provider)
        });
        let record = CurrencyRate {
            id: new_id(),
            base_currency: base,
            target_currency: target,
            rate,
            provider,
            fetched_at: input.fetched_at.unwrap_or_else(now),
            max_age_seconds: max_age,
            created_at: now(),
            updated_at: now(),
        };
        state.rates.insert(record.id.clone(), record.clone());
        let _ = caller;
        Ok(record)
    })();
    respond_created(result, debug)
}

async fn delete_rate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let _ = caller;
        state
            .rates
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Currency rate not found"))
    })();
    json::respond(result, debug)
}

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalculatorSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_USE)?;
        Ok(state.settings_snapshot())
    })();
    json::respond(result, debug)
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CalculatorState>>,
    req: HttpRequest,
    body: web::Json<UpdateCalculatorSettings>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalculatorSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALCULATOR_MANAGE)?;
        let input = body.into_inner();
        if let Some(provider) = &input.rate_provider {
            require_text(provider, "rate_provider", 120)?;
        }
        if let Some(seconds) = input.rate_max_age_seconds {
            if seconds < 1 {
                return Err(UnifiedError::bad_request(
                    "rate_max_age_seconds must be at least 1",
                ));
            }
        }
        let mut settings = state
            .settings
            .write()
            .expect("calculator settings lock poisoned");
        if let Some(provider) = input.rate_provider {
            settings.rate_provider = provider.trim().to_string();
        }
        if let Some(seconds) = input.rate_max_age_seconds {
            settings.rate_max_age_seconds = seconds;
        }
        let updated = settings.clone();
        drop(settings);
        let _ = caller;
        Ok(updated)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// The unit cost for a product from the latest saved pricing
/// calculation whose inputs name that product. The cost is the
/// solved `cost` in the calculation's results — stored data, never
/// computed here and never guessed. When the request carries a
/// company id, only calculations of that company count. Returns
/// the cost plus the calculation it came from, for traceability.
fn latest_pricing_cost(
    state: &CalculatorState,
    product_id: &str,
    company_id: Option<&str>,
) -> Option<(f64, String)> {
    state
        .calculations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|calculation| calculation.calculator_type == "pricing")
        .filter(|calculation| {
            company_id.is_none_or(|company| calculation.company_id == company)
        })
        .filter(|calculation| {
            calculation
                .inputs
                .get("product_id")
                .and_then(|value| value.as_str())
                == Some(product_id)
        })
        .filter_map(|calculation| {
            let cost = calculation.results.get("cost").and_then(|value| value.as_f64())?;
            if cost.is_finite() && cost >= 0.0 {
                Some((calculation.created_at, cost, calculation.id.clone()))
            } else {
                None
            }
        })
        .max_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, cost, id)| (cost, id))
}

/// Answer `manufacturing.cost.requested` with per-product unit
/// costs from saved pricing calculations (see
/// `latest_pricing_cost`). The answer carries `unit_costs`
/// (product id to unit cost), `gaps` (one entry per product with
/// no stored cost, so the caller can report `partial: true`),
/// `partial`, and `sources` (product id to the calculation the
/// cost came from). Products with no stored cost are listed in
/// `gaps` — never answered with zero, never fabricated. The
/// request may carry `items: [{product_id, ...}]` (the shape
/// manufacturing sends) or a single `product_id`.
fn on_manufacturing_cost_requested(state: Arc<CalculatorState>, ctx: &mut HookContext) {
    let mut product_ids: Vec<String> = ctx
        .get("items")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("product_id").and_then(|value| value.as_str()))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if product_ids.is_empty() {
        if let Some(product_id) = ctx
            .get("product_id")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            product_ids.push(product_id.to_string());
        }
    }
    if product_ids.is_empty() {
        return;
    }
    product_ids.sort();
    product_ids.dedup();
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut unit_costs = serde_json::Map::new();
    let mut sources = serde_json::Map::new();
    let mut gaps: Vec<Value> = Vec::new();
    for product_id in &product_ids {
        match latest_pricing_cost(&state, product_id, company_id.as_deref()) {
            Some((cost, calculation_id)) => {
                unit_costs.insert(product_id.clone(), json!(round_money(cost)));
                sources.insert(product_id.clone(), json!(calculation_id));
            }
            None => gaps.push(json!({
                "product_id": product_id,
                "reason": "No saved pricing calculation holds a unit cost for this product",
            })),
        }
    }
    let partial = !gaps.is_empty();
    let answer = json!({
        "unit_costs": unit_costs,
        "gaps": gaps,
        "partial": partial,
        "sources": sources,
    });
    ctx.set("unit_costs", answer["unit_costs"].clone());
    ctx.set("gaps", answer["gaps"].clone());
    ctx.set("partial", json!(partial));
    ctx.add_result(answer);
}

/// Answer `calculator.rate_requested` with the stored rate for a
/// currency pair, when one is stored. No result means "no rate
/// stored" and the requester must treat the rate as unavailable.
fn on_rate_requested(state: Arc<CalculatorState>, ctx: &mut HookContext) {
    let base = ctx
        .get("base_currency")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let target = ctx
        .get("target_currency")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if !valid_currency_code(&base) || !valid_currency_code(&target) {
        return;
    }
    if let Some(rate) = state.rate_for(&base, &target) {
        let age_seconds = now()
            .signed_duration_since(rate.fetched_at)
            .num_seconds()
            .max(0) as u64;
        ctx.add_result(json!({
            "base_currency": rate.base_currency,
            "target_currency": rate.target_currency,
            "rate": round_ratio(rate.rate),
            "provider": rate.provider,
            "stale": age_seconds > rate.max_age_seconds,
            "rate_age_seconds": age_seconds,
        }));
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct CalculatorModule {
    state: Arc<CalculatorState>,
}

impl CalculatorModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(CalculatorState::new()),
        }
    }
}

impl Default for CalculatorModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CalculatorModule {
    fn id(&self) -> &'static str {
        "calculator"
    }

    fn name(&self) -> &'static str {
        "Calculator Enterprise"
    }

    fn description(&self) -> &'static str {
        "Business and economics calculation toolkit (ROI, break-even, loan amortisation, depreciation, NPV/IRR, cash-flow projection, pricing, salary, currency conversion, tax estimation) as a submodule of Company Management."
    }

    fn requires(&self) -> &[&'static str] {
        &["company"]
    }

    fn available_as_product(&self) -> bool {
        false
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_rate_requested(state.clone(), ctx));
        bus.register("calculator.rate_requested", "calculator", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_manufacturing_cost_requested(state.clone(), ctx)
        });
        bus.register(
            "manufacturing.cost.requested",
            "calculator",
            handler,
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/calculator")
                // Literal segments first: they must win over `/{id}`.
                .route("/roi", web::post().to(calculate_roi))
                .route("/break-even", web::post().to(calculate_break_even))
                .route("/loan", web::post().to(calculate_loan))
                .route("/depreciation", web::post().to(calculate_depreciation))
                .route("/npv-irr", web::post().to(calculate_npv_irr))
                .route("/cash-flow", web::post().to(calculate_cash_flow))
                .route("/pricing", web::post().to(calculate_pricing))
                .route("/salary", web::post().to(calculate_salary))
                .route("/currency-convert", web::post().to(convert_currency))
                .route("/tax-estimate", web::post().to(estimate_tax))
                .route("/calculations", web::get().to(list_calculations))
                .route("/calculations", web::post().to(create_calculation))
                .route("/calculations/{id}", web::get().to(get_calculation))
                .route("/calculations/{id}", web::put().to(update_calculation))
                .route("/calculations/{id}", web::delete().to(delete_calculation))
                .route("/reports", web::get().to(list_reports))
                .route("/reports", web::post().to(create_report))
                .route("/reports/shared/{token}", web::get().to(get_shared_report))
                .route("/reports/{id}", web::get().to(get_report))
                .route("/reports/{id}", web::put().to(update_report))
                .route("/reports/{id}", web::delete().to(delete_report))
                .route("/history", web::get().to(list_history))
                .route("/history/{id}", web::get().to(get_history_entry))
                .route("/rates", web::get().to(list_rates))
                .route("/rates", web::post().to(create_rate))
                .route("/rates/{id}", web::delete().to(delete_rate))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "POST", path: "/api/v1/calculator/roi", owner: "calculator", description: "Compute return on investment (gain, cost; cost must be greater than zero)." },
            RouteInfo { method: "POST", path: "/api/v1/calculator/break-even", owner: "calculator", description: "Compute the break-even point in units and revenue (price must exceed variable cost)." },
            RouteInfo { method: "POST", path: "/api/v1/calculator/loan", owner: "calculator", description: "Compute a loan amortisation schedule (principal, annual rate, term months)." },
            RouteInfo { method: "POST", path: "/api/v1/calculator/depreciation", owner: "calculator", description: "Compute a depreciation schedule (straight-line or declining balance)." },
            RouteInfo { method: "POST", path: "/api/v1/calculator/npv-irr", owner: "calculator", description: "Compute net present value and internal rate of return (bounded bisection; explicit error when IRR does not converge)." },
            RouteInfo { method: "POST", path: "/api/v1/calculator/cash-flow", owner: "calculator", description: "Compute a monthly cash-flow projection over N months." },
            RouteInfo { method: "POST", path: "/api/v1/calculator/pricing", owner: "calculator", description: "Solve pricing, cost, or margin from any two of the three." },
            RouteInfo { method: "POST", path: "/api/v1/calculator/salary", owner: "calculator", description: "Compute salary and payroll cost (gross, employer contributions, period)." },
            RouteInfo { method: "POST", path: "/api/v1/calculator/currency-convert", owner: "calculator", description: "Convert currency using a stored rate only; stale rates are flagged." },
            RouteInfo { method: "POST", path: "/api/v1/calculator/tax-estimate", owner: "calculator", description: "Request a tax estimate from the tax module; explicit tax_estimate_unavailable error when no handler answers." },
            RouteInfo { method: "GET", path: "/api/v1/calculator/calculations", owner: "calculator", description: "List saved calculations (paginated; filter by company_id, calculator_type, created_by)." },
            RouteInfo { method: "POST", path: "/api/v1/calculator/calculations", owner: "calculator", description: "Save a calculation for a company." },
            RouteInfo { method: "GET", path: "/api/v1/calculator/calculations/{id}", owner: "calculator", description: "Get one saved calculation." },
            RouteInfo { method: "PUT", path: "/api/v1/calculator/calculations/{id}", owner: "calculator", description: "Update a saved calculation." },
            RouteInfo { method: "DELETE", path: "/api/v1/calculator/calculations/{id}", owner: "calculator", description: "Delete a saved calculation (its reports go with it)." },
            RouteInfo { method: "GET", path: "/api/v1/calculator/reports", owner: "calculator", description: "List shareable calculation reports (paginated; filter by company_id, visibility, calculation_id)." },
            RouteInfo { method: "POST", path: "/api/v1/calculator/reports", owner: "calculator", description: "Create a shareable report with a random share token." },
            RouteInfo { method: "GET", path: "/api/v1/calculator/reports/shared/{token}", owner: "calculator", description: "Fetch a report through its share token (private reports: creator only)." },
            RouteInfo { method: "GET", path: "/api/v1/calculator/reports/{id}", owner: "calculator", description: "Get one report (private reports: creator only)." },
            RouteInfo { method: "PUT", path: "/api/v1/calculator/reports/{id}", owner: "calculator", description: "Update a report title or visibility." },
            RouteInfo { method: "DELETE", path: "/api/v1/calculator/reports/{id}", owner: "calculator", description: "Delete a report." },
            RouteInfo { method: "GET", path: "/api/v1/calculator/history", owner: "calculator", description: "List calculation history (paginated; filter by company_id, calculator_type, actor)." },
            RouteInfo { method: "GET", path: "/api/v1/calculator/history/{id}", owner: "calculator", description: "Get one history entry." },
            RouteInfo { method: "GET", path: "/api/v1/calculator/rates", owner: "calculator", description: "List stored currency rates (paginated; filter by base_currency, target_currency, provider)." },
            RouteInfo { method: "POST", path: "/api/v1/calculator/rates", owner: "calculator", description: "Store a currency rate (replaces the stored rate for the same base/target/provider)." },
            RouteInfo { method: "DELETE", path: "/api/v1/calculator/rates/{id}", owner: "calculator", description: "Delete a stored currency rate." },
            RouteInfo { method: "GET", path: "/api/v1/calculator/settings", owner: "calculator", description: "Get the currency provider settings (provider key and default max rate age)." },
            RouteInfo { method: "PUT", path: "/api/v1/calculator/settings", owner: "calculator", description: "Update the currency provider settings." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CalculatorModule::new()));
}
