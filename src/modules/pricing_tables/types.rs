//! pricing tables - types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub(crate) const PERM_PRICING_READ: &str = "pricing_tables:read";

pub(crate) const PERM_PRICING_MANAGE: &str = "pricing_tables:manage";

pub(crate) const PERM_MARKETPLACE_READ: &str = "marketplace:read";

pub(crate) const PERM_MARKETPLACE_MANAGE: &str = "marketplace:manage";

pub(crate) const PERM_MARKETPLACE_VENDOR: &str = "marketplace:vendor";

/// Default commission rate (percent) when no vendor, category, or
/// global rule matches.
pub(crate) const DEFAULT_COMMISSION_RATE_PERCENT: f64 = 10.0;

/// Maximum length for free-text fields, so no unbounded string is
/// ever stored from a request body.
pub(crate) const MAX_TEXT_LEN: usize = 2000;

pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_URL_LEN: usize = 2048;

/// One reusable pricing table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingTable {
    pub id: String,
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub description: String,
    /// ISO 4217 currency code, uppercase, for example `USD`.
    pub currency: String,
    /// `grid`, `columns`, or `comparison`.
    pub layout: String,
    /// Theme accent colour as a `#rrggbb` hex string.
    pub theme_accent: String,
    /// `draft` or `published`.
    pub status: String,
    /// Yearly discount applied to the yearly cycle, percent 0..=100.
    pub yearly_discount_percent: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePricingTable {
    pub name: String,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub currency: Option<String>,
    pub layout: Option<String>,
    pub theme_accent: Option<String>,
    pub yearly_discount_percent: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePricingTable {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub currency: Option<String>,
    pub layout: Option<String>,
    pub theme_accent: Option<String>,
    pub yearly_discount_percent: Option<f64>,
}

/// One tier of a pricing table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingTier {
    pub id: String,
    pub table_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Price per month in minor units when the monthly cycle is
    /// selected. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub monthly_price: i64,
    /// Price per year in minor units when the yearly cycle is
    /// selected. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub yearly_price: i64,
    /// Call-to-action label, for example `Get started`.
    pub cta_label: String,
    /// Call-to-action target: relative path or `https://` URL only.
    pub cta_url: String,
    /// Exactly one tier per table is highlighted.
    pub highlighted: bool,
    pub sort_order: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePricingTier {
    pub name: String,
    pub description: Option<String>,
    #[serde(with = "unified_core::money::serde_major")]
    pub monthly_price: i64,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub yearly_price: Option<i64>,
    pub cta_label: Option<String>,
    pub cta_url: Option<String>,
    pub highlighted: Option<bool>,
    pub sort_order: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePricingTier {
    pub name: Option<String>,
    pub description: Option<String>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub monthly_price: Option<i64>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub yearly_price: Option<i64>,
    pub cta_label: Option<String>,
    pub cta_url: Option<String>,
    pub highlighted: Option<bool>,
    pub sort_order: Option<u32>,
}

/// One feature row of a tier: included features and limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierFeature {
    pub id: String,
    pub tier_id: String,
    pub feature_text: String,
    /// Whether the feature is included in this tier.
    pub included: bool,
    #[serde(default)]
    pub note: String,
    pub sort_order: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTierFeature {
    pub feature_text: String,
    pub included: Option<bool>,
    pub note: Option<String>,
    pub sort_order: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTierFeature {
    pub feature_text: Option<String>,
    pub included: Option<bool>,
    pub note: Option<String>,
    pub sort_order: Option<u32>,
}

/// One tier inside the render payload, with its features and the
/// yearly-cycle prices computed from the table discount rule.
#[derive(Debug, Clone, Serialize)]
pub struct TierRender {
    #[serde(flatten)]
    pub tier: PricingTier,
    pub features: Vec<TierFeature>,
    /// Monthly price * 12 with the table yearly discount applied,
    /// in minor units. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub yearly_total_discounted: i64,
    /// Discounted yearly total expressed as a monthly equivalent,
    /// in minor units. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub yearly_monthly_equivalent: i64,
    /// Amount saved per year by choosing the yearly cycle, in minor
    /// units. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub yearly_savings: i64,
}

/// Structured render data for a table: what a page needs to render
/// it, with no HTML. The billing-cycle toggle is client state; both
/// cycle prices are present so the page can switch without a second
/// request.
#[derive(Debug, Clone, Serialize)]
pub struct PricingTableRender {
    pub table: PricingTable,
    pub tiers: Vec<TierRender>,
    /// `monthly` or `yearly`: which cycle the table defaults to.
    pub default_cycle: String,
}

/// One marketplace vendor. `user_id` is the owning user; `company_id`
/// is the optional company record the vendor sells for, used to match
/// store order lines to this vendor when no product approval link
/// exists. `company_name` is the display name shown on the storefront.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vendor {
    pub id: String,
    pub user_id: String,
    #[serde(default)]
    pub company_id: Option<String>,
    pub company_name: String,
    pub slug: String,
    pub email: String,
    /// `pending`, `approved`, `suspended`, or `rejected`.
    pub status: String,
    #[serde(default)]
    pub application_notes: String,
    #[serde(default)]
    pub review_reason: String,
    #[serde(default)]
    pub reviewed_by: Option<String>,
    #[serde(default)]
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateVendor {
    pub user_id: Option<String>,
    pub company_id: Option<String>,
    pub company_name: String,
    pub slug: Option<String>,
    pub email: String,
    pub application_notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateVendor {
    pub company_id: Option<String>,
    pub company_name: Option<String>,
    pub slug: Option<String>,
    pub email: Option<String>,
    pub application_notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReviewInput {
    pub reason: Option<String>,
}

/// One commission rule. `scope` is `vendor`, `category`, or
/// `global`; `scope_value` holds the vendor id or the category slug
/// and is empty for a global rule. At most one rule per
/// (scope, scope_value) pair may exist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommissionRule {
    pub id: String,
    /// `vendor`, `category`, or `global`.
    pub scope: String,
    #[serde(default)]
    pub scope_value: String,
    /// Commission rate, percent 0..=100.
    pub rate_percent: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCommissionRule {
    pub scope: String,
    pub scope_value: Option<String>,
    pub rate_percent: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateCommissionRule {
    pub scope: Option<String>,
    pub scope_value: Option<String>,
    pub rate_percent: Option<f64>,
}

/// Approval state for one vendor product. The Store module is the
/// source of truth for the product itself; this record holds only
/// the approval state, the vendor link, and the product category
/// captured at submission time (used for category commission rates).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductApproval {
    pub id: String,
    pub product_id: String,
    pub vendor_id: String,
    #[serde(default)]
    pub category: Option<String>,
    /// `pending`, `approved`, or `rejected`.
    pub status: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub reviewed_by: Option<String>,
    #[serde(default)]
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubmitProduct {
    pub vendor_id: String,
    pub category: Option<String>,
}

/// One computed commission ledger entry. Created exactly once per
/// (order, product, vendor) by the paid-order hook handlers; lines
/// for the same product in one order are aggregated into that one
/// entry, never recomputed from stored totals, and never written
/// by hand. `paid_amount` records how much of the commission a
/// payout has already settled: 0 until settlement starts, equal to
/// `commission_amount` once fully paid. A commission stays
/// `pending` while `paid_amount` is smaller than
/// `commission_amount`, so a partial settlement never counts as
/// paid in full and never leaves paid money in the pending balance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commission {
    pub id: String,
    pub order_id: String,
    pub vendor_id: String,
    pub product_id: String,
    /// Order line total the commission was computed from, in minor
    /// units. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub order_total: i64,
    pub rate_percent: f64,
    /// Commission amount in minor units. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub commission_amount: i64,
    /// Amount already settled by payouts (partial settlement), in
    /// minor units. JSON carries major units.
    #[serde(default, with = "unified_core::money::serde_major")]
    pub paid_amount: i64,
    pub currency: String,
    /// `pending`, `paid`, or `reversed`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One vendor payout request. Billing is notified through
/// `marketplace.payout_requested`; the payout itself is paid out
/// through billing, and the invoice id is attached when billing
/// emits `invoice.created` for it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payout {
    pub id: String,
    pub vendor_id: String,
    /// Payout amount in minor units. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
    pub currency: String,
    /// `requested`, `approved`, `paid`, `rejected`, or `cancelled`.
    pub status: String,
    #[serde(default)]
    pub invoice_id: Option<String>,
    #[serde(default)]
    pub reference: String,
    pub requested_by: String,
    #[serde(default)]
    pub reviewed_by: Option<String>,
    #[serde(default)]
    pub review_reason: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePayout {
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarkPaidInput {
    pub reference: Option<String>,
}

/// Aggregated vendor dashboard data, computed live from the
/// commission ledger, payouts, and product approvals. Nothing here
/// is stored: every figure is derived on read.
#[derive(Debug, Clone, Serialize)]
pub struct VendorDashboard {
    pub vendor_id: String,
    pub vendor_status: String,
    pub total_orders: u64,
    /// All money figures are in minor units; JSON carries major
    /// units.
    #[serde(with = "unified_core::money::serde_major")]
    pub total_sales: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub total_commission: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub pending_commission: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub paid_commission: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub reversed_commission: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub payouts_requested: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub payouts_approved: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub payouts_paid: i64,
    pub payouts_pending_count: u64,
    pub approved_products: u64,
    pub pending_products: u64,
    pub rejected_products: u64,
    /// Invoice references billing attached to this vendor's payouts.
    pub invoice_ids: Vec<String>,
}

/// Public storefront data for one approved vendor: the vendor record
/// plus the product ids whose approval is `approved`. Product
/// details (name, price, images) are resolved by the caller through
/// the Store module, which owns them.
#[derive(Debug, Clone, Serialize)]
pub struct MarketplaceStorefront {
    pub vendor: Vendor,
    pub approved_product_ids: Vec<String>,
    pub product_count: u64,
}
