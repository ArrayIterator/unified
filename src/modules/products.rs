// Product Management module — the single source of truth for "what
// we sell".
//
// This module owns the full product lifecycle (draft -> active ->
// deprecated -> retired, with validated transitions), product kinds
// (digital, physical, service, subscription, bundle, meeting,
// hosting), variants and options, bundles of products with bundle
// pricing rules, pricing (tiers, per-seat, usage-based, one-time /
// subscription, regional pricing, taxes), inventory for physical
// goods with low-stock alerts, product files and versions that only
// reference Software Manager records (this module never handles
// files itself), product groups / categories / tags mirroring the
// software taxonomy shapes, and vendor ownership validated through
// the `company.exists` hook.
//
// The store sells what this module defines: a product becomes
// buyable in the store when it is listed here, via the
// `product.listed` / `product.delisted` events. Licensed products
// declare their license template and seat counts, billing reads
// price and tax data through the `product.price.quote` hook, and
// support routing reads the owning department through
// `product.support.route` (and can open a ticket through
// `ticket.create_requested`). Entitlement questions are answered by
// the `product.entitlement.check` hook, which asks the store through
// `store.entitlement.check` and says so honestly when no store
// handler answers.
//
// Money is decimal-safe: every amount is stored as integer minor
// units (`i64`, cents for two-decimal currencies, whole units for
// zero-decimal ones) and parsed from decimal strings with exact
// integer arithmetic. No floating-point value is ever used for
// money. Durable storage uses NUMERIC(19,4); see the module report
// for the migration sketch.
//
// Persistence is in-memory (DashMap) in this module; the Diesel
// tables for durable storage are listed in the module report and
// are applied by a later wiring step.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Product kinds accepted by the catalogue.
pub const PRODUCT_KINDS: [&str; 7] = [
    "digital",
    "physical",
    "service",
    "subscription",
    "bundle",
    "meeting",
    "hosting",
];

/// Product lifecycle statuses.
pub const PRODUCT_STATUSES: [&str; 4] = ["draft", "active", "deprecated", "retired"];

/// Pricing models.
pub const PRICING_MODELS: [&str; 4] = ["one_time", "subscription", "usage", "per_seat"];

/// Billing periods for subscriptions.
pub const BILLING_PERIODS: [&str; 3] = ["monthly", "yearly", "weekly"];

/// Bundle pricing rules: `sum` charges the sum of the children,
/// `fixed` charges the bundle's own base price, `discount_percent`
/// charges the sum less the configured discount.
pub const BUNDLE_RULES: [&str; 3] = ["sum", "fixed", "discount_percent"];

/// Taxonomy node kinds, mirroring the software module.
pub const TAXONOMY_KINDS: [&str; 2] = ["group", "category"];

// ---------------------------------------------------------------------------
// Money (integer minor units; never f64)
// ---------------------------------------------------------------------------

/// A decimal-safe amount: integer minor units plus an ISO 4217
/// currency code. Two-decimal currencies store cents (1999 = 19.99
/// USD); zero-decimal currencies store whole units (1999 = 1999 JPY).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Money {
    pub amount_minor: i64,
    pub currency: String,
}

/// A money amount as supplied by API callers: an exact decimal
/// string such as `"19.99"`, parsed with integer arithmetic only.
#[derive(Debug, Clone, Deserialize)]
pub struct MoneyInput {
    pub amount: String,
    #[serde(default)]
    pub currency: Option<String>,
}

/// True for zero-decimal currencies (no minor units), matching the
/// store and payments modules' lists.
fn is_zero_decimal_currency(currency: &str) -> bool {
    matches!(
        currency,
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "UGX" | "XAF" | "XOF" | "XPF"
    )
}

/// Number of fractional digits a currency uses: 0 or 2.
fn currency_exponent(currency: &str) -> u32 {
    if is_zero_decimal_currency(currency) {
        0
    } else {
        2
    }
}

fn validate_currency(currency: &str) -> UnifiedResult<()> {
    if currency.len() == 3 && currency.chars().all(|c| c.is_ascii_alphabetic()) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(
            "Currency must be a 3-letter code such as USD",
        ))
    }
}

