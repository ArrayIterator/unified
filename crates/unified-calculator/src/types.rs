//! calculator - types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
