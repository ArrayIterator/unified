//! Product catalogue types: products, variants, pricing, taxonomy, tags.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap};
use super::money::{Money, MoneyInput};

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

/// Proration policies for mid-cycle plan changes on subscription
/// products: `prorate` charges the new plan for the remaining time
/// and credits the unused time on the old plan, `no_prorate` applies
/// the change at the next period end with no mid-cycle charge or
/// credit, and `credit_only` credits the unused time on the old plan
/// and applies the new price from the next period.
pub const PRORATION_POLICIES: [&str; 3] = ["prorate", "no_prorate", "credit_only"];

/// Maximum failed-payment (dunning) retries for a subscription.
pub const MAX_DUNNING_RETRIES: u32 = 10;

/// Maximum grace period in days after a failed renewal payment.
pub const MAX_GRACE_PERIOD_DAYS: u32 = 90;

/// Maximum days between dunning retries.
pub const MAX_DUNNING_RETRY_INTERVAL_DAYS: u32 = 30;

/// Bundle pricing rules: `sum` charges the sum of the children,
/// `fixed` charges the bundle's own base price, `discount_percent`
/// charges the sum less the configured discount.
pub const BUNDLE_RULES: [&str; 3] = ["sum", "fixed", "discount_percent"];

/// Taxonomy node kinds, mirroring the software module.
pub const TAXONOMY_KINDS: [&str; 2] = ["group", "category"];

/// May read the product catalogue, including the module offerings
/// list. Checked through the `role.check` hook; fail closed.
pub const PERM_PRODUCTS_READ: &str = "products:read";

/// May create products and their sub-resources (variants, tiers,
/// regional prices, taxonomy nodes, tags, files, versions, tax
/// rules, support tickets). Checked through the `role.check` hook;
/// fail closed.
pub const PERM_PRODUCTS_CREATE: &str = "products:create";

/// May update, delete, transition, list/delist, and otherwise
/// manage products and their sub-resources. Checked through the
/// `role.check` hook; fail closed.
pub const PERM_PRODUCTS_MANAGE: &str = "products:manage";

// Products

/// License terms a licensed product declares. The licensing module
/// reads these through `GET /api/v1/products/{id}/license-template`
/// when it issues a license for an order containing this product.
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

pub(crate) fn default_seats() -> i32 {
    1
}

pub(crate) fn default_download_limit() -> u32 {
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
    /// Mid-cycle plan-change policy: `prorate`, `no_prorate`, or
    /// `credit_only` (see `PRORATION_POLICIES`). Only meaningful
    /// for subscription products; the store and billing modules read
    /// it when a subscription changes plan mid-cycle.
    #[serde(default = "default_proration_policy")]
    pub proration_policy: String,
    /// Days of continued access after a failed renewal payment
    /// before the subscription is cancelled (0 means cancel as soon
    /// as dunning is exhausted). Only meaningful for subscription
    /// products.
    #[serde(default)]
    pub grace_period_days: u32,
    /// Maximum failed-payment (dunning) retries before the
    /// subscription is cancelled. Only meaningful for subscription
    /// products.
    #[serde(default = "default_dunning_max_retries")]
    pub dunning_max_retries: u32,
    /// Days between dunning retries. Only meaningful for
    /// subscription products.
    #[serde(default = "default_dunning_retry_interval_days")]
    pub dunning_retry_interval_days: u32,
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

pub(crate) fn default_bundle_rule() -> String {
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
    /// Subscription plan fields (see `Product`): proration policy,
    /// grace period in days, dunning retry count and interval.
    /// Non-default values are only accepted for subscription
    /// products.
    #[serde(default)]
    pub proration_policy: Option<String>,
    #[serde(default)]
    pub grace_period_days: Option<u32>,
    #[serde(default)]
    pub dunning_max_retries: Option<u32>,
    #[serde(default)]
    pub dunning_retry_interval_days: Option<u32>,
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

#[derive(Debug, Clone, Serialize)]
pub struct SubscriptionPlanView {
    pub product_id: String,
    pub pricing_model: String,
    pub billing_period: Option<String>,
    pub trial_days: u32,
    /// Base price in integer minor units.
    pub price_minor: i64,
    /// Renewal price in integer minor units (base price when unset).
    pub renewal_price_minor: i64,
    pub currency: String,
    pub proration_policy: String,
    pub grace_period_days: u32,
    pub dunning_max_retries: u32,
    pub dunning_retry_interval_days: u32,
}

pub(crate) fn default_pricing_model() -> String {
    "one_time".to_string()
}

pub(crate) fn default_proration_policy() -> String {
    "prorate".to_string()
}

pub(crate) fn default_dunning_max_retries() -> u32 {
    3
}

pub(crate) fn default_dunning_retry_interval_days() -> u32 {
    3
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
    pub proration_policy: Option<String>,
    #[serde(default)]
    pub grace_period_days: Option<u32>,
    #[serde(default)]
    pub dunning_max_retries: Option<u32>,
    #[serde(default)]
    pub dunning_retry_interval_days: Option<u32>,
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

// Variants and options

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

pub(crate) fn default_variant_status() -> String {
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

// Bundles

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

// Pricing: tiers, regional prices, usage prices, tax rules

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

pub(crate) fn default_tier_status() -> String {
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

// Inventory

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

// Product files and versions (references into the Software Manager)

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

// Taxonomy (groups and categories) and tags - same shapes as the
// software module, in this module's own namespace.

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

// Entitlement and support wiring payloads

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