/// Parse an exact decimal string into integer minor units. Rejects
/// negative amounts, non-numeric input, and more fractional digits
/// than the currency allows. No floating point is involved.
fn parse_money_amount(amount: &str, currency: &str) -> UnifiedResult<i64> {
    let exponent = currency_exponent(currency);
    let text = amount.trim();
    if text.is_empty() {
        return Err(UnifiedError::bad_request("Amount must not be empty"));
    }
    let (whole, frac) = match text.split_once('.') {
        Some((whole, frac)) => (whole, Some(frac)),
        None => (text, None),
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
    if frac_digits.len() as u32 > exponent {
        return Err(UnifiedError::bad_request(format!(
            "Amount \"{amount}\" has more than {exponent} decimal places for {currency}"
        )));
    }
    let mut frac_padded = frac_digits.to_string();
    while frac_padded.len() < exponent as usize {
        frac_padded.push('0');
    }
    let scale: i128 = 10i128.pow(exponent);
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
    let minor = whole_val
        .checked_mul(scale)
        .and_then(|value| value.checked_add(frac_val))
        .ok_or_else(|| UnifiedError::bad_request("Amount is too large"))?;
    i64::try_from(minor)
        .map_err(|_| UnifiedError::bad_request("Amount is too large"))
}

/// Format integer minor units as an exact decimal string (`"19.99"`,
/// `"1999"` for zero-decimal currencies).
fn format_minor(amount_minor: i64, currency: &str) -> String {
    if currency_exponent(currency) == 0 {
        format!("{amount_minor}")
    } else {
        format!("{}.{:02}", amount_minor / 100, amount_minor % 100)
    }
}

/// Resolve a caller-supplied money input against a fallback
/// currency (the product currency, else USD).
fn resolve_money(input: &MoneyInput, fallback_currency: Option<&str>) -> UnifiedResult<Money> {
    let currency = input
        .currency
        .clone()
        .or_else(|| fallback_currency.map(str::to_string))
        .unwrap_or_else(|| "USD".to_string())
        .to_uppercase();
    validate_currency(&currency)?;
    let amount_minor = parse_money_amount(&input.amount, &currency)?;
    Ok(Money {
        amount_minor,
        currency,
    })
}

/// Round half-up of `value * numerator / denominator` using a wide
/// intermediate, so percentage math never overflows i64 and never
/// touches floating point.
fn mul_div_round_half_up(value: i64, numerator: i64, denominator: i64) -> i64 {
    let wide = value as i128 * numerator as i128;
    let rounded = (wide + denominator as i128 / 2) / denominator as i128;
    rounded.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

// ---------------------------------------------------------------------------
// Products
// ---------------------------------------------------------------------------

/// License terms a licensed product declares. The licensing module
/// reads these through the `product.license.template` hook when it
/// issues a license for an order containing this product.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseTemplate {
    #[serde(default)]
    pub template_name: Option<String>,
    /// Activation seats granted per purchase.
    #[serde(default = "default_seats")]
    pub seats: i32,
    /// Maximum downloads granted per license.
    #[serde(default = "default_download_limit")]
    pub download_limit: u32,
    /// License lifetime in days from purchase; null means no expiry.
    #[serde(default)]
    pub duration_days: Option<i64>,
    /// True when buying this product must issue a license.
    #[serde(default)]
    pub requires_license: bool,
}

fn default_seats() -> i32 {
    1
}

fn default_download_limit() -> u32 {
    10
}

impl Default for LicenseTemplate {
    fn default() -> Self {
        Self {
            template_name: None,
            seats: 1,
            download_limit: 10,
            duration_days: None,
            requires_license: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Product {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    /// One of `digital`, `physical`, `service`, `subscription`,
    /// `bundle`, `meeting`, `hosting`.
    pub kind: String,
    /// `draft`, `active`, `deprecated`, or `retired`.
    pub status: String,
    /// `one_time`, `subscription`, `usage`, or `per_seat`.
    pub pricing_model: String,
    /// `monthly`, `yearly`, or `weekly` for subscriptions.
    #[serde(default)]
    pub billing_period: Option<String>,
    /// Base price in integer minor units.
    pub price: Money,
    /// Price charged on each renewal (defaults to the base price).
    #[serde(default)]
    pub renewal_price: Option<Money>,
    /// Free trial length in days (0 means no trial).
    #[serde(default)]
    pub trial_days: u32,
    /// Owning vendor company id (UUID string); validated against the
    /// company module through the `company.exists` hook when a handler
    /// is registered, otherwise accepted as a plain reference.
    #[serde(default)]
    pub vendor_company_id: Option<String>,
    /// Owning group taxonomy node id.
    #[serde(default)]
    pub group_id: Option<String>,
    /// Owning category taxonomy node id.
    #[serde(default)]
    pub category_id: Option<String>,
    /// Id of the linked software record, when this product sells
    /// software. Files and versions are owned by the Software
    /// Manager; this is a reference only.
    #[serde(default)]
    pub software_id: Option<String>,
    /// License terms for licensed products.
    #[serde(default)]
    pub license_template: LicenseTemplate,
    /// Owning support department id (plain reference; the
    /// departments module owns departments). Support tickets for
    /// this product route to this department.
    #[serde(default)]
    pub support_department_id: Option<String>,
    #[serde(default)]
    pub support_department_name: Option<String>,
    /// Bundle pricing rule for `bundle` products: `sum`, `fixed`,
    /// or `discount_percent`.
    #[serde(default = "default_bundle_rule")]
    pub bundle_rule: String,
    /// Discount in basis points (100 = 1%) for the
    /// `discount_percent` bundle rule.
    #[serde(default)]
    pub bundle_discount_bps: Option<i64>,
    /// True when the product is listed for sale in the store. Only
    /// `active` products can be listed.
    #[serde(default)]
    pub listed: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_bundle_rule() -> String {
    "sum".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProduct {
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_pricing_model")]
    pub pricing_model: String,
    #[serde(default)]
    pub billing_period: Option<String>,
    pub price: MoneyInput,
    #[serde(default)]
    pub renewal_price: Option<MoneyInput>,
    #[serde(default)]
    pub trial_days: Option<u32>,
    #[serde(default)]
    pub vendor_company_id: Option<String>,
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub category_id: Option<String>,
    #[serde(default)]
    pub software_id: Option<String>,
    #[serde(default)]
    pub license_template: Option<LicenseTemplate>,
    #[serde(default)]
    pub support_department_id: Option<String>,
    #[serde(default)]
    pub support_department_name: Option<String>,
    #[serde(default)]
    pub bundle_rule: Option<String>,
    #[serde(default)]
    pub bundle_discount_bps: Option<i64>,
    #[serde(default)]
    pub tag_ids: Vec<String>,
}

fn default_pricing_model() -> String {
    "one_time".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProduct {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub pricing_model: Option<String>,
    #[serde(default)]
    pub billing_period: Option<String>,
    #[serde(default)]
    pub price: Option<MoneyInput>,
    #[serde(default)]
    pub renewal_price: Option<MoneyInput>,
    #[serde(default)]
    pub trial_days: Option<u32>,
    #[serde(default)]
    pub vendor_company_id: Option<String>,
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub category_id: Option<String>,
    #[serde(default)]
    pub software_id: Option<String>,
    #[serde(default)]
    pub license_template: Option<LicenseTemplate>,
    #[serde(default)]
    pub support_department_id: Option<String>,
    #[serde(default)]
    pub support_department_name: Option<String>,
    #[serde(default)]
    pub bundle_rule: Option<String>,
    #[serde(default)]
    pub bundle_discount_bps: Option<i64>,
    /// When present, must be a valid lifecycle transition.
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TransitionProduct {
    /// Target status: `active`, `deprecated`, or `retired`.
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventorySummary {
    pub total_quantity: i64,
    pub total_reserved: i64,
    pub total_available: i64,
    pub low_stock: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductDetail {
    #[serde(flatten)]
    pub product: Product,
    pub tags: Vec<Tag>,
    pub variant_count: u64,
    pub tier_count: u64,
    pub bundle_item_count: u64,
    /// Base price rendered as an exact decimal string.
    pub price_major: String,
    #[serde(default)]
    pub inventory: Option<InventorySummary>,
}

// ---------------------------------------------------------------------------
// Variants and options
// ---------------------------------------------------------------------------

/// One configurable option of a product, for example `plan` or
/// `size`, with its allowed values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductOption {
    pub id: String,
    pub product_id: String,
    pub name: String,
    pub values: Vec<String>,
    pub required: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProductOption {
    pub name: String,
    pub values: Vec<String>,
    #[serde(default)]
    pub required: Option<bool>,
}

/// One sellable variant: a combination of option values with its
/// own SKU and a price delta (in minor units, may be negative)
/// applied on top of the product base price.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductVariant {
    pub id: String,
    pub product_id: String,
    pub sku: String,
    pub label: String,
    /// Option name -> chosen value, for example `{"size": "XL"}`.
    pub option_values: HashMap<String, String>,
    /// Price delta in minor units relative to the product base
    /// price; negative deltas lower the price.
    #[serde(default)]
    pub price_delta_minor: i64,
    /// `active` or `disabled`.
    #[serde(default = "default_variant_status")]
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_variant_status() -> String {
    "active".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProductVariant {
    pub sku: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub option_values: HashMap<String, String>,
    #[serde(default)]
    pub price_delta_minor: Option<i64>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProductVariant {
    #[serde(default)]
    pub sku: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub option_values: Option<HashMap<String, String>>,
    #[serde(default)]
    pub price_delta_minor: Option<i64>,
    #[serde(default)]
    pub status: Option<String>,
}

// ---------------------------------------------------------------------------
// Bundles
// ---------------------------------------------------------------------------

/// One product contained in a bundle, with its quantity and an
/// optional per-item price override (minor units) that replaces the
/// child product's base price inside this bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleItem {
    pub id: String,
    pub bundle_product_id: String,
    pub child_product_id: String,
    pub quantity: u32,
    #[serde(default)]
    pub price_override_minor: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddBundleItem {
    pub child_product_id: String,
    #[serde(default)]
    pub quantity: Option<u32>,
    #[serde(default)]
    pub price_override_minor: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateBundleRule {
    pub bundle_rule: String,
    #[serde(default)]
    pub bundle_discount_bps: Option<i64>,
}

// ---------------------------------------------------------------------------
// Pricing: tiers, regional prices, usage prices, tax rules
// ---------------------------------------------------------------------------

/// One pricing tier / edition of a product, for example
/// `single-site`, `multi-site`, or `agency`, with its own price,
/// seat count, trial, renewal terms, and feature list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceTier {
    pub id: String,
    pub product_id: String,
    pub slug: String,
    pub label: String,
    pub price: Money,
    #[serde(default)]
    pub renewal_price: Option<Money>,
    #[serde(default)]
    pub billing_period: Option<String>,
    #[serde(default)]
    pub trial_days: u32,
    /// Activation seats included with this tier.
    #[serde(default = "default_seats")]
    pub seats: i32,
    /// When true the tier price is charged per seat (per unit).
    #[serde(default)]
    pub per_seat: bool,
    /// Maximum downloads allowed for the entitlement of this tier.
    #[serde(default = "default_download_limit")]
    pub download_limit: u32,
    pub features: Vec<String>,
    /// `active` or `disabled`.
    #[serde(default = "default_tier_status")]
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_tier_status() -> String {
    "active".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePriceTier {
    pub slug: String,
    #[serde(default)]
    pub label: Option<String>,
    pub price: MoneyInput,
    #[serde(default)]
    pub renewal_price: Option<MoneyInput>,
    #[serde(default)]
    pub billing_period: Option<String>,
    #[serde(default)]
    pub trial_days: Option<u32>,
    #[serde(default)]
    pub seats: Option<i32>,
    #[serde(default)]
    pub per_seat: Option<bool>,
    #[serde(default)]
    pub download_limit: Option<u32>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePriceTier {
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub price: Option<MoneyInput>,
    #[serde(default)]
    pub renewal_price: Option<MoneyInput>,
    #[serde(default)]
    pub billing_period: Option<String>,
    #[serde(default)]
    pub trial_days: Option<u32>,
    #[serde(default)]
    pub seats: Option<i32>,
    #[serde(default)]
    pub per_seat: Option<bool>,
    #[serde(default)]
    pub download_limit: Option<u32>,
    #[serde(default)]
    pub features: Option<Vec<String>>,
    #[serde(default)]
    pub status: Option<String>,
}

/// A regional override: the price of a product in one country
/// (ISO 3166-1 alpha-2 code, or `GLOBAL` as the fallback).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionalPrice {
    pub id: String,
    pub product_id: String,
    /// Two-letter country code, or `GLOBAL`.
    pub region: String,
    pub price: Money,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRegionalPrice {
    pub region: String,
    pub price: MoneyInput,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateRegionalPrice {
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub price: Option<MoneyInput>,
}

/// A usage-based price: a metered metric charged per unit after the
/// included allowance, on top of the product base price.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsagePrice {
    pub id: String,
    pub product_id: String,
    /// Metered metric, for example `api_calls` or `seats_used`.
    pub metric: String,
    /// Price per unit in minor units.
    pub unit_price_minor: i64,
    pub currency: String,
    /// Units included before metering starts.
    #[serde(default)]
    pub included_units: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateUsagePrice {
    pub metric: String,
    pub unit_price: MoneyInput,
    #[serde(default)]
    pub included_units: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateUsagePrice {
    #[serde(default)]
    pub metric: Option<String>,
    #[serde(default)]
    pub unit_price: Option<MoneyInput>,
    #[serde(default)]
    pub included_units: Option<u64>,
}

/// A tax rule: a rate in basis points (100 = 1%) for one country
/// (ISO 3166-1 alpha-2), or `GLOBAL` as the fallback rate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxRule {
    pub id: String,
    pub country: String,
    pub name: String,
    /// Tax rate in basis points (100 = 1%, 10000 = 100%).
    pub rate_bps: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTaxRule {
    pub country: String,
    pub name: String,
    pub rate_bps: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTaxRule {
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub rate_bps: Option<i64>,
}

/// A price quote: the exact breakdown billing needs for an invoice
/// line. All amounts are integer minor units plus exact decimal
/// strings; the tax is exclusive (added on top of the subtotal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceQuote {
    pub product_id: String,
    #[serde(default)]
    pub tier_id: Option<String>,
    #[serde(default)]
    pub variant_id: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    pub currency: String,
    pub quantity: u32,
    pub unit_price_minor: i64,
    pub unit_price: String,
    pub subtotal_minor: i64,
    pub subtotal: String,
    pub tax_rate_bps: i64,
    pub tax_minor: i64,
    pub tax: String,
    pub total_minor: i64,
    pub total: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PriceQuoteRequest {
    #[serde(default)]
    pub tier_id: Option<String>,
    #[serde(default)]
    pub variant_id: Option<String>,
    #[serde(default)]
    pub quantity: Option<u32>,
    #[serde(default)]
    pub region: Option<String>,
    /// Metered usage units for `usage` pricing (defaults to 1).
    #[serde(default)]
    pub usage_units: Option<u64>,
}

// ---------------------------------------------------------------------------
// Inventory
// ---------------------------------------------------------------------------

/// Stock for a physical product (or one of its variants).
/// `available` is always `quantity - reserved`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inventory {
    pub id: String,
    pub product_id: String,
    /// Null for product-level stock, set for variant-level stock.
    #[serde(default)]
    pub variant_id: Option<String>,
    pub quantity: i64,
    pub reserved: i64,
    pub low_stock_threshold: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdjustInventory {
    #[serde(default)]
    pub variant_id: Option<String>,
    /// Change to on-hand stock; may be negative, never below zero.
    #[serde(default)]
    pub quantity_delta: Option<i64>,
    /// Change to reserved stock; may be negative, never below zero
    /// and never above the on-hand quantity.
    #[serde(default)]
    pub reserved_delta: Option<i64>,
    /// New low-stock threshold; applied when present.
    #[serde(default)]
    pub low_stock_threshold: Option<i64>,
    #[serde(default)]
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Product files and versions (references into the Software Manager)
// ---------------------------------------------------------------------------

/// A file attached to a product version. This record only references
/// a software release / file owned by the Software Manager; this
/// module never stores, serves, or verifies file bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductFile {
    pub id: String,
    pub product_id: String,
    /// Referenced software record id.
    #[serde(default)]
    pub software_id: Option<String>,
    /// Referenced software version string.
    #[serde(default)]
    pub software_version: Option<String>,
    /// Referenced software release id.
    #[serde(default)]
    pub release_id: Option<String>,
    /// Referenced file id in the Software Manager / file store.
    #[serde(default)]
    pub file_id: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub checksum_sha256: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProductFile {
    #[serde(default)]
    pub software_id: Option<String>,
    #[serde(default)]
    pub software_version: Option<String>,
    #[serde(default)]
    pub release_id: Option<String>,
    #[serde(default)]
    pub file_id: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub checksum_sha256: Option<String>,
}

/// One released version of a product, referencing a software
/// version / release when the product sells software.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductVersion {
    pub id: String,
    pub product_id: String,
    pub version: String,
    #[serde(default)]
    pub software_id: Option<String>,
    #[serde(default)]
    pub release_id: Option<String>,
    pub changelog: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProductVersion {
    pub version: String,
    #[serde(default)]
    pub software_id: Option<String>,
    #[serde(default)]
    pub release_id: Option<String>,
    #[serde(default)]
    pub changelog: Option<String>,
}

// ---------------------------------------------------------------------------
// Taxonomy (groups and categories) and tags — same shapes as the
// software module, in this module's own namespace.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxonomyNode {
    pub id: String,
    /// `group` or `category`.
    pub kind: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub parent_id: Option<String>,
    pub sort_order: i64,
    /// Depth in the tree; roots are 0.
    pub depth: i64,
    /// Slash path of slugs from the root, for example `/tools/wordpress`.
    pub path: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxonomyTreeNode {
    #[serde(flatten)]
    pub node: TaxonomyNode,
    pub children: Vec<TaxonomyTreeNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTaxonomyNode {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTaxonomyNode {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MoveTaxonomyNode {
    /// New parent id; null (absent) moves the node to the root.
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTag {
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTag {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagCount {
    #[serde(flatten)]
    pub tag: Tag,
    pub product_count: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AttachTag {
    pub tag_id: String,
}

// ---------------------------------------------------------------------------
// Entitlement and support wiring payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct EntitlementCheckRequest {
    pub product_id: String,
    pub user_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateSupportTicket {
    pub user_id: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub priority: Option<String>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct ProductsState {
    hooks: RwLock<Arc<HookBus>>,
    products: DashMap<String, Product>,
    options: DashMap<String, ProductOption>,
    variants: DashMap<String, ProductVariant>,
    bundle_items: DashMap<String, BundleItem>,
    tiers: DashMap<String, PriceTier>,
    regional_prices: DashMap<String, RegionalPrice>,
    usage_prices: DashMap<String, UsagePrice>,
    tax_rules: DashMap<String, TaxRule>,
    inventory: DashMap<String, Inventory>,
    files: DashMap<String, ProductFile>,
    versions: DashMap<String, ProductVersion>,
    taxonomy: DashMap<String, TaxonomyNode>,
    tags: DashMap<String, Tag>,
    /// product id -> assigned tag ids.
    product_tags: DashMap<String, HashSet<String>>,
}

impl ProductsState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            products: DashMap::new(),
            options: DashMap::new(),
            variants: DashMap::new(),
            bundle_items: DashMap::new(),
            tiers: DashMap::new(),
            regional_prices: DashMap::new(),
            usage_prices: DashMap::new(),
            tax_rules: DashMap::new(),
            inventory: DashMap::new(),
            files: DashMap::new(),
            versions: DashMap::new(),
            taxonomy: DashMap::new(),
            tags: DashMap::new(),
            product_tags: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    fn product_detail(&self, product: &Product) -> ProductDetail {
        let tag_ids = self
            .product_tags
            .get(&product.id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        let mut tags: Vec<Tag> = tag_ids
            .iter()
            .filter_map(|id| self.tags.get(id).map(|entry| entry.value().clone()))
            .collect();
        tags.sort_by(|a, b| a.name.cmp(&b.name));
        let variant_count = self
            .variants
            .iter()
            .filter(|entry| entry.value().product_id == product.id)
            .count() as u64;
        let tier_count = self
            .tiers
            .iter()
            .filter(|entry| entry.value().product_id == product.id)
            .count() as u64;
        let bundle_item_count = self
            .bundle_items
            .iter()
            .filter(|entry| entry.value().bundle_product_id == product.id)
            .count() as u64;
        let inventory = if product.kind == "physical" {
            let records: Vec<&Inventory> = self
                .inventory
                .iter()
                .filter(|entry| entry.value().product_id == product.id)
                .map(|entry| entry.value())
                .collect();
            if records.is_empty() {
                None
            } else {
                let total_quantity: i64 = records.iter().map(|record| record.quantity).sum();
                let total_reserved: i64 = records.iter().map(|record| record.reserved).sum();
                let low_stock = records
                    .iter()
                    .any(|record| record.quantity - record.reserved <= record.low_stock_threshold);
                Some(InventorySummary {
                    total_quantity,
                    total_reserved,
                    total_available: total_quantity - total_reserved,
                    low_stock,
                })
            }
        } else {
            None
        };
        ProductDetail {
            price_major: format_minor(product.price.amount_minor, &product.price.currency),
            product: product.clone(),
            tags,
            variant_count,
            tier_count,
            bundle_item_count,
            inventory,
        }
    }

    /// True when the referenced company exists. When no
    /// `company.exists` handler is registered the reference is
    /// accepted unchecked, because this module cannot see the
    /// company store directly. Mirrors the software module.
    fn vendor_exists(&self, company_id: &str) -> bool {
        if !self.bus().has("company.exists") {
            return true;
        }
        let context = self.bus().emit(
            "company.exists",
            json!({ "company_id": company_id }),
        );
        context.results.iter().any(|result| {
            result.get("exists").and_then(|v| v.as_bool()).unwrap_or(false)
        })
    }

    /// Answer the entitlement question for one user and product.
    ///
    /// The real check path is the store's entitlement data, asked
    /// through the `store.entitlement.check` hook with payload
    /// `{"product_id", "user_id"}`. No handler for that event exists
    /// in the store module today (its `register_hooks` only listens
    /// to `software.release.published`, payment, and bank-transfer
    /// events), so until the store (or another module) answers it,
    /// this honestly reports `entitled: false` with reason
    /// `no_entitlement_source` instead of guessing from local data.
    /// A result counts as entitled when it carries `"entitled": true`
    /// or an `entitlements` array containing an `active` entry.
    fn check_entitlement(&self, product_id: &str, user_id: &str) -> Value {
        if !self.products.contains_key(product_id) {
            return json!({
                "entitled": false,
                "product_id": product_id,
                "user_id": user_id,
                "source": "none",
                "reason": "product_not_found",
            });
        }
        if !self.bus().has("store.entitlement.check") {
            return json!({
                "entitled": false,
                "product_id": product_id,
                "user_id": user_id,
                "source": "none",
                "reason": "no_entitlement_source",
            });
        }
        let context = self.bus().emit(
            "store.entitlement.check",
            json!({ "product_id": product_id, "user_id": user_id }),
        );
        let entitled = context.results.iter().any(|result| {
            result.get("entitled").and_then(|v| v.as_bool()).unwrap_or(false)
                || result
                    .get("entitlements")
                    .and_then(|v| v.as_array())
                    .is_some_and(|items| {
                        items.iter().any(|item| {
                            item.get("status").and_then(|v| v.as_str()) == Some("active")
                        })
                    })
        });
        json!({
            "entitled": entitled,
            "product_id": product_id,
            "user_id": user_id,
            "source": "store.entitlement.check",
            "reason": if entitled { "entitled" } else { "no_active_entitlement" },
        })
    }

    /// Compute the exact price quote billing needs for an invoice
    /// line. All arithmetic is integer minor units; tax is exclusive
    /// and rounded half-up from the configured tax rule.
    fn quote_price(
        &self,
        product: &Product,
        request: &PriceQuoteRequest,
    ) -> UnifiedResult<PriceQuote> {
        let quantity = request.quantity.unwrap_or(1).max(1);
        let currency = product.price.currency.clone();
        // A tier, when named, must belong to this product and be active.
        let tier = match &request.tier_id {
            Some(tier_id) => {
                let tier = self
                    .tiers
                    .get(tier_id.as_str())
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))?;
                if tier.product_id != product.id {
                    return Err(UnifiedError::bad_request(
                        "Pricing tier does not belong to this product",
                    ));
                }
                if tier.status != "active" {
                    return Err(UnifiedError::unprocessable("Pricing tier is disabled"));
                }
                if tier.price.currency != currency {
                    return Err(UnifiedError::bad_request(
                        "Pricing tier currency does not match the product currency",
                    ));
                }
                Some(tier)
            }
            None => None,
        };
        // A variant, when named, must belong to this product and be active.
        let variant = match &request.variant_id {
            Some(variant_id) => {
                let variant = self
                    .variants
                    .get(variant_id.as_str())
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| UnifiedError::not_found("Product variant not found"))?;
                if variant.product_id != product.id {
                    return Err(UnifiedError::bad_request(
                        "Product variant does not belong to this product",
                    ));
                }
                if variant.status != "active" {
                    return Err(UnifiedError::unprocessable("Product variant is disabled"));
                }
                Some(variant)
            }
            None => None,
        };
        // Regional price, when a region is named and one exists.
        let regional = request.region.as_ref().and_then(|region| {
            self.regional_prices.iter().find_map(|entry| {
                let price = entry.value();
                if price.product_id == product.id
                    && price.region.eq_ignore_ascii_case(region)
                {
                    Some(price.clone())
                } else {
                    None
                }
            })
        });
        let mut unit_price_minor: i64 = match (&tier, &regional) {
            (Some(tier), _) => tier.price.amount_minor,
            (None, Some(regional)) => regional.price.amount_minor,
            (None, None) => product.price.amount_minor,
        };
        let mut quote_currency = match (&tier, &regional) {
            (Some(tier), _) => tier.price.currency.clone(),
            (None, Some(regional)) => regional.price.currency.clone(),
            (None, None) => currency.clone(),
        };
        // Bundle pricing replaces the unit price with the rule result.
        if product.kind == "bundle" && tier.is_none() {
            unit_price_minor = self.bundle_unit_price(product)?;
            quote_currency = currency.clone();
        }
        // The variant delta applies on top of the unit price.
        if let Some(variant) = &variant {
            unit_price_minor = unit_price_minor
                .checked_add(variant.price_delta_minor)
                .ok_or_else(|| UnifiedError::bad_request("Variant price overflows"))?;
            if unit_price_minor < 0 {
                return Err(UnifiedError::bad_request(
                    "Variant price delta makes the price negative",
                ));
            }
        }
        // Usage pricing: the base price plus metered units above the
        // included allowance, at the first usage price of the product.
        let mut subtotal_minor: i64 = unit_price_minor
            .checked_mul(quantity as i64)
            .ok_or_else(|| UnifiedError::bad_request("Quantity makes the price overflow"))?;
        if product.pricing_model == "usage" {
            let usage = self
                .usage_prices
                .iter()
                .find(|entry| entry.value().product_id == product.id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::unprocessable(
                        "Usage-priced product has no usage price configured",
                    )
                })?;
            if usage.currency != quote_currency {
                return Err(UnifiedError::bad_request(
                    "Usage price currency does not match the product currency",
                ));
            }
            let units = request.usage_units.unwrap_or(1);
            let billable = units.saturating_sub(usage.included_units);
            let usage_total = (usage.unit_price_minor as i128 * billable as i128)
                .clamp(i64::MIN as i128, i64::MAX as i128) as i64;
            subtotal_minor = subtotal_minor
                .checked_add(usage_total)
                .ok_or_else(|| UnifiedError::bad_request("Usage price overflows"))?;
        }
        let tax_rate_bps = self.tax_rate_bps_for(request.region.as_deref());
        let tax_minor = mul_div_round_half_up(subtotal_minor, tax_rate_bps, 10_000);
        let total_minor = subtotal_minor
            .checked_add(tax_minor)
            .ok_or_else(|| UnifiedError::bad_request("Tax makes the price overflow"))?;
        Ok(PriceQuote {
            product_id: product.id.clone(),
            tier_id: request.tier_id.clone(),
            variant_id: request.variant_id.clone(),
            region: request.region.clone(),
            currency: quote_currency.clone(),
            quantity,
            unit_price_minor,
            unit_price: format_minor(unit_price_minor, &quote_currency),
            subtotal_minor,
            subtotal: format_minor(subtotal_minor, &quote_currency),
            tax_rate_bps,
            tax_minor,
            tax: format_minor(tax_minor, &quote_currency),
            total_minor,
            total: format_minor(total_minor, &quote_currency),
        })
    }

    /// Unit price of a bundle under its pricing rule, from the
    /// children's base prices (or per-item overrides).
    fn bundle_unit_price(&self, product: &Product) -> UnifiedResult<i64> {
        let items: Vec<BundleItem> = self
            .bundle_items
            .iter()
            .filter(|entry| entry.value().bundle_product_id == product.id)
            .map(|entry| entry.value().clone())
            .collect();
        if items.is_empty() {
            return Err(UnifiedError::unprocessable(
                "Bundle product has no bundle items",
            ));
        }
        let mut sum: i64 = 0;
        for item in &items {
            let child = self
                .products
                .get(&item.child_product_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::internal("Bundle item references a missing product")
                })?;
            if child.price.currency != product.price.currency {
                return Err(UnifiedError::bad_request(
                    "Bundle child currency does not match the bundle currency",
                ));
            }
            let unit = item
                .price_override_minor
                .unwrap_or(child.price.amount_minor);
            let line = unit
                .checked_mul(item.quantity as i64)
                .ok_or_else(|| UnifiedError::bad_request("Bundle price overflows"))?;
            sum = sum
                .checked_add(line)
                .ok_or_else(|| UnifiedError::bad_request("Bundle price overflows"))?;
        }
        match product.bundle_rule.as_str() {
            "sum" => Ok(sum),
            "fixed" => Ok(product.price.amount_minor),
            "discount_percent" => {
                let discount_bps = product.bundle_discount_bps.unwrap_or(0);
                Ok(sum - mul_div_round_half_up(sum, discount_bps, 10_000))
            }
            _ => Err(UnifiedError::internal("Bundle product has an invalid pricing rule")),
        }
    }

    /// Tax rate in basis points for a region: the exact country rule
    /// first, then the `GLOBAL` fallback, then zero.
    fn tax_rate_bps_for(&self, region: Option<&str>) -> i64 {
        if let Some(region) = region {
            if let Some(rule) = self.tax_rules.iter().find(|entry| {
                entry.value().country.eq_ignore_ascii_case(region)
            }) {
                return rule.value().rate_bps;
            }
        }
        self.tax_rules
            .iter()
            .find(|entry| entry.value().country == "GLOBAL")
            .map(|entry| entry.value().rate_bps)
            .unwrap_or(0)
    }

    /// True when adding `child_id` to `bundle_id` would create a
    /// bundle cycle (the child already contains the bundle,
    /// directly or transitively).
    fn bundle_would_cycle(&self, bundle_id: &str, child_id: &str) -> bool {
        if bundle_id == child_id {
            return true;
        }
        let mut stack: Vec<String> = vec![child_id.to_string()];
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(current) = stack.pop() {
            if current == bundle_id {
                return true;
            }
            if !seen.insert(current.clone()) {
                continue;
            }
            for entry in self.bundle_items.iter() {
                if entry.value().bundle_product_id == current {
                    stack.push(entry.value().child_product_id.clone());
                }
            }
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Helpers
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

fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

fn validate_kind(kind: &str) -> UnifiedResult<()> {
    if PRODUCT_KINDS.contains(&kind) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "Kind must be one of: {}",
            PRODUCT_KINDS.join(", ")
        )))
    }
}

fn validate_pricing_model(model: &str) -> UnifiedResult<()> {
    if PRICING_MODELS.contains(&model) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "Pricing model must be one of: {}",
            PRICING_MODELS.join(", ")
        )))
    }
}

fn validate_billing_period(period: &str) -> UnifiedResult<()> {
    if BILLING_PERIODS.contains(&period) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "Billing period must be one of: {}",
            BILLING_PERIODS.join(", ")
        )))
    }
}

fn validate_bundle_rule(rule: &str) -> UnifiedResult<()> {
    if BUNDLE_RULES.contains(&rule) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "Bundle rule must be one of: {}",
            BUNDLE_RULES.join(", ")
        )))
    }
}

fn validate_bundle_discount_bps(value: i64) -> UnifiedResult<()> {
    if (0..=10_000).contains(&value) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(
            "Bundle discount must be between 0 and 10000 basis points",
        ))
    }
}

fn validate_tax_rate_bps(value: i64) -> UnifiedResult<()> {
    if (0..=10_000).contains(&value) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(
            "Tax rate must be between 0 and 10000 basis points",
        ))
    }
}

fn validate_region(region: &str) -> UnifiedResult<()> {
    if region == "GLOBAL"
        || (region.len() == 2 && region.chars().all(|c| c.is_ascii_alphabetic()))
    {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(
            "Region must be a 2-letter country code or GLOBAL",
        ))
    }
}

fn validate_license_template(template: &LicenseTemplate) -> UnifiedResult<()> {
    if template.seats < 1 {
        return Err(UnifiedError::bad_request("License seats must be at least 1"));
    }
    if template.download_limit < 1 {
        return Err(UnifiedError::bad_request(
            "License download limit must be at least 1",
        ));
    }
    if let Some(days) = template.duration_days {
        if days < 1 {
            return Err(UnifiedError::bad_request(
                "License duration must be at least 1 day",
            ));
        }
    }
    Ok(())
}

/// Validated lifecycle transitions. `retired` is terminal; every
/// other move must be listed here.
fn validate_status_transition(from: &str, to: &str) -> UnifiedResult<()> {
    if from == to {
        return Err(UnifiedError::conflict(format!(
            "Product is already {from}"
        )));
    }
    let allowed = match from {
        "draft" => ["active", "retired"].as_slice(),
        "active" => ["deprecated", "retired"].as_slice(),
        "deprecated" => ["active", "retired"].as_slice(),
        "retired" => &[],
        _ => &[],
    };
    if allowed.contains(&to) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "Cannot move product from {from} to {to}"
        )))
    }
}

fn validate_hex_checksum(value: &str, expected_len: usize, field: &str) -> UnifiedResult<()> {
    let valid =
        value.len() == expected_len && value.chars().all(|ch| ch.is_ascii_hexdigit());
    if valid {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "{field} must be {expected_len} lowercase hex characters"
        )))
    }
}

fn taxonomy_path(state: &ProductsState, parent_id: Option<&str>, slug: &str) -> (i64, String) {
    match parent_id.and_then(|id| state.taxonomy.get(id)) {
        Some(parent) => (parent.depth + 1, format!("{}/{}", parent.path, slug)),
        None => (0, format!("/{slug}")),
    }
}

/// Recompute depth and path for a node and every descendant after a move.
fn recompute_subtree(state: &ProductsState, root_id: &str) {
    let mut queue: Vec<String> = vec![root_id.to_string()];
    while let Some(id) = queue.pop() {
        let (parent_id, slug) = match state.taxonomy.get(&id) {
            Some(node) => (node.parent_id.clone(), node.slug.clone()),
            None => continue,
        };
        let (depth, path) = taxonomy_path(state, parent_id.as_deref(), &slug);
        if let Some(mut node) = state.taxonomy.get_mut(&id) {
            node.depth = depth;
            node.path = path;
            node.updated_at = now();
        }
        let children: Vec<String> = state
            .taxonomy
            .iter()
            .filter(|entry| entry.value().parent_id.as_deref() == Some(id.as_str()))
            .map(|entry| entry.key().clone())
            .collect();
        queue.extend(children);
    }
}

fn build_tree(
    state: &ProductsState,
    parent_id: Option<&str>,
    kind: Option<&str>,
) -> Vec<TaxonomyTreeNode> {
    let mut nodes: Vec<TaxonomyNode> = state
        .taxonomy
        .iter()
        .filter(|entry| {
            entry.value().parent_id.as_deref() == parent_id
                && kind.is_none_or(|kind| entry.value().kind == kind)
        })
        .map(|entry| entry.value().clone())
        .collect();
    nodes.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.cmp(&b.name))
    });
    nodes
        .into_iter()
        .map(|node| TaxonomyTreeNode {
            children: build_tree(state, Some(node.id.as_str()), kind),
            node,
        })
        .collect()
}

fn is_descendant_of(state: &ProductsState, node_id: &str, ancestor_id: &str) -> bool {
    let mut current = state
        .taxonomy
        .get(node_id)
        .and_then(|node| node.parent_id.clone());
    while let Some(id) = current {
        if id == ancestor_id {
            return true;
        }
        current = state
            .taxonomy
            .get(&id)
            .and_then(|node| node.parent_id.clone());
    }
    false
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

fn validate_product_refs(
    state: &ProductsState,
    vendor_company_id: Option<&str>,
    group_id: Option<&str>,
    category_id: Option<&str>,
) -> UnifiedResult<()> {
    if let Some(vendor) = vendor_company_id {
        if vendor.trim().is_empty() {
            return Err(UnifiedError::bad_request("Vendor company id must not be empty"));
        }
        if vendor.parse::<Uuid>().is_err() {
            return Err(UnifiedError::bad_request(
                "Vendor company id must be a UUID",
            ));
        }
        if !state.vendor_exists(vendor) {
            return Err(UnifiedError::not_found("Vendor company not found"));
        }
    }
    if let Some(group_id) = group_id {
        match state.taxonomy.get(group_id) {
            Some(node) if node.kind == "group" => {}
            _ => return Err(UnifiedError::not_found("Group not found")),
        }
    }
    if let Some(category_id) = category_id {
        match state.taxonomy.get(category_id) {
            Some(node) if node.kind == "category" => {}
            _ => return Err(UnifiedError::not_found("Category not found")),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Product handlers
// ---------------------------------------------------------------------------

async fn list_products(
    state: web::Data<Arc<ProductsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let sort = params.allowed_sort(&["name", "slug", "status", "created_at"], "created_at");
    let tag_filter = params.filters.get("tag").cloned();
    let mut items: Vec<Product> = state
        .products
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|product| {
            matches_search(
                &format!("{} {} {}", product.name, product.slug, product.description),
                &params,
            ) && params.filters.get("kind").is_none_or(|kind| {
                product.kind == *kind
            }) && params.filters.get("status").is_none_or(|status| {
                product.status == *status
            }) && params.filters.get("pricing_model").is_none_or(|model| {
                product.pricing_model == *model
            }) && params.filters.get("vendor_company_id").is_none_or(|vendor| {
                product.vendor_company_id.as_deref() == Some(vendor.as_str())
            }) && params.filters.get("group_id").is_none_or(|group_id| {
                product.group_id.as_deref() == Some(group_id.as_str())
            }) && params.filters.get("category_id").is_none_or(|category_id| {
                product.category_id.as_deref() == Some(category_id.as_str())
            }) && params.filters.get("currency").is_none_or(|currency| {
                product.price.currency.eq_ignore_ascii_case(currency)
            }) && params.filters.get("listed").is_none_or(|flag| {
                product.listed == (*flag == "true" || *flag == "1")
            }) && tag_filter.as_ref().is_none_or(|tag_id| {
                state
                    .product_tags
                    .get(&product.id)
                    .is_some_and(|tags| tags.contains(tag_id.as_str()))
            })
        })
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "slug" => items.sort_by(|a, b| a.slug.cmp(&b.slug)),
        "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_product(
    state: web::Data<Arc<ProductsState>>,
    body: web::Json<CreateProduct>,
) -> HttpResponse {
    let result: UnifiedResult<ProductDetail> = (|| {
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Name must not be empty"));
        }
        validate_kind(&input.kind)?;
        validate_pricing_model(&input.pricing_model)?;
        if let Some(period) = &input.billing_period {
            validate_billing_period(period)?;
        }
        if input.pricing_model == "subscription" && input.billing_period.is_none() {
            return Err(UnifiedError::bad_request(
                "Subscription products require a billing period",
            ));
        }
        if input.kind == "bundle" {
            if let Some(rule) = &input.bundle_rule {
                validate_bundle_rule(rule)?;
            }
            if let Some(discount) = input.bundle_discount_bps {
                validate_bundle_discount_bps(discount)?;
            }
        } else if input.bundle_rule.is_some() || input.bundle_discount_bps.is_some() {
            return Err(UnifiedError::bad_request(
                "Bundle pricing rules only apply to bundle products",
            ));
        }
        let price = resolve_money(&input.price, None)?;
        let renewal_price = match &input.renewal_price {
            Some(renewal) => Some(resolve_money(renewal, Some(&price.currency))?),
            None => None,
        };
        if let Some(renewal) = &renewal_price {
            if renewal.currency != price.currency {
                return Err(UnifiedError::bad_request(
                    "Renewal price currency must match the base price currency",
                ));
            }
        }
        if let Some(template) = &input.license_template {
            validate_license_template(template)?;
        }
        let slug = match &input.slug {
            Some(slug) if !slug.trim().is_empty() => slugify(slug),
            _ => slugify(&input.name),
        };
        if slug.is_empty() {
            return Err(UnifiedError::bad_request(
                "Could not derive a slug from the product name",
            ));
        }
        if state
            .products
            .iter()
            .any(|entry| entry.value().slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "Product with slug \"{slug}\" already exists"
            )));
        }
        validate_product_refs(
            &state,
            input.vendor_company_id.as_deref(),
            input.group_id.as_deref(),
            input.category_id.as_deref(),
        )?;
        for tag_id in &input.tag_ids {
            if !state.tags.contains_key(tag_id.as_str()) {
                return Err(UnifiedError::not_found(format!(
                    "Tag \"{tag_id}\" not found"
                )));
            }
        }
        if input.trial_days.unwrap_or(0) > 3650 {
            return Err(UnifiedError::bad_request(
                "Trial days must be at most 3650",
            ));
        }
        let product = Product {
            id: new_id(),
            slug,
            name: input.name.trim().to_string(),
            kind: input.kind,
            description: input.description.unwrap_or_default(),
            status: "draft".to_string(),
            pricing_model: input.pricing_model,
            billing_period: input.billing_period,
            price,
            renewal_price,
            trial_days: input.trial_days.unwrap_or(0),
            vendor_company_id: input.vendor_company_id,
            group_id: input.group_id,
            category_id: input.category_id,
            software_id: input.software_id,
            license_template: input.license_template.unwrap_or_default(),
            support_department_id: input.support_department_id,
            support_department_name: input.support_department_name,
            bundle_rule: input.bundle_rule.unwrap_or_else(default_bundle_rule),
            bundle_discount_bps: input.bundle_discount_bps,
            listed: false,
            created_at: now(),
            updated_at: now(),
        };
        state.products.insert(product.id.clone(), product.clone());
        if !input.tag_ids.is_empty() {
            state.product_tags.insert(
                product.id.clone(),
                input.tag_ids.iter().cloned().collect(),
            );
        }
        let detail = state.product_detail(&product);
        state.emit(
            "product.created",
            serde_json::to_value(&detail).unwrap_or(Value::Null),
        );
        Ok(detail)
    })();
    respond_created(result)
}

async fn get_product(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ProductDetail> = state
        .products
        .get(path.as_str())
        .map(|entry| state.product_detail(entry.value()))
        .ok_or_else(|| UnifiedError::not_found("Product not found"));
    json::respond(result, false)
}

async fn update_product(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateProduct>,
) -> HttpResponse {
    let result: UnifiedResult<ProductDetail> = (|| {
        let input = body.into_inner();
        // Slug uniqueness is checked before taking the write lock:
        // iterating the map while holding a RefMut on one of its
        // entries would deadlock on the shard lock.
        let new_slug = input.slug.as_ref().map(|slug| slugify(slug));
        if let Some(slug) = &new_slug {
            if slug.is_empty() {
                return Err(UnifiedError::bad_request("Slug must not be empty"));
            }
            if state
                .products
                .iter()
                .any(|entry| entry.value().slug == *slug && entry.key() != path.as_str())
            {
                return Err(UnifiedError::conflict(format!(
                    "Product with slug \"{slug}\" already exists"
                )));
            }
        }
        // Resolve money inputs before touching the stored product so
        // a rejected update never leaves a half-mutated record.
        let new_price = match &input.price {
            Some(price) => Some(resolve_money(price, None)?),
            None => None,
        };
        let new_renewal_price = match &input.renewal_price {
            Some(renewal) => Some(resolve_money(renewal, None)?),
            None => None,
        };
        if let Some(kind) = &input.kind {
            validate_kind(kind)?;
        }
        if let Some(model) = &input.pricing_model {
            validate_pricing_model(model)?;
        }
        if let Some(period) = &input.billing_period {
            validate_billing_period(period)?;
        }
        if let Some(template) = &input.license_template {
            validate_license_template(template)?;
        }
        // Every remaining fallible check runs here, against a
        // snapshot of the stored product and before the write lock is
        // taken, so a rejected update never leaves a half-mutated
        // record behind.
        let existing = state
            .products
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if let Some(kind) = &input.kind {
            if *kind != "bundle" && existing.kind == "bundle" {
                // A bundle keeps its bundle items; changing a bundle
                // into another kind is refused while it still has
                // items, so pricing can never dangle.
                let has_items = state
                    .bundle_items
                    .iter()
                    .any(|entry| entry.value().bundle_product_id == existing.id);
                if has_items {
                    return Err(UnifiedError::conflict(
                        "Product has bundle items; remove them before changing its kind",
                    ));
                }
            }
        }
        let effective_model = input
            .pricing_model
            .as_deref()
            .unwrap_or(existing.pricing_model.as_str());
        let effective_period = input
            .billing_period
            .as_deref()
            .or(existing.billing_period.as_deref());
        if effective_model == "subscription" && effective_period.is_none() {
            return Err(UnifiedError::bad_request(
                "Subscription products require a billing period",
            ));
        }
        let effective_currency = new_price
            .as_ref()
            .map(|price| price.currency.clone())
            .unwrap_or_else(|| existing.price.currency.clone());
        if let Some(renewal) = &new_renewal_price {
            if renewal.currency != effective_currency {
                return Err(UnifiedError::bad_request(
                    "Renewal price currency must match the base price currency",
                ));
            }
        }
        if let Some(trial_days) = input.trial_days {
            if trial_days > 3650 {
                return Err(UnifiedError::bad_request(
                    "Trial days must be at most 3650",
                ));
            }
        }
        let effective_kind = input.kind.as_deref().unwrap_or(existing.kind.as_str());
        if effective_kind != "bundle"
            && (input.bundle_rule.is_some() || input.bundle_discount_bps.is_some())
        {
            return Err(UnifiedError::bad_request(
                "Bundle pricing rules only apply to bundle products",
            ));
        }
        if effective_kind == "bundle" {
            if let Some(rule) = &input.bundle_rule {
                validate_bundle_rule(rule)?;
            }
            if let Some(discount) = input.bundle_discount_bps {
                validate_bundle_discount_bps(discount)?;
            }
        }
        if let Some(status) = &input.status {
            if !PRODUCT_STATUSES.contains(&status.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "Status must be one of: {}",
                    PRODUCT_STATUSES.join(", ")
                )));
            }
            validate_status_transition(&existing.status, status)?;
        }
        validate_product_refs(
            &state,
            input.vendor_company_id.as_deref(),
            input.group_id.as_deref(),
            input.category_id.as_deref(),
        )?;
        let mut product = state
            .products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Name must not be empty"));
            }
            product.name = name.trim().to_string();
        }
        if let Some(slug) = new_slug {
            product.slug = slug;
        }
        if let Some(kind) = input.kind {
            product.kind = kind;
        }
        if let Some(description) = input.description {
            product.description = description;
        }
        if let Some(model) = input.pricing_model {
            product.pricing_model = model;
        }
        if let Some(period) = input.billing_period {
            product.billing_period = Some(period);
        }
        let old_price_minor = product.price.amount_minor;
        let old_currency = product.price.currency.clone();
        if let Some(price) = new_price {
            product.price = price;
        }
        if let Some(renewal) = new_renewal_price {
            product.renewal_price = Some(renewal);
        }
        if let Some(trial_days) = input.trial_days {
            product.trial_days = trial_days;
        }
        if let Some(vendor) = input.vendor_company_id {
            product.vendor_company_id = Some(vendor);
        }
        if let Some(group_id) = input.group_id {
            product.group_id = Some(group_id);
        }
        if let Some(category_id) = input.category_id {
            product.category_id = Some(category_id);
        }
        if let Some(software_id) = input.software_id {
            product.software_id = Some(software_id);
        }
        if let Some(template) = input.license_template {
            product.license_template = template;
        }
        if let Some(department_id) = input.support_department_id {
            product.support_department_id = Some(department_id);
        }
        if let Some(department_name) = input.support_department_name {
            product.support_department_name = Some(department_name);
        }
        if product.kind == "bundle" {
            if let Some(rule) = input.bundle_rule {
                product.bundle_rule = rule;
            }
            if let Some(discount) = input.bundle_discount_bps {
                product.bundle_discount_bps = Some(discount);
            }
            if product.bundle_rule != "discount_percent" {
                product.bundle_discount_bps = None;
            }
        }
        let old_status = product.status.clone();
        let mut status_changed_to: Option<String> = None;
        if let Some(status) = input.status {
            validate_status_transition(&product.status, &status)?;
            product.status = status.clone();
            status_changed_to = Some(status);
        }
        // Leaving `active` unlists the product in the store.
        let mut delisted = false;
        if status_changed_to
            .as_deref()
            .is_some_and(|to| to != "active")
            && product.listed
        {
            product.listed = false;
            delisted = true;
        }
        let price_changed = product.price.amount_minor != old_price_minor
            || product.price.currency != old_currency;
        product.updated_at = now();
        let detail = state.product_detail(&product.clone());
        let detail_value = serde_json::to_value(&detail).unwrap_or(Value::Null);
        drop(product);
        state.emit("product.updated", detail_value.clone());
        if let Some(to) = status_changed_to {
            state.emit(
                "product.status_changed",
                json!({
                    "product_id": detail.product.id,
                    "from": old_status,
                    "to": to,
                }),
            );
        }
        if price_changed {
            state.emit("product.price_changed", detail_value.clone());
        }
        if delisted {
            state.emit("product.delisted", detail_value);
        }
        Ok(detail)
    })();
    json::respond(result, false)
}

async fn delete_product(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let id = path.as_str().to_string();
        let blocked = |what: &str| {
            UnifiedError::conflict(format!(
                "Product has {what}; delete them first"
            ))
        };
        if state
            .variants
            .iter()
            .any(|entry| entry.value().product_id == id)
        {
            return Err(blocked("variants"));
        }
        if state
            .options
            .iter()
            .any(|entry| entry.value().product_id == id)
        {
            return Err(blocked("options"));
        }
        if state
            .tiers
            .iter()
            .any(|entry| entry.value().product_id == id)
        {
            return Err(blocked("pricing tiers"));
        }
        if state
            .bundle_items
            .iter()
            .any(|entry| entry.value().bundle_product_id == id)
        {
            return Err(blocked("bundle items"));
        }
        if state
            .bundle_items
            .iter()
            .any(|entry| entry.value().child_product_id == id)
        {
            return Err(UnifiedError::conflict(
                "Product is part of another bundle; remove it from the bundle first",
            ));
        }
        if state
            .files
            .iter()
            .any(|entry| entry.value().product_id == id)
        {
            return Err(blocked("files"));
        }
        if state
            .versions
            .iter()
            .any(|entry| entry.value().product_id == id)
        {
            return Err(blocked("versions"));
        }
        if state
            .inventory
            .iter()
            .any(|entry| entry.value().product_id == id)
        {
            return Err(blocked("inventory records"));
        }
        let removed = state
            .products
            .remove(&id)
            .map(|(_, product)| product);
        match removed {
            Some(product) => {
                state.product_tags.remove(&id);
                let value = serde_json::to_value(&product).unwrap_or(Value::Null);
                state.emit("product.deleted", value);
                if product.listed {
                    state.emit("product.delisted", serde_json::to_value(&product).unwrap_or(Value::Null));
                }
                Ok(json!({ "deleted": true, "id": id }))
            }
            None => Err(UnifiedError::not_found("Product not found")),
        }
    })();
    json::respond(result, false)
}

async fn transition_product(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<TransitionProduct>,
) -> HttpResponse {
    let result: UnifiedResult<ProductDetail> = (|| {
        let target = body.into_inner().to;
        if !PRODUCT_STATUSES.contains(&target.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Status must be one of: {}",
                PRODUCT_STATUSES.join(", ")
            )));
        }
        let mut product = state
            .products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        validate_status_transition(&product.status, &target)?;
        let from = product.status.clone();
        product.status = target.clone();
        let mut delisted = false;
        if target != "active" && product.listed {
            product.listed = false;
            delisted = true;
        }
        product.updated_at = now();
        let detail = state.product_detail(&product.clone());
        let detail_value = serde_json::to_value(&detail).unwrap_or(Value::Null);
        drop(product);
        state.emit(
            "product.status_changed",
            json!({
                "product_id": detail.product.id,
                "from": from,
                "to": target,
            }),
        );
        if delisted {
            state.emit("product.delisted", detail_value);
        }
        Ok(detail)
    })();
    json::respond(result, false)
}

/// List a product in the store: only `active` products can be
/// listed. The store consumes the `product.listed` event.
async fn list_product(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ProductDetail> = (|| {
        let mut product = state
            .products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if product.status != "active" {
            return Err(UnifiedError::unprocessable(
                "Only active products can be listed in the store",
            ));
        }
        if product.listed {
            return Err(UnifiedError::conflict("Product is already listed"));
        }
        product.listed = true;
        product.updated_at = now();
        let detail = state.product_detail(&product.clone());
        let detail_value = serde_json::to_value(&detail).unwrap_or(Value::Null);
        drop(product);
        state.emit("product.listed", detail_value);
        Ok(detail)
    })();
    json::respond(result, false)
}

/// Remove a product from the store. The store consumes the
/// `product.delisted` event.
async fn delist_product(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ProductDetail> = (|| {
        let mut product = state
            .products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if !product.listed {
            return Err(UnifiedError::conflict("Product is not listed"));
        }
        product.listed = false;
        product.updated_at = now();
        let detail = state.product_detail(&product.clone());
        let detail_value = serde_json::to_value(&detail).unwrap_or(Value::Null);
        drop(product);
        state.emit("product.delisted", detail_value);
        Ok(detail)
    })();
    json::respond(result, false)
}

async fn price_quote(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<PriceQuoteRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PriceQuote> = (|| {
        let product = state
            .products
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if let Some(region) = &body.region {
            validate_region(&region.to_uppercase())?;
        }
        state.quote_price(&product, &body.into_inner())
    })();
    json::respond(result, false)
}

async fn get_license_template(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = state
        .products
        .get(path.as_str())
        .map(|entry| {
            let product = entry.value();
            json!({
                "product_id": product.id,
                "licensed": product.license_template.requires_license,
                "license_template": product.license_template,
                "seats": product.license_template.seats,
                "download_limit": product.license_template.download_limit,
            })
        })
        .ok_or_else(|| UnifiedError::not_found("Product not found"));
    json::respond(result, false)
}

/// Open a support ticket for a product. The ticket is created by the
/// tickets module through the `ticket.create_requested` hook, routed
/// to this product's support department.
async fn create_support_ticket(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<CreateSupportTicket>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request("Title must not be empty"));
        }
        if input.body.trim().is_empty() {
            return Err(UnifiedError::bad_request("Body must not be empty"));
        }
        let product = state
            .products
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if !state.bus().has("ticket.create_requested") {
            return Err(UnifiedError::unprocessable(
                "No ticket handler is registered",
            ));
        }
        let context = state.bus().emit(
            "ticket.create_requested",
            json!({
                "title": input.title,
                "body": input.body,
                "user_id": input.user_id,
                "company_id": product.vendor_company_id,
                "priority": input.priority,
                "category": product.slug,
                "product_id": product.id,
                "department_id": product.support_department_id,
                "department_name": product.support_department_name,
            }),
        );
        let ticket = context.results.into_iter().find(|result| {
            result.get("error").is_none()
                && (result.get("id").is_some() || result.get("ticket").is_some())
        });
        match ticket {
            Some(ticket) => Ok(json!({
                "ticket": ticket,
                "product_id": product.id,
                "department_id": product.support_department_id,
                "department_name": product.support_department_name,
            })),
            None => Err(UnifiedError::unprocessable(
                "Ticket creation was rejected by the tickets module",
            )),
        }
    })();
    json::respond(result, false)
}

/// HTTP view of the `product.entitlement.check` hook: does this user
/// hold an active entitlement for this product?
async fn entitlement_check(
    state: web::Data<Arc<ProductsState>>,
    body: web::Json<EntitlementCheckRequest>,
) -> HttpResponse {
    let input = body.into_inner();
    let result = state.check_entitlement(&input.product_id, &input.user_id);
    json::success_response(result, 200)
}

// ---------------------------------------------------------------------------
// Option handlers
// ---------------------------------------------------------------------------

async fn list_options(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    if !state.products.contains_key(path.as_str()) {
        return json::respond(
            Err::<Value, UnifiedError>(UnifiedError::not_found("Product not found")),
            false,
        );
    }
    let mut items: Vec<ProductOption> = state
        .options
        .iter()
        .filter(|entry| entry.value().product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .filter(|option| matches_search(&option.name, &params))
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_option(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<CreateProductOption>,
) -> HttpResponse {
    let result: UnifiedResult<ProductOption> = (|| {
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Option name must not be empty"));
        }
        if input.values.is_empty()
            || input.values.iter().any(|value| value.trim().is_empty())
        {
            return Err(UnifiedError::bad_request(
                "Option values must not be empty",
            ));
        }
        if !state.products.contains_key(path.as_str()) {
            return Err(UnifiedError::not_found("Product not found"));
        }
        if state.options.iter().any(|entry| {
            entry.value().product_id == path.as_str()
                && entry.value().name.eq_ignore_ascii_case(input.name.trim())
        }) {
            return Err(UnifiedError::conflict(format!(
                "Product already has an option named \"{}\"",
                input.name.trim()
            )));
        }
        let option = ProductOption {
            id: new_id(),
            product_id: path.into_inner(),
            name: input.name.trim().to_string(),
            values: input.values.iter().map(|value| value.trim().to_string()).collect(),
            required: input.required.unwrap_or(false),
            created_at: now(),
            updated_at: now(),
        };
        state.options.insert(option.id.clone(), option.clone());
        Ok(option)
    })();
    respond_created(result)
}

async fn delete_option(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let option = state
            .options
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Option not found"))?;
        let in_use = state.variants.iter().any(|entry| {
            entry.value().product_id == option.product_id
                && entry.value().option_values.contains_key(&option.name)
        });
        if in_use {
            return Err(UnifiedError::conflict(
                "Option is used by variants; delete the variants first",
            ));
        }
        state.options.remove(path.as_str());
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Variant handlers
// ---------------------------------------------------------------------------

async fn list_variants(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    if !state.products.contains_key(path.as_str()) {
        return json::respond(
            Err::<Value, UnifiedError>(UnifiedError::not_found("Product not found")),
            false,
        );
    }
    let mut items: Vec<ProductVariant> = state
        .variants
        .iter()
        .filter(|entry| entry.value().product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .filter(|variant| {
            matches_search(&format!("{} {}", variant.sku, variant.label), &params)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| variant.status == *status)
        })
        .collect();
    items.sort_by(|a, b| a.sku.cmp(&b.sku));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

fn validate_variant_options(
    state: &ProductsState,
    product_id: &str,
    option_values: &HashMap<String, String>,
) -> UnifiedResult<()> {
    // Every named option must exist on the product and the chosen
    // value must be one of its allowed values. Required options
    // must all be present.
    let options: Vec<ProductOption> = state
        .options
        .iter()
        .filter(|entry| entry.value().product_id == product_id)
        .map(|entry| entry.value().clone())
        .collect();
    for (name, value) in option_values {
        match options.iter().find(|option| option.name == *name) {
            Some(option) => {
                if !option.values.iter().any(|allowed| allowed == value) {
                    return Err(UnifiedError::bad_request(format!(
                        "Value \"{value}\" is not allowed for option \"{name}\""
                    )));
                }
            }
            None => {
                return Err(UnifiedError::bad_request(format!(
                    "Product has no option named \"{name}\""
                )));
            }
        }
    }
    for option in &options {
        if option.required && !option_values.contains_key(&option.name) {
            return Err(UnifiedError::bad_request(format!(
                "Option \"{}\" is required",
                option.name
            )));
        }
    }
    Ok(())
}

async fn create_variant(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<CreateProductVariant>,
) -> HttpResponse {
    let result: UnifiedResult<ProductVariant> = (|| {
        let input = body.into_inner();
        let product_id = path.into_inner();
        if input.sku.trim().is_empty() {
            return Err(UnifiedError::bad_request("SKU must not be empty"));
        }
        if !state.products.contains_key(&product_id) {
            return Err(UnifiedError::not_found("Product not found"));
        }
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "disabled") {
                return Err(UnifiedError::bad_request(
                    "Variant status must be active or disabled",
                ));
            }
        }
        validate_variant_options(&state, &product_id, &input.option_values)?;
        if state
            .variants
            .iter()
            .any(|entry| entry.value().sku == input.sku.trim())
        {
            return Err(UnifiedError::conflict(format!(
                "Variant with SKU \"{}\" already exists",
                input.sku.trim()
            )));
        }
        let variant = ProductVariant {
            id: new_id(),
            product_id,
            sku: input.sku.trim().to_string(),
            label: input
                .label
                .unwrap_or_else(|| input.sku.trim().to_string()),
            option_values: input.option_values,
            price_delta_minor: input.price_delta_minor.unwrap_or(0),
            status: input.status.unwrap_or_else(default_variant_status),
            created_at: now(),
            updated_at: now(),
        };
        state.variants.insert(variant.id.clone(), variant.clone());
        Ok(variant)
    })();
    respond_created(result)
}

async fn get_variant(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ProductVariant> = state
        .variants
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Variant not found"));
    json::respond(result, false)
}

async fn update_variant(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateProductVariant>,
) -> HttpResponse {
    let result: UnifiedResult<ProductVariant> = (|| {
        let input = body.into_inner();
        let new_sku = input.sku.as_ref().map(|sku| sku.trim().to_string());
        if let Some(sku) = &new_sku {
            if sku.is_empty() {
                return Err(UnifiedError::bad_request("SKU must not be empty"));
            }
            if state
                .variants
                .iter()
                .any(|entry| entry.value().sku == *sku && entry.key() != path.as_str())
            {
                return Err(UnifiedError::conflict(format!(
                    "Variant with SKU \"{sku}\" already exists"
                )));
            }
        }
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "disabled") {
                return Err(UnifiedError::bad_request(
                    "Variant status must be active or disabled",
                ));
            }
        }
        let mut variant = state
            .variants
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Variant not found"))?;
        if let Some(option_values) = &input.option_values {
            validate_variant_options(&state, &variant.product_id, option_values)?;
            variant.option_values = option_values.clone();
        }
        if let Some(sku) = new_sku {
            variant.sku = sku;
        }
        if let Some(label) = input.label {
            variant.label = label;
        }
        if let Some(delta) = input.price_delta_minor {
            variant.price_delta_minor = delta;
        }
        if let Some(status) = input.status {
            variant.status = status;
        }
        variant.updated_at = now();
        Ok(variant.clone())
    })();
    json::respond(result, false)
}

async fn delete_variant(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        if state
            .inventory
            .iter()
            .any(|entry| entry.value().variant_id.as_deref() == Some(path.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Variant has inventory records; delete them first",
            ));
        }
        state
            .variants
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Variant not found"))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Bundle handlers
// ---------------------------------------------------------------------------

async fn list_bundle_items(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    if !state.products.contains_key(path.as_str()) {
        return json::respond(
            Err::<Value, UnifiedError>(UnifiedError::not_found("Product not found")),
            false,
        );
    }
    let items: Vec<BundleItem> = state
        .bundle_items
        .iter()
        .filter(|entry| entry.value().bundle_product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .collect();
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn add_bundle_item(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<AddBundleItem>,
) -> HttpResponse {
    let result: UnifiedResult<BundleItem> = (|| {
        let input = body.into_inner();
        let bundle_id = path.into_inner();
        let bundle = state
            .products
            .get(&bundle_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if bundle.kind != "bundle" {
            return Err(UnifiedError::bad_request(
                "Only bundle products can contain bundle items",
            ));
        }
        let quantity = input.quantity.unwrap_or(1);
        if quantity < 1 {
            return Err(UnifiedError::bad_request("Quantity must be at least 1"));
        }
        let child = state
            .products
            .get(&input.child_product_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Child product not found"))?;
        if let Some(override_minor) = input.price_override_minor {
            if override_minor < 0 {
                return Err(UnifiedError::bad_request(
                    "Price override must not be negative",
                ));
            }
            if child.price.currency != bundle.price.currency {
                return Err(UnifiedError::bad_request(
                    "Bundle child currency does not match the bundle currency",
                ));
            }
        }
        if state.bundle_would_cycle(&bundle_id, &input.child_product_id) {
            return Err(UnifiedError::bad_request(
                "Adding this product would create a bundle cycle",
            ));
        }
        if state.bundle_items.iter().any(|entry| {
            entry.value().bundle_product_id == bundle_id
                && entry.value().child_product_id == input.child_product_id
        }) {
            return Err(UnifiedError::conflict(
                "Product is already part of this bundle",
            ));
        }
        let item = BundleItem {
            id: new_id(),
            bundle_product_id: bundle_id,
            child_product_id: input.child_product_id,
            quantity,
            price_override_minor: input.price_override_minor,
            created_at: now(),
            updated_at: now(),
        };
        state.bundle_items.insert(item.id.clone(), item.clone());
        state.emit(
            "product.bundle.updated",
            json!({ "product_id": bundle_id, "bundle_item_id": item.id }),
        );
        Ok(item)
    })();
    respond_created(result)
}

async fn delete_bundle_item(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let item = state
            .bundle_items
            .remove(path.as_str())
            .map(|(_, item)| item)
            .ok_or_else(|| UnifiedError::not_found("Bundle item not found"))?;
        state.emit(
            "product.bundle.updated",
            json!({ "product_id": item.bundle_product_id, "deleted_bundle_item_id": item.id }),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, false)
}

async fn update_bundle_rule(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateBundleRule>,
) -> HttpResponse {
    let result: UnifiedResult<ProductDetail> = (|| {
        let input = body.into_inner();
        validate_bundle_rule(&input.bundle_rule)?;
        if input.bundle_rule == "discount_percent" {
            let discount = input.bundle_discount_bps.unwrap_or(0);
            validate_bundle_discount_bps(discount)?;
        } else if input.bundle_discount_bps.is_some() {
            return Err(UnifiedError::bad_request(
                "A discount only applies to the discount_percent bundle rule",
            ));
        }
        let mut product = state
            .products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if product.kind != "bundle" {
            return Err(UnifiedError::bad_request(
                "Only bundle products have a bundle pricing rule",
            ));
        }
        product.bundle_rule = input.bundle_rule;
        product.bundle_discount_bps = if product.bundle_rule == "discount_percent" {
            Some(input.bundle_discount_bps.unwrap_or(0))
        } else {
            None
        };
        product.updated_at = now();
        let detail = state.product_detail(&product.clone());
        let detail_value = serde_json::to_value(&detail).unwrap_or(Value::Null);
        drop(product);
        state.emit("product.bundle.updated", detail_value);
        Ok(detail)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Pricing tier handlers
// ---------------------------------------------------------------------------

async fn list_tiers(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    if !state.products.contains_key(path.as_str()) {
        return json::respond(
            Err::<Value, UnifiedError>(UnifiedError::not_found("Product not found")),
            false,
        );
    }
    let mut items: Vec<PriceTier> = state
        .tiers
        .iter()
        .filter(|entry| entry.value().product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .filter(|tier| {
            matches_search(&format!("{} {}", tier.slug, tier.label), &params)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| tier.status == *status)
        })
        .collect();
    items.sort_by(|a, b| a.slug.cmp(&b.slug));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_tier(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<CreatePriceTier>,
) -> HttpResponse {
    let result: UnifiedResult<PriceTier> = (|| {
        let input = body.into_inner();
        let product_id = path.into_inner();
        let product = state
            .products
            .get(&product_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if input.slug.trim().is_empty() {
            return Err(UnifiedError::bad_request("Tier slug must not be empty"));
        }
        if let Some(period) = &input.billing_period {
            validate_billing_period(period)?;
        }
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "disabled") {
                return Err(UnifiedError::bad_request(
                    "Tier status must be active or disabled",
                ));
            }
        }
        let price = resolve_money(&input.price, Some(&product.price.currency))?;
        if price.currency != product.price.currency {
            return Err(UnifiedError::bad_request(
                "Tier currency must match the product currency",
            ));
        }
        let renewal_price = match &input.renewal_price {
            Some(renewal) => {
                let renewal = resolve_money(renewal, Some(&product.price.currency))?;
                if renewal.currency != product.price.currency {
                    return Err(UnifiedError::bad_request(
                        "Renewal price currency must match the product currency",
                    ));
                }
                Some(renewal)
            }
            None => None,
        };
        if state.tiers.iter().any(|entry| {
            entry.value().product_id == product_id && entry.value().slug == input.slug.trim()
        }) {
            return Err(UnifiedError::conflict(format!(
                "Product already has a tier with slug \"{}\"",
                input.slug.trim()
            )));
        }
        let seats = input.seats.unwrap_or(1);
        if seats < 1 {
            return Err(UnifiedError::bad_request("Tier seats must be at least 1"));
        }
        let tier = PriceTier {
            id: new_id(),
            product_id,
            slug: input.slug.trim().to_string(),
            label: input.label.unwrap_or_else(|| input.slug.trim().to_string()),
            price,
            renewal_price,
            billing_period: input.billing_period,
            trial_days: input.trial_days.unwrap_or(0),
            seats,
            per_seat: input.per_seat.unwrap_or(false),
            download_limit: input.download_limit.unwrap_or(10),
            features: input.features,
            status: input.status.unwrap_or_else(default_tier_status),
            created_at: now(),
            updated_at: now(),
        };
        state.tiers.insert(tier.id.clone(), tier.clone());
        Ok(tier)
    })();
    respond_created(result)
}

async fn get_tier(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PriceTier> = state
        .tiers
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"));
    json::respond(result, false)
}

async fn update_tier(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<UpdatePriceTier>,
) -> HttpResponse {
    let result: UnifiedResult<PriceTier> = (|| {
        let input = body.into_inner();
        let new_slug = input.slug.as_ref().map(|slug| slug.trim().to_string());
        if let Some(slug) = &new_slug {
            if slug.is_empty() {
                return Err(UnifiedError::bad_request("Tier slug must not be empty"));
            }
        }
        if let Some(period) = &input.billing_period {
            validate_billing_period(period)?;
        }
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "disabled") {
                return Err(UnifiedError::bad_request(
                    "Tier status must be active or disabled",
                ));
            }
        }
        let new_price = match &input.price {
            Some(price) => Some(resolve_money(price, None)?),
            None => None,
        };
        let new_renewal = match &input.renewal_price {
            Some(renewal) => Some(resolve_money(renewal, None)?),
            None => None,
        };
        let mut tier = state
            .tiers
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))?;
        if let Some(slug) = new_slug {
            if state.tiers.iter().any(|entry| {
                entry.value().product_id == tier.product_id
                    && entry.value().slug == slug
                    && entry.key() != path.as_str()
            }) {
                return Err(UnifiedError::conflict(format!(
                    "Product already has a tier with slug \"{slug}\""
                )));
            }
            tier.slug = slug;
        }
        if let Some(label) = input.label {
            tier.label = label;
        }
        if let Some(price) = new_price {
            if price.currency != tier.price.currency {
                return Err(UnifiedError::bad_request(
                    "Tier currency must match the product currency",
                ));
            }
            tier.price = price;
        }
        if let Some(renewal) = new_renewal {
            if renewal.currency != tier.price.currency {
                return Err(UnifiedError::bad_request(
                    "Renewal price currency must match the product currency",
                ));
            }
            tier.renewal_price = Some(renewal);
        }
        if let Some(period) = input.billing_period {
            tier.billing_period = Some(period);
        }
        if let Some(trial_days) = input.trial_days {
            tier.trial_days = trial_days;
        }
        if let Some(seats) = input.seats {
            if seats < 1 {
                return Err(UnifiedError::bad_request("Tier seats must be at least 1"));
            }
            tier.seats = seats;
        }
        if let Some(per_seat) = input.per_seat {
            tier.per_seat = per_seat;
        }
        if let Some(download_limit) = input.download_limit {
            tier.download_limit = download_limit;
        }
        if let Some(features) = input.features {
            tier.features = features;
        }
        if let Some(status) = input.status {
            tier.status = status;
        }
        tier.updated_at = now();
        Ok(tier.clone())
    })();
    json::respond(result, false)
}

async fn delete_tier(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .tiers
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Regional price handlers
// ---------------------------------------------------------------------------

async fn list_regional_prices(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    if !state.products.contains_key(path.as_str()) {
        return json::respond(
            Err::<Value, UnifiedError>(UnifiedError::not_found("Product not found")),
            false,
        );
    }
    let mut items: Vec<RegionalPrice> = state
        .regional_prices
        .iter()
        .filter(|entry| entry.value().product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .collect();
    items.sort_by(|a, b| a.region.cmp(&b.region));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_regional_price(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<CreateRegionalPrice>,
) -> HttpResponse {
    let result: UnifiedResult<RegionalPrice> = (|| {
        let input = body.into_inner();
        let product_id = path.into_inner();
        if !state.products.contains_key(&product_id) {
            return Err(UnifiedError::not_found("Product not found"));
        }
        let region = input.region.trim().to_uppercase();
        validate_region(&region)?;
        let price = resolve_money(&input.price, None)?;
        if state.regional_prices.iter().any(|entry| {
            entry.value().product_id == product_id && entry.value().region == region
        }) {
            return Err(UnifiedError::conflict(format!(
                "Product already has a regional price for \"{region}\""
            )));
        }
        let regional = RegionalPrice {
            id: new_id(),
            product_id,
            region,
            price,
            created_at: now(),
            updated_at: now(),
        };
        state
            .regional_prices
            .insert(regional.id.clone(), regional.clone());
        Ok(regional)
    })();
    respond_created(result)
}

async fn update_regional_price(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateRegionalPrice>,
) -> HttpResponse {
    let result: UnifiedResult<RegionalPrice> = (|| {
        let input = body.into_inner();
        let new_region = input.region.as_ref().map(|region| region.trim().to_uppercase());
        if let Some(region) = &new_region {
            validate_region(region)?;
        }
        let new_price = match &input.price {
            Some(price) => Some(resolve_money(price, None)?),
            None => None,
        };
        let mut regional = state
            .regional_prices
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Regional price not found"))?;
        if let Some(region) = new_region {
            if state.regional_prices.iter().any(|entry| {
                entry.value().product_id == regional.product_id
                    && entry.value().region == region
                    && entry.key() != path.as_str()
            }) {
                return Err(UnifiedError::conflict(format!(
                    "Product already has a regional price for \"{region}\""
                )));
            }
            regional.region = region;
        }
        if let Some(price) = new_price {
            regional.price = price;
        }
        regional.updated_at = now();
        Ok(regional.clone())
    })();
    json::respond(result, false)
}

async fn delete_regional_price(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .regional_prices
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Regional price not found"))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Usage price handlers
// ---------------------------------------------------------------------------

async fn list_usage_prices(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    if !state.products.contains_key(path.as_str()) {
        return json::respond(
            Err::<Value, UnifiedError>(UnifiedError::not_found("Product not found")),
            false,
        );
    }
    let mut items: Vec<UsagePrice> = state
        .usage_prices
        .iter()
        .filter(|entry| entry.value().product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .filter(|usage| matches_search(&usage.metric, &params))
        .collect();
    items.sort_by(|a, b| a.metric.cmp(&b.metric));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_usage_price(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<CreateUsagePrice>,
) -> HttpResponse {
    let result: UnifiedResult<UsagePrice> = (|| {
        let input = body.into_inner();
        let product_id = path.into_inner();
        let product = state
            .products
            .get(&product_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if input.metric.trim().is_empty() {
            return Err(UnifiedError::bad_request("Metric must not be empty"));
        }
        let unit = resolve_money(&input.unit_price, Some(&product.price.currency))?;
        if state.usage_prices.iter().any(|entry| {
            entry.value().product_id == product_id
                && entry.value().metric == input.metric.trim()
        }) {
            return Err(UnifiedError::conflict(format!(
                "Product already has a usage price for metric \"{}\"",
                input.metric.trim()
            )));
        }
        let usage = UsagePrice {
            id: new_id(),
            product_id,
            metric: input.metric.trim().to_string(),
            unit_price_minor: unit.amount_minor,
            currency: unit.currency,
            included_units: input.included_units.unwrap_or(0),
            created_at: now(),
            updated_at: now(),
        };
        state.usage_prices.insert(usage.id.clone(), usage.clone());
        Ok(usage)
    })();
    respond_created(result)
}

async fn update_usage_price(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateUsagePrice>,
) -> HttpResponse {
    let result: UnifiedResult<UsagePrice> = (|| {
        let input = body.into_inner();
        let new_unit = match &input.unit_price {
            Some(unit) => Some(resolve_money(unit, None)?),
            None => None,
        };
        let mut usage = state
            .usage_prices
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Usage price not found"))?;
        if let Some(metric) = input.metric {
            if metric.trim().is_empty() {
                return Err(UnifiedError::bad_request("Metric must not be empty"));
            }
            if state.usage_prices.iter().any(|entry| {
                entry.value().product_id == usage.product_id
                    && entry.value().metric == metric.trim()
                    && entry.key() != path.as_str()
            }) {
                return Err(UnifiedError::conflict(format!(
                    "Product already has a usage price for metric \"{}\"",
                    metric.trim()
                )));
            }
            usage.metric = metric.trim().to_string();
        }
        if let Some(unit) = new_unit {
            if unit.currency != usage.currency {
                return Err(UnifiedError::bad_request(
                    "Usage price currency must not change",
                ));
            }
            usage.unit_price_minor = unit.amount_minor;
        }
        if let Some(included) = input.included_units {
            usage.included_units = included;
        }
        usage.updated_at = now();
        Ok(usage.clone())
    })();
    json::respond(result, false)
}

async fn delete_usage_price(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .usage_prices
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Usage price not found"))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Tax rule handlers (global)
// ---------------------------------------------------------------------------

async fn list_tax_rules(
    state: web::Data<Arc<ProductsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<TaxRule> = state
        .tax_rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| {
            matches_search(&format!("{} {}", rule.country, rule.name), &params)
                && params
                    .filters
                    .get("country")
                    .is_none_or(|country| rule.country.eq_ignore_ascii_case(country))
        })
        .collect();
    items.sort_by(|a, b| a.country.cmp(&b.country));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_tax_rule(
    state: web::Data<Arc<ProductsState>>,
    body: web::Json<CreateTaxRule>,
) -> HttpResponse {
    let result: UnifiedResult<TaxRule> = (|| {
        let input = body.into_inner();
        let country = input.country.trim().to_uppercase();
        validate_region(&country)?;
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Tax rule name must not be empty"));
        }
        validate_tax_rate_bps(input.rate_bps)?;
        if state
            .tax_rules
            .iter()
            .any(|entry| entry.value().country == country)
        {
            return Err(UnifiedError::conflict(format!(
                "A tax rule already exists for country \"{country}\""
            )));
        }
        let rule = TaxRule {
            id: new_id(),
            country,
            name: input.name.trim().to_string(),
            rate_bps: input.rate_bps,
            created_at: now(),
            updated_at: now(),
        };
        state.tax_rules.insert(rule.id.clone(), rule.clone());
        Ok(rule)
    })();
    respond_created(result)
}

async fn update_tax_rule(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateTaxRule>,
) -> HttpResponse {
    let result: UnifiedResult<TaxRule> = (|| {
        let input = body.into_inner();
        let new_country = input.country.as_ref().map(|country| country.trim().to_uppercase());
        if let Some(country) = &new_country {
            validate_region(country)?;
        }
        if let Some(rate_bps) = input.rate_bps {
            validate_tax_rate_bps(rate_bps)?;
        }
        let mut rule = state
            .tax_rules
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Tax rule not found"))?;
        if let Some(country) = new_country {
            if state.tax_rules.iter().any(|entry| {
                entry.value().country == country && entry.key() != path.as_str()
            }) {
                return Err(UnifiedError::conflict(format!(
                    "A tax rule already exists for country \"{country}\""
                )));
            }
            rule.country = country;
        }
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Tax rule name must not be empty"));
            }
            rule.name = name.trim().to_string();
        }
        if let Some(rate_bps) = input.rate_bps {
            rule.rate_bps = rate_bps;
        }
        rule.updated_at = now();
        Ok(rule.clone())
    })();
    json::respond(result, false)
}

async fn delete_tax_rule(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .tax_rules
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Tax rule not found"))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Inventory handlers (physical products only)
// ---------------------------------------------------------------------------

async fn list_inventory(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    if !state.products.contains_key(path.as_str()) {
        return json::respond(
            Err::<Value, UnifiedError>(UnifiedError::not_found("Product not found")),
            false,
        );
    }
    let items: Vec<Inventory> = state
        .inventory
        .iter()
        .filter(|entry| entry.value().product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .collect();
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn list_low_stock(
    state: web::Data<Arc<ProductsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<Inventory> = state
        .inventory
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            record.quantity - record.reserved <= record.low_stock_threshold
                && params.filters.get("product_id").is_none_or(|product_id| {
                    record.product_id == *product_id
                })
        })
        .collect();
    items.sort_by(|a, b| {
        (a.quantity - a.reserved).cmp(&(b.quantity - b.reserved))
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn adjust_inventory(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<AdjustInventory>,
) -> HttpResponse {
    let result: UnifiedResult<Inventory> = (|| {
        let input = body.into_inner();
        let product_id = path.into_inner();
        let product = state
            .products
            .get(&product_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if product.kind != "physical" {
            return Err(UnifiedError::bad_request(
                "Inventory only applies to physical products",
            ));
        }
        if let Some(variant_id) = &input.variant_id {
            match state.variants.get(variant_id.as_str()) {
                Some(variant) if variant.product_id == product_id => {}
                _ => {
                    return Err(UnifiedError::not_found(
                        "Variant not found for this product",
                    ));
                }
            }
        }
        let quantity_delta = input.quantity_delta.unwrap_or(0);
        let reserved_delta = input.reserved_delta.unwrap_or(0);
        if let Some(threshold) = input.low_stock_threshold {
            if threshold < 0 {
                return Err(UnifiedError::bad_request(
                    "Low-stock threshold must not be negative",
                ));
            }
        }
        // Find the existing record for this product / variant pair, or
        // create one (a fresh record starts at zero stock).
        let record_id = state
            .inventory
            .iter()
            .find(|entry| {
                entry.value().product_id == product_id
                    && entry.value().variant_id == input.variant_id
            })
            .map(|entry| entry.key().clone());
        let mut record = match record_id {
            Some(id) => state
                .inventory
                .get_mut(&id)
                .expect("inventory record still present")
                .clone(),
            None => Inventory {
                id: new_id(),
                product_id: product_id.clone(),
                variant_id: input.variant_id.clone(),
                quantity: 0,
                reserved: 0,
                low_stock_threshold: input.low_stock_threshold.unwrap_or(0),
                created_at: now(),
                updated_at: now(),
            },
        };
        let new_quantity = record
            .quantity
            .checked_add(quantity_delta)
            .ok_or_else(|| UnifiedError::bad_request("Stock quantity overflows"))?;
        if new_quantity < 0 {
            return Err(UnifiedError::bad_request(
                "Stock quantity must not go below zero",
            ));
        }
        let new_reserved = record
            .reserved
            .checked_add(reserved_delta)
            .ok_or_else(|| UnifiedError::bad_request("Reserved stock overflows"))?;
        if new_reserved < 0 || new_reserved > new_quantity {
            return Err(UnifiedError::bad_request(
                "Reserved stock must stay between zero and the on-hand quantity",
            ));
        }
        record.quantity = new_quantity;
        record.reserved = new_reserved;
        if let Some(threshold) = input.low_stock_threshold {
            record.low_stock_threshold = threshold;
        }
        record.updated_at = now();
        let updated = record.clone();
        state.inventory.insert(record.id.clone(), record);
        state.emit(
            "product.inventory.adjusted",
            json!({
                "product_id": product_id,
                "variant_id": updated.variant_id,
                "quantity": updated.quantity,
                "reserved": updated.reserved,
                "quantity_delta": quantity_delta,
                "reserved_delta": reserved_delta,
                "reason": input.reason,
            }),
        );
        if updated.quantity - updated.reserved <= updated.low_stock_threshold {
            state.emit(
                "product.inventory.low_stock",
                json!({
                    "product_id": product_id,
                    "variant_id": updated.variant_id,
                    "quantity": updated.quantity,
                    "reserved": updated.reserved,
                    "available": updated.quantity - updated.reserved,
                    "low_stock_threshold": updated.low_stock_threshold,
                }),
            );
        }
        Ok(updated)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Product file and version handlers (references only)
// ---------------------------------------------------------------------------

async fn list_product_files(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    if !state.products.contains_key(path.as_str()) {
        return json::respond(
            Err::<Value, UnifiedError>(UnifiedError::not_found("Product not found")),
            false,
        );
    }
    let mut items: Vec<ProductFile> = state
        .files
        .iter()
        .filter(|entry| entry.value().product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_product_file(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<CreateProductFile>,
) -> HttpResponse {
    let result: UnifiedResult<ProductFile> = (|| {
        let input = body.into_inner();
        let product_id = path.into_inner();
        if !state.products.contains_key(&product_id) {
            return Err(UnifiedError::not_found("Product not found"));
        }
        if input.software_id.is_none()
            && input.release_id.is_none()
            && input.file_id.is_none()
        {
            return Err(UnifiedError::bad_request(
                "At least one of software_id, release_id, or file_id is required",
            ));
        }
        if let Some(checksum) = &input.checksum_sha256 {
            validate_hex_checksum(&checksum.to_lowercase(), 64, "Checksum SHA-256")?;
        }
        let file = ProductFile {
            id: new_id(),
            product_id,
            software_id: input.software_id,
            software_version: input.software_version,
            release_id: input.release_id,
            file_id: input.file_id,
            filename: input.filename,
            checksum_sha256: input.checksum_sha256.map(|value| value.to_lowercase()),
            created_at: now(),
            updated_at: now(),
        };
        state.files.insert(file.id.clone(), file.clone());
        Ok(file)
    })();
    respond_created(result)
}

async fn delete_product_file(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .files
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Product file not found"))
    })();
    json::respond(result, false)
}

async fn list_product_versions(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    if !state.products.contains_key(path.as_str()) {
        return json::respond(
            Err::<Value, UnifiedError>(UnifiedError::not_found("Product not found")),
            false,
        );
    }
    let mut items: Vec<ProductVersion> = state
        .versions
        .iter()
        .filter(|entry| entry.value().product_id == path.as_str())
        .map(|entry| entry.value().clone())
        .collect();
    items.sort_by(|a, b| {
        let parsed_a = Version::parse(&a.version);
        let parsed_b = Version::parse(&b.version);
        match (parsed_a, parsed_b) {
            (Ok(left), Ok(right)) => left.cmp(&right),
            _ => a.version.cmp(&b.version),
        }
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_product_version(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<CreateProductVersion>,
) -> HttpResponse {
    let result: UnifiedResult<ProductVersion> = (|| {
        let input = body.into_inner();
        let product_id = path.into_inner();
        if !state.products.contains_key(&product_id) {
            return Err(UnifiedError::not_found("Product not found"));
        }
        Version::parse(&input.version).map_err(|_| {
            UnifiedError::bad_request(format!(
                "Version \"{}\" is not a valid semantic version (expected MAJOR.MINOR.PATCH)",
                input.version
            ))
        })?;
        if state.versions.iter().any(|entry| {
            entry.value().product_id == product_id && entry.value().version == input.version
        }) {
            return Err(UnifiedError::conflict(format!(
                "Product already has version \"{}\"",
                input.version
            )));
        }
        let version = ProductVersion {
            id: new_id(),
            product_id,
            version: input.version,
            software_id: input.software_id,
            release_id: input.release_id,
            changelog: input.changelog.unwrap_or_default(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .versions
            .insert(version.id.clone(), version.clone());
        Ok(version)
    })();
    respond_created(result)
}

async fn delete_product_version(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .versions
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Product version not found"))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Taxonomy handlers (same shapes as the software module)
// ---------------------------------------------------------------------------

async fn list_taxonomy(
    state: web::Data<Arc<ProductsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<TaxonomyNode> = state
        .taxonomy
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|node| {
            matches_search(&format!("{} {} {}", node.name, node.slug, node.description), &params)
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| node.kind == *kind)
                && params.filters.get("parent_id").is_none_or(|parent_id| {
                    node.parent_id.as_deref() == Some(parent_id.as_str())
                })
        })
        .collect();
    items.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.cmp(&b.name))
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn taxonomy_tree(
    state: web::Data<Arc<ProductsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let kind = params.filters.get("kind").map(String::as_str);
    if let Some(kind) = kind {
        if !TAXONOMY_KINDS.contains(&kind) {
            return json::respond(
                Err::<Value, UnifiedError>(UnifiedError::bad_request(
                    "Kind must be group or category",
                )),
                false,
            );
        }
    }
    let tree = build_tree(&state, None, kind);
    json::success_response(tree, 200)
}

async fn create_taxonomy_node(
    state: web::Data<Arc<ProductsState>>,
    body: web::Json<CreateTaxonomyNode>,
) -> HttpResponse {
    let result: UnifiedResult<TaxonomyNode> = (|| {
        let input = body.into_inner();
        if !TAXONOMY_KINDS.contains(&input.kind.as_str()) {
            return Err(UnifiedError::bad_request(
                "Kind must be group or category",
            ));
        }
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Name must not be empty"));
        }
        let slug = match &input.slug {
            Some(slug) if !slug.trim().is_empty() => slugify(slug),
            _ => slugify(&input.name),
        };
        if slug.is_empty() {
            return Err(UnifiedError::bad_request(
                "Could not derive a slug from the taxonomy name",
            ));
        }
        if state
            .taxonomy
            .iter()
            .any(|entry| entry.value().slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "Taxonomy node with slug \"{slug}\" already exists"
            )));
        }
        if let Some(parent_id) = &input.parent_id {
            match state.taxonomy.get(parent_id.as_str()) {
                Some(parent) if parent.kind == input.kind => {}
                _ => {
                    return Err(UnifiedError::not_found(
                        "Parent taxonomy node not found for this kind",
                    ));
                }
            }
        }
        let (depth, path) = taxonomy_path(&state, input.parent_id.as_deref(), &slug);
        let node = TaxonomyNode {
            id: new_id(),
            kind: input.kind,
            slug,
            name: input.name.trim().to_string(),
            description: input.description.unwrap_or_default(),
            parent_id: input.parent_id,
            sort_order: input.sort_order.unwrap_or(0),
            depth,
            path,
            created_at: now(),
            updated_at: now(),
        };
        state.taxonomy.insert(node.id.clone(), node.clone());
        Ok(node)
    })();
    respond_created(result)
}

async fn get_taxonomy_node(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<TaxonomyNode> = state
        .taxonomy
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Taxonomy node not found"));
    json::respond(result, false)
}

async fn update_taxonomy_node(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateTaxonomyNode>,
) -> HttpResponse {
    let result: UnifiedResult<TaxonomyNode> = (|| {
        let input = body.into_inner();
        let new_slug = input.slug.as_ref().map(|slug| slugify(slug));
        if let Some(slug) = &new_slug {
            if slug.is_empty() {
                return Err(UnifiedError::bad_request("Slug must not be empty"));
            }
            if state.taxonomy.iter().any(|entry| {
                entry.value().slug == *slug && entry.key() != path.as_str()
            }) {
                return Err(UnifiedError::conflict(format!(
                    "Taxonomy node with slug \"{slug}\" already exists"
                )));
            }
        }
        let mut node = state
            .taxonomy
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Taxonomy node not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Name must not be empty"));
            }
            node.name = name.trim().to_string();
        }
        if let Some(slug) = new_slug {
            node.slug = slug;
            // The path embeds the slug, so recompute this subtree.
            let node_id = node.id.clone();
            drop(node);
            recompute_subtree(&state, &node_id);
            return Ok(state
                .taxonomy
                .get(path.as_str())
                .map(|entry| entry.value().clone())
                .expect("taxonomy node still present"));
        }
        if let Some(description) = input.description {
            node.description = description;
        }
        if let Some(sort_order) = input.sort_order {
            node.sort_order = sort_order;
        }
        node.updated_at = now();
        Ok(node.clone())
    })();
    json::respond(result, false)
}

async fn delete_taxonomy_node(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let node = state
            .taxonomy
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Taxonomy node not found"))?;
        if state
            .taxonomy
            .iter()
            .any(|entry| entry.value().parent_id.as_deref() == Some(path.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Taxonomy node has children; move or delete the children first",
            ));
        }
        let referenced = state.products.iter().any(|entry| {
            (node.kind == "group" && entry.value().group_id.as_deref() == Some(path.as_str()))
                || (node.kind == "category"
                    && entry.value().category_id.as_deref() == Some(path.as_str()))
        });
        if referenced {
            return Err(UnifiedError::conflict(
                "Taxonomy node is referenced by products",
            ));
        }
        state.taxonomy.remove(path.as_str());
        state.emit(
            "product.taxonomy.deleted",
            serde_json::to_value(&node).unwrap_or(Value::Null),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, false)
}

async fn move_taxonomy_node(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<MoveTaxonomyNode>,
) -> HttpResponse {
    let result: UnifiedResult<TaxonomyNode> = (|| {
        let input = body.into_inner();
        let node = state
            .taxonomy
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Taxonomy node not found"))?;
        if let Some(parent_id) = &input.parent_id {
            if parent_id == path.as_str() {
                return Err(UnifiedError::bad_request(
                    "A taxonomy node cannot be its own parent",
                ));
            }
            match state.taxonomy.get(parent_id.as_str()) {
                Some(parent) if parent.kind == node.kind => {}
                _ => {
                    return Err(UnifiedError::not_found(
                        "Parent taxonomy node not found for this kind",
                    ));
                }
            }
            if is_descendant_of(&state, parent_id, path.as_str()) {
                return Err(UnifiedError::bad_request(
                    "A taxonomy node cannot move under its own descendant",
                ));
            }
        }
        let node_id = path.as_str().to_string();
        if let Some(mut entry) = state.taxonomy.get_mut(&node_id) {
            entry.parent_id = input.parent_id;
            if let Some(sort_order) = input.sort_order {
                entry.sort_order = sort_order;
            }
            entry.updated_at = now();
        }
        recompute_subtree(&state, &node_id);
        Ok(state
            .taxonomy
            .get(&node_id)
            .map(|entry| entry.value().clone())
            .expect("taxonomy node still present"))
    })();
    json::respond(result, false)
}

async fn list_taxonomy_children(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    if !state.taxonomy.contains_key(path.as_str()) {
        return json::respond(
            Err::<Value, UnifiedError>(UnifiedError::not_found("Taxonomy node not found")),
            false,
        );
    }
    let mut items: Vec<TaxonomyNode> = state
        .taxonomy
        .iter()
        .filter(|entry| entry.value().parent_id.as_deref() == Some(path.as_str()))
        .map(|entry| entry.value().clone())
        .collect();
    items.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.cmp(&b.name))
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

// ---------------------------------------------------------------------------
// Tag handlers
// ---------------------------------------------------------------------------

async fn list_tags(
    state: web::Data<Arc<ProductsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<Tag> = state
        .tags
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|tag| matches_search(&format!("{} {}", tag.name, tag.slug), &params))
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_tag(
    state: web::Data<Arc<ProductsState>>,
    body: web::Json<CreateTag>,
) -> HttpResponse {
    let result: UnifiedResult<Tag> = (|| {
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Tag name must not be empty"));
        }
        let slug = match &input.slug {
            Some(slug) if !slug.trim().is_empty() => slugify(slug),
            _ => slugify(&input.name),
        };
        if slug.is_empty() {
            return Err(UnifiedError::bad_request(
                "Could not derive a slug from the tag name",
            ));
        }
        if state.tags.iter().any(|entry| entry.value().slug == slug) {
            return Err(UnifiedError::conflict(format!(
                "Tag with slug \"{slug}\" already exists"
            )));
        }
        let tag = Tag {
            id: new_id(),
            slug,
            name: input.name.trim().to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.tags.insert(tag.id.clone(), tag.clone());
        Ok(tag)
    })();
    respond_created(result)
}

async fn get_tag(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Tag> = state
        .tags
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Tag not found"));
    json::respond(result, false)
}

async fn update_tag(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateTag>,
) -> HttpResponse {
    let result: UnifiedResult<Tag> = (|| {
        let input = body.into_inner();
        let new_slug = input.slug.as_ref().map(|slug| slugify(slug));
        if let Some(slug) = &new_slug {
            if slug.is_empty() {
                return Err(UnifiedError::bad_request("Slug must not be empty"));
            }
            if state
                .tags
                .iter()
                .any(|entry| entry.value().slug == *slug && entry.key() != path.as_str())
            {
                return Err(UnifiedError::conflict(format!(
                    "Tag with slug \"{slug}\" already exists"
                )));
            }
        }
        let mut tag = state
            .tags
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Tag not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Tag name must not be empty"));
            }
            tag.name = name.trim().to_string();
        }
        if let Some(slug) = new_slug {
            tag.slug = slug;
        }
        tag.updated_at = now();
        Ok(tag.clone())
    })();
    json::respond(result, false)
}

async fn delete_tag(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tag = state
            .tags
            .remove(path.as_str())
            .map(|(_, tag)| tag)
            .ok_or_else(|| UnifiedError::not_found("Tag not found"))?;
        for mut entry in state.product_tags.iter_mut() {
            entry.value_mut().remove(path.as_str());
        }
        state.emit(
            "product.taxonomy.deleted",
            serde_json::to_value(&tag).unwrap_or(Value::Null),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, false)
}

async fn tag_cloud(
    state: web::Data<Arc<ProductsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let sort = params.allowed_sort(&["name", "product_count", "created_at"], "product_count");
    let mut counts: HashMap<String, u64> = HashMap::new();
    for entry in state.product_tags.iter() {
        for tag_id in entry.value().iter() {
            *counts.entry(tag_id.clone()).or_insert(0) += 1;
        }
    }
    let mut items: Vec<TagCount> = state
        .tags
        .iter()
        .map(|entry| entry.value().clone())
        .map(|tag| TagCount {
            product_count: counts.get(&tag.id).copied().unwrap_or(0),
            tag,
        })
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.tag.name.cmp(&b.tag.name)),
        "created_at" => items.sort_by(|a, b| a.tag.created_at.cmp(&b.tag.created_at)),
        _ => items.sort_by(|a, b| a.product_count.cmp(&b.product_count)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn list_product_tags(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Vec<Tag>> = state
        .products
        .get(path.as_str())
        .map(|_| {
            let tag_ids = state
                .product_tags
                .get(path.as_str())
                .map(|entry| entry.value().clone())
                .unwrap_or_default();
            let mut tags: Vec<Tag> = tag_ids
                .iter()
                .filter_map(|id| state.tags.get(id).map(|entry| entry.value().clone()))
                .collect();
            tags.sort_by(|a, b| a.name.cmp(&b.name));
            tags
        })
        .ok_or_else(|| UnifiedError::not_found("Product not found"));
    json::respond(result, false)
}

async fn attach_product_tag(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<String>,
    body: web::Json<AttachTag>,
) -> HttpResponse {
    let result: UnifiedResult<Vec<Tag>> = (|| {
        let input = body.into_inner();
        if !state.products.contains_key(path.as_str()) {
            return Err(UnifiedError::not_found("Product not found"));
        }
        if !state.tags.contains_key(&input.tag_id) {
            return Err(UnifiedError::not_found("Tag not found"));
        }
        state
            .product_tags
            .entry(path.as_str().to_string())
            .or_default()
            .insert(input.tag_id);
        let tag_ids = state
            .product_tags
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        let mut tags: Vec<Tag> = tag_ids
            .iter()
            .filter_map(|id| state.tags.get(id).map(|entry| entry.value().clone()))
            .collect();
        tags.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(tags)
    })();
    json::respond(result, false)
}

async fn detach_product_tag(
    state: web::Data<Arc<ProductsState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (product_id, tag_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        if !state.products.contains_key(&product_id) {
            return Err(UnifiedError::not_found("Product not found"));
        }
        let removed = state
            .product_tags
            .get_mut(&product_id)
            .is_some_and(|mut tags| tags.remove(&tag_id));
        if removed {
            Ok(json!({ "deleted": true, "product_id": product_id, "tag_id": tag_id }))
        } else {
            Err(UnifiedError::not_found("Tag is not attached to this product"))
        }
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Hook handlers: this module answers other modules' questions.
// ---------------------------------------------------------------------------

/// Answer `product.entitlement.check` with payload
/// `{"product_id", "user_id"}` and result `{"entitled": bool, ...}`.
/// The check itself runs through the store's entitlement data via
/// `store.entitlement.check` (see `ProductsState::check_entitlement`).
fn on_product_entitlement_check(state: Arc<ProductsState>, ctx: &mut HookContext) {
    let product_id = ctx
        .get("product_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if product_id.trim().is_empty() || user_id.trim().is_empty() {
        ctx.add_result(json!({
            "entitled": false,
            "reason": "product_id and user_id are required",
        }));
        return;
    }
    ctx.add_result(state.check_entitlement(&product_id, &user_id));
}

/// Answer `product.price.quote` with the exact price breakdown
/// billing needs for an invoice line. Payload: `{"product_id",
/// "tier_id"?, "variant_id"?, "quantity"?, "region"?,
/// "usage_units"?}`.
fn on_product_price_quote(state: Arc<ProductsState>, ctx: &mut HookContext) {
    let product_id = ctx
        .get("product_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let Some(product) = state
        .products
        .get(&product_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "error": "Product not found",
            "product_id": product_id,
        }));
        return;
    };
    let request = PriceQuoteRequest {
        tier_id: ctx
            .get("tier_id")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        variant_id: ctx
            .get("variant_id")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        quantity: ctx
            .get("quantity")
            .and_then(|value| value.as_u64())
            .map(|value| value as u32),
        region: ctx
            .get("region")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        usage_units: ctx.get("usage_units").and_then(|value| value.as_u64()),
    };
    match state.quote_price(&product, &request) {
        Ok(quote) => {
            ctx.add_result(serde_json::to_value(&quote).unwrap_or(Value::Null));
        }
        Err(error) => {
            ctx.add_result(json!({ "error": error.message }));
        }
    }
}

/// Answer `product.license.template` with the license terms the
/// licensing module needs when it issues a license for this product.
/// Payload: `{"product_id"}`.
fn on_product_license_template(state: Arc<ProductsState>, ctx: &mut HookContext) {
    let product_id = ctx
        .get("product_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    match state.products.get(&product_id) {
        Some(product) => {
            ctx.add_result(json!({
                "product_id": product.id,
                "licensed": product.license_template.requires_license,
                "license_template": product.license_template,
                "seats": product.license_template.seats,
                "download_limit": product.license_template.download_limit,
            }));
        }
        None => {
            ctx.add_result(json!({
                "error": "Product not found",
                "product_id": product_id,
            }));
        }
    }
}

/// Answer `product.support.route` with the department that owns
/// support for a product. Payload: `{"product_id"}`. No departments
/// module exists in the tree yet, so the department is the plain
/// reference stored on the product.
fn on_product_support_route(state: Arc<ProductsState>, ctx: &mut HookContext) {
    let product_id = ctx
        .get("product_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    match state.products.get(&product_id) {
        Some(product) => {
            ctx.add_result(json!({
                "product_id": product.id,
                "department_id": product.support_department_id,
                "department_name": product.support_department_name,
            }));
        }
        None => {
            ctx.add_result(json!({
                "error": "Product not found",
                "product_id": product_id,
            }));
        }
    }
}

/// Answer `product.exists` with `{"exists": bool}`. Payload:
/// `{"product_id"}`.
fn on_product_exists(state: Arc<ProductsState>, ctx: &mut HookContext) {
    let product_id = ctx
        .get("product_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    ctx.add_result(json!({
        "exists": state.products.contains_key(&product_id),
        "product_id": product_id,
    }));
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct ProductsModule {
    state: Arc<ProductsState>,
}

impl ProductsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(ProductsState::new()),
        }
    }
}

impl Default for ProductsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ProductsModule {
    fn id(&self) -> &'static str {
        "products"
    }

    fn name(&self) -> &'static str {
        "Product Management"
    }

    fn description(&self) -> &'static str {
        "Product management, the single source of truth for what is sold: lifecycle, kinds, variants and options, bundles, pricing tiers with per-seat, usage-based and regional pricing, taxes, inventory with low-stock alerts, product files and versions referencing the Software Manager, taxonomy and tags, vendor ownership, store listing, license templates, billing price quotes, support routing, and entitlement checks."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["store", "licensing", "billing", "software"]
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
            "product.entitlement.check",
            "products",
            Arc::new(move |ctx: &mut HookContext| {
                on_product_entitlement_check(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "product.price.quote",
            "products",
            Arc::new(move |ctx: &mut HookContext| {
                on_product_price_quote(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "product.license.template",
            "products",
            Arc::new(move |ctx: &mut HookContext| {
                on_product_license_template(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "product.support.route",
            "products",
            Arc::new(move |ctx: &mut HookContext| {
                on_product_support_route(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "product.exists",
            "products",
            Arc::new(move |ctx: &mut HookContext| {
                on_product_exists(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        // Specific single-segment paths are registered before the
        // generic `/{id}` routes so they are matched first.
        cfg.service(
            web::scope("/api/v1/products")
                .route("/taxonomy", web::get().to(list_taxonomy))
                .route("/taxonomy", web::post().to(create_taxonomy_node))
                .route("/taxonomy/tree", web::get().to(taxonomy_tree))
                .route("/taxonomy/{id}", web::get().to(get_taxonomy_node))
                .route("/taxonomy/{id}", web::put().to(update_taxonomy_node))
                .route("/taxonomy/{id}", web::delete().to(delete_taxonomy_node))
                .route("/taxonomy/{id}/move", web::post().to(move_taxonomy_node))
                .route("/taxonomy/{id}/children", web::get().to(list_taxonomy_children))
                .route("/tags", web::get().to(list_tags))
                .route("/tags", web::post().to(create_tag))
                .route("/tags/cloud", web::get().to(tag_cloud))
                .route("/tags/{id}", web::get().to(get_tag))
                .route("/tags/{id}", web::put().to(update_tag))
                .route("/tags/{id}", web::delete().to(delete_tag))
                .route("/tax-rules", web::get().to(list_tax_rules))
                .route("/tax-rules", web::post().to(create_tax_rule))
                .route("/tax-rules/{id}", web::put().to(update_tax_rule))
                .route("/tax-rules/{id}", web::delete().to(delete_tax_rule))
                .route("/entitlement-check", web::post().to(entitlement_check))
                .route("/inventory/low-stock", web::get().to(list_low_stock))
                .route("", web::get().to(list_products))
                .route("", web::post().to(create_product))
                .route("/{id}", web::get().to(get_product))
                .route("/{id}", web::put().to(update_product))
                .route("/{id}", web::delete().to(delete_product))
                .route("/{id}/transition", web::post().to(transition_product))
                .route("/{id}/list", web::post().to(list_product))
                .route("/{id}/delist", web::post().to(delist_product))
                .route("/{id}/price-quote", web::post().to(price_quote))
                .route("/{id}/license-template", web::get().to(get_license_template))
                .route("/{id}/support-ticket", web::post().to(create_support_ticket))
                .route("/{id}/variants", web::get().to(list_variants))
                .route("/{id}/variants", web::post().to(create_variant))
                .route("/variants/{id}", web::get().to(get_variant))
                .route("/variants/{id}", web::put().to(update_variant))
                .route("/variants/{id}", web::delete().to(delete_variant))
                .route("/{id}/options", web::get().to(list_options))
                .route("/{id}/options", web::post().to(create_option))
                .route("/options/{id}", web::delete().to(delete_option))
                .route("/{id}/bundle", web::get().to(list_bundle_items))
                .route("/{id}/bundle", web::put().to(update_bundle_rule))
                .route("/{id}/bundle/items", web::post().to(add_bundle_item))
                .route("/bundle-items/{id}", web::delete().to(delete_bundle_item))
                .route("/{id}/tiers", web::get().to(list_tiers))
                .route("/{id}/tiers", web::post().to(create_tier))
                .route("/tiers/{id}", web::get().to(get_tier))
                .route("/tiers/{id}", web::put().to(update_tier))
                .route("/tiers/{id}", web::delete().to(delete_tier))
                .route("/{id}/regional-prices", web::get().to(list_regional_prices))
                .route("/{id}/regional-prices", web::post().to(create_regional_price))
                .route("/regional-prices/{id}", web::put().to(update_regional_price))
                .route("/regional-prices/{id}", web::delete().to(delete_regional_price))
                .route("/{id}/usage-prices", web::get().to(list_usage_prices))
                .route("/{id}/usage-prices", web::post().to(create_usage_price))
                .route("/usage-prices/{id}", web::put().to(update_usage_price))
                .route("/usage-prices/{id}", web::delete().to(delete_usage_price))
                .route("/{id}/inventory", web::get().to(list_inventory))
                .route("/{id}/inventory/adjust", web::post().to(adjust_inventory))
                .route("/{id}/files", web::get().to(list_product_files))
                .route("/{id}/files", web::post().to(create_product_file))
                .route("/files/{id}", web::delete().to(delete_product_file))
                .route("/{id}/versions", web::get().to(list_product_versions))
                .route("/{id}/versions", web::post().to(create_product_version))
                .route("/versions/{id}", web::delete().to(delete_product_version))
                .route("/{id}/tags", web::get().to(list_product_tags))
                .route("/{id}/tags", web::post().to(attach_product_tag))
                .route("/{id}/tags/{tag_id}", web::delete().to(detach_product_tag)),
        );
    }

    fn route_info(&self) -> Vec<crate::core::modules::RouteInfo> {
        vec![
            crate::core::modules::RouteInfo { method: "GET", path: "/api/v1/products", owner: "products", description: "List products with pagination and filters." },
            crate::core::modules::RouteInfo { method: "POST", path: "/api/v1/products", owner: "products", description: "Create a product (starts as draft)." },
            crate::core::modules::RouteInfo { method: "GET", path: "/api/v1/products/{id}", owner: "products", description: "Get one product with tags, counts, and inventory summary." },
            crate::core::modules::RouteInfo { method: "PUT", path: "/api/v1/products/{id}", owner: "products", description: "Update a product, including validated lifecycle transitions." },
            crate::core::modules::RouteInfo { method: "DELETE", path: "/api/v1/products/{id}", owner: "products", description: "Delete a product with no sub-resources." },
            crate::core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/transition", owner: "products", description: "Move a product to another lifecycle status." },
            crate::core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/list", owner: "products", description: "List an active product in the store (emits product.listed)." },
            crate::core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/delist", owner: "products", description: "Remove a product from the store (emits product.delisted)." },
            crate::core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/price-quote", owner: "products", description: "Exact price quote with tax, in integer minor units." },
            crate::core::modules::RouteInfo { method: "GET", path: "/api/v1/products/{id}/license-template", owner: "products", description: "License terms declared by a product." },
            crate::core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/support-ticket", owner: "products", description: "Open a support ticket routed to the product's department." },
            crate::core::modules::RouteInfo { method: "POST", path: "/api/v1/products/entitlement-check", owner: "products", description: "Check whether a user holds an active entitlement for a product." },
            crate::core::modules::RouteInfo { method: "GET", path: "/api/v1/products/tax-rules", owner: "products", description: "List tax rules." },
            crate::core::modules::RouteInfo { method: "POST", path: "/api/v1/products/tax-rules", owner: "products", description: "Create a tax rule (rate in basis points)." },
            crate::core::modules::RouteInfo { method: "GET", path: "/api/v1/products/inventory/low-stock", owner: "products", description: "List inventory records at or below their low-stock threshold." },
            crate::core::modules::RouteInfo { method: "GET", path: "/api/v1/products/taxonomy", owner: "products", description: "List product groups and categories." },
            crate::core::modules::RouteInfo { method: "GET", path: "/api/v1/products/taxonomy/tree", owner: "products", description: "Product taxonomy as a tree." },
            crate::core::modules::RouteInfo { method: "GET", path: "/api/v1/products/tags", owner: "products", description: "List product tags." },
            crate::core::modules::RouteInfo { method: "GET", path: "/api/v1/products/tags/cloud", owner: "products", description: "Tag cloud with product counts." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ProductsModule::new()));
}
