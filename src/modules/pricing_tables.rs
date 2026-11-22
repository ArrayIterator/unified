// Pricing Tables + Marketplace Builder module.
//
// Two builders in one sellable module (`available_as_product`, category
// `commerce`):
//
//   1. Pricing tables — an admin builds reusable pricing tables made of
//      tiers with monthly/yearly prices, per-tier feature lists, a
//      billing-cycle toggle (monthly/yearly with a yearly discount
//      rule), and table settings (currency, layout, theme accent).
//      A table is rendered by pages as structured JSON through the
//      render-data endpoint; this module never renders HTML.
//   2. Marketplace builder — on top of the Site Builder and the Store,
//      an admin runs a marketplace: vendor onboarding, vendor product
//      approval, per-vendor / per-category / global commission rules
//      computed from store order events, vendor dashboards aggregated
//      from those computed records, and vendor payouts requested
//      through billing.
//
// Invariants enforced on every write:
//   - Every price is >= 0; every rate and discount is 0..=100.
//   - Exactly one highlighted tier per table while the table has any
//     tier: creating the first tier highlights it; highlighting a tier
//     clears the flag on every other tier of the same table; clearing
//     the only highlighted tier is rejected; deleting the highlighted
//     tier promotes the lowest sort-order remaining tier.
//   - CTA URLs are relative paths (`/path`) or `https://` URLs only.
//     `javascript:`, `data:`, `vbscript:`, protocol-relative (`//host`)
//     and plain `http://` URLs are rejected (no open redirects).
//
// Money: amounts are `f64` in memory (the same convention as the
// Store module) and `NUMERIC(19,4)` in the database migration
// (`migrations/2026-11-22-000041_pricing_marketplace`). Commission
// amounts are rounded to 4 decimal places when computed.
//
// Source of truth: the Store module owns products and orders. This
// module stores only the approval state + vendor link for a product
// (`marketplace_product_approvals`), the computed commission ledger
// (`marketplace_commissions`, idempotent per order + product), and
// payout records. Vendor dashboard figures are aggregated live from
// those records, never stored twice.
//
// Routes (all under `/api/v1/pricing-tables`):
//   Tables:
//     GET    /tables                                  - paginated tables
//     POST   /tables                                  - create a table
//     GET    /tables/{id}                             - one table
//     PUT    /tables/{id}                             - update a table
//     DELETE /tables/{id}                             - delete a table + tiers + features
//     POST   /tables/{id}/publish                     - draft -> published
//     POST   /tables/{id}/unpublish                   - published -> draft
//     GET    /tables/{id}/render                      - structured render data
//     GET    /tables/{id}/embed                       - table id + render URL + snippet
//   Tiers:
//     GET    /tables/{id}/tiers                       - tiers in sort order
//     POST   /tables/{id}/tiers                       - create a tier
//     GET    /tables/{id}/tiers/{tier_id}             - one tier
//     PUT    /tables/{id}/tiers/{tier_id}             - update a tier
//     DELETE /tables/{id}/tiers/{tier_id}             - delete a tier + features
//   Tier features:
//     GET    /tables/{id}/tiers/{tier_id}/features    - features in sort order
//     POST   /tables/{id}/tiers/{tier_id}/features    - create a feature row
//     GET    /tables/{id}/tiers/{tier_id}/features/{feature_id}
//     PUT    /tables/{id}/tiers/{tier_id}/features/{feature_id}
//     DELETE /tables/{id}/tiers/{tier_id}/features/{feature_id}
//   Marketplace vendors:
//     GET    /marketplace/vendors                      - paginated vendors
//     POST   /marketplace/vendors                      - vendor application
//     GET    /marketplace/vendors/{id}                 - one vendor
//     PUT    /marketplace/vendors/{id}                 - update vendor (admin)
//     POST   /marketplace/vendors/{id}/approve         - pending -> approved
//     POST   /marketplace/vendors/{id}/reject          - pending -> rejected
//     POST   /marketplace/vendors/{id}/suspend         - approved -> suspended
//     POST   /marketplace/vendors/{id}/reactivate      - suspended -> approved
//     GET    /marketplace/vendors/{id}/dashboard       - aggregated sales/orders/payouts
//     GET    /marketplace/vendors/{id}/commissions     - vendor commission ledger
//     GET    /marketplace/vendors/{id}/payouts         - vendor payouts
//     POST   /marketplace/vendors/{id}/payouts         - request a payout
//   Marketplace storefront:
//     GET    /marketplace/storefronts/{slug}           - public storefront data
//   Commission rules:
//     GET    /marketplace/commission-rules             - paginated rules
//     POST   /marketplace/commission-rules             - create a rule
//     GET    /marketplace/commission-rules/{id}        - one rule
//     PUT    /marketplace/commission-rules/{id}        - update a rule
//     DELETE /marketplace/commission-rules/{id}        - delete a rule
//   Commissions:
//     GET    /marketplace/commissions                  - paginated ledger
//     GET    /marketplace/commissions/{id}             - one commission
//   Product approvals:
//     POST   /marketplace/products/{product_id}/submit - vendor submits a product
//     GET    /marketplace/product-approvals            - paginated approvals
//     GET    /marketplace/product-approvals/{id}       - one approval
//     POST   /marketplace/product-approvals/{id}/approve
//     POST   /marketplace/product-approvals/{id}/reject
//   Payouts (admin):
//     GET    /marketplace/payouts                      - paginated payouts
//     GET    /marketplace/payouts/{id}                 - one payout
//     POST   /marketplace/payouts/{id}/approve         - requested -> approved
//     POST   /marketplace/payouts/{id}/reject          - requested -> rejected
//     POST   /marketplace/payouts/{id}/mark-paid       - approved -> paid
//
// Events emitted:
//   `pricing_table.created`, `pricing_table.published`,
//   `pricing_table.unpublished`, `pricing_table.deleted`,
//   `marketplace.vendor_applied`, `marketplace.vendor_approved`,
//   `marketplace.vendor_rejected`, `marketplace.vendor_suspended`,
//   `marketplace.vendor_reactivated`,
//   `marketplace.product_approval_requested`,
//   `marketplace.product_approved`, `marketplace.product_rejected`,
//   `marketplace.payout_requested`, `marketplace.payout_approved`,
//   `marketplace.payout_rejected`, `marketplace.payout_paid`,
//   `marketplace.commission_computed` (also for reversals, with
//   `"status": "reversed"`), `marketplace.commission_rule_created`,
//   `marketplace.commission_rule_updated`,
//   `marketplace.commission_rule_deleted`.
//
// Events consumed:
//   `order.paid` and `store.order.completed` — the exact event names
//   the Store module emits when an order is paid / completed
//   (there is no `store.order_paid` event in the Store module; the
//   briefed name was corrected to the real ones). Each paid order
//   line is resolved to a vendor through the product approval link,
//   falling back to a vendor whose `company_id` matches the order
//   line's `company_id`, and a commission is computed. The handler
//   is idempotent: a commission already recorded for the same
//   order + product is never computed twice, so an order that emits
//   both events yields one ledger entry per line.
//   `store.order.refunded` — pending commissions for the order are
//   reversed (status `reversed`). Commissions already paid out are
//   left untouched: clawing back a paid payout is a billing action,
//   not an automatic reversal, and the reversal event carries the
//   paid entries so billing can reconcile them.
//   `invoice.created` — the exact event name the Billing module
//   emits (there is no `billing.invoice_created` event; the briefed
//   name was corrected to the real one). When the payload names a
//   payout id, the invoice id is attached to that payout for
//   reconciliation; otherwise the invoice reference is kept in the
//   in-memory reconciliation log the dashboard reports.
//
// Events answered (hook requests from other modules):
//   `pricing_table.render_requested` — `{"table_id"}` answers with
//   the render data for a published table.
//   `marketplace.vendor_check` — `{"user_id"}` answers with the
//   vendor id and status for that user, when one exists.
//   `marketplace.commission_quote` — `{"amount", "vendor_id"?,
//   "category"?}` answers with the applicable rate and the computed
//   commission amount, without writing anything.
//   `marketplace.product_approval_status` — `{"product_id"}` answers
//   with the approval status for that product, when one exists.
//
// Capability checks: every route except the two public read paths
// requires the caller headers `X-User-Id` + `X-Role` and a
// permission checked fail-closed through the `role.check` hook
// (same shape as the AI module). The public paths are
// `GET /tables/{id}/render` and
// `GET /marketplace/storefronts/{slug}`, which serve published
// content only — a draft table or a non-approved vendor answers
// 404 there, exactly like a published page. All writes require
// `pricing_tables:manage` or `marketplace:manage`; vendor
// self-service (apply, own dashboard, own payouts, own product
// submissions) requires `marketplace:vendor` and ownership of the
// vendor record.

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

const PERM_PRICING_READ: &str = "pricing_tables:read";
const PERM_PRICING_MANAGE: &str = "pricing_tables:manage";
const PERM_MARKETPLACE_READ: &str = "marketplace:read";
const PERM_MARKETPLACE_MANAGE: &str = "marketplace:manage";
const PERM_MARKETPLACE_VENDOR: &str = "marketplace:vendor";

/// Default commission rate (percent) when no vendor, category, or
/// global rule matches.
const DEFAULT_COMMISSION_RATE_PERCENT: f64 = 10.0;

/// Maximum length for free-text fields, so no unbounded string is
/// ever stored from a request body.
const MAX_TEXT_LEN: usize = 2000;
const MAX_NAME_LEN: usize = 200;
const MAX_URL_LEN: usize = 2048;

// ---------------------------------------------------------------------------
// Types: pricing tables
// ---------------------------------------------------------------------------

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
    /// Price per month when the monthly cycle is selected.
    pub monthly_price: f64,
    /// Price per year when the yearly cycle is selected.
    pub yearly_price: f64,
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
    pub monthly_price: f64,
    pub yearly_price: Option<f64>,
    pub cta_label: Option<String>,
    pub cta_url: Option<String>,
    pub highlighted: Option<bool>,
    pub sort_order: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePricingTier {
    pub name: Option<String>,
    pub description: Option<String>,
    pub monthly_price: Option<f64>,
    pub yearly_price: Option<f64>,
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
    /// Monthly price * 12 with the table yearly discount applied.
    pub yearly_total_discounted: f64,
    /// Discounted yearly total expressed as a monthly equivalent.
    pub yearly_monthly_equivalent: f64,
    /// Amount saved per year by choosing the yearly cycle.
    pub yearly_savings: f64,
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

// ---------------------------------------------------------------------------
// Types: marketplace
// ---------------------------------------------------------------------------

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
/// (order, product) pair by the paid-order hook handlers; never
/// recomputed from stored totals, and never written by hand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commission {
    pub id: String,
    pub order_id: String,
    pub vendor_id: String,
    pub product_id: String,
    /// Order line total the commission was computed from.
    pub order_total: f64,
    pub rate_percent: f64,
    pub commission_amount: f64,
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
    pub amount: f64,
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
    pub amount: f64,
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
    pub total_sales: f64,
    pub total_commission: f64,
    pub pending_commission: f64,
    pub paid_commission: f64,
    pub reversed_commission: f64,
    pub payouts_requested: f64,
    pub payouts_approved: f64,
    pub payouts_paid: f64,
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

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct PricingTablesState {
    hooks: RwLock<Arc<HookBus>>,
    tables: DashMap<String, PricingTable>,
    tiers: DashMap<String, PricingTier>,
    features: DashMap<String, TierFeature>,
    vendors: DashMap<String, Vendor>,
    commission_rules: DashMap<String, CommissionRule>,
    product_approvals: DashMap<String, ProductApproval>,
    commissions: DashMap<String, Commission>,
    payouts: DashMap<String, Payout>,
    /// Invoice references seen on `invoice.created`, keyed by the
    /// invoice id, kept for payout reconciliation on the dashboard.
    invoice_refs: DashMap<String, Value>,
}

impl PricingTablesState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            tables: DashMap::new(),
            tiers: DashMap::new(),
            features: DashMap::new(),
            vendors: DashMap::new(),
            commission_rules: DashMap::new(),
            product_approvals: DashMap::new(),
            commissions: DashMap::new(),
            payouts: DashMap::new(),
            invoice_refs: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Tiers of one table, in sort order (ties by creation time).
    fn tiers_of(&self, table_id: &str) -> Vec<PricingTier> {
        let mut tiers: Vec<PricingTier> = self
            .tiers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|tier| tier.table_id == table_id)
            .collect();
        tiers.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(a.created_at.cmp(&b.created_at))
        });
        tiers
    }

    /// Features of one tier, in sort order (ties by creation time).
    fn features_of(&self, tier_id: &str) -> Vec<TierFeature> {
        let mut features: Vec<TierFeature> = self
            .features
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|feature| feature.tier_id == tier_id)
            .collect();
        features.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(a.created_at.cmp(&b.created_at))
        });
        features
    }

    /// Clear the highlighted flag on every tier of a table except
    /// `except_tier_id`. This is the single place that enforces the
    /// exactly-one-highlighted-tier invariant on write.
    fn clear_other_highlights(&self, table_id: &str, except_tier_id: &str) {
        for mut entry in self.tiers.iter_mut() {
            if entry.value().table_id == table_id
                && entry.key() != except_tier_id
                && entry.value().highlighted
            {
                entry.value_mut().highlighted = false;
                entry.value_mut().updated_at = now();
            }
        }
    }

    /// After the highlighted tier of a table is deleted, promote the
    /// lowest sort-order remaining tier so the table keeps exactly
    /// one highlighted tier.
    fn promote_next_highlight(&self, table_id: &str) {
        let tiers = self.tiers_of(table_id);
        if tiers.iter().any(|tier| tier.highlighted) || tiers.is_empty() {
            return;
        }
        if let Some(first) = tiers.first() {
            if let Some(mut entry) = self.tiers.get_mut(&first.id) {
                entry.value_mut().highlighted = true;
                entry.value_mut().updated_at = now();
            }
        }
    }

    /// Resolve the commission rate for a vendor and an optional
    /// product category: vendor rule wins, then category rule, then
    /// the global rule, then the built-in default.
    fn commission_rate_for(&self, vendor_id: &str, category: Option<&str>) -> f64 {
        let by_scope = |scope: &str, value: &str| {
            self.commission_rules
                .iter()
                .find(|entry| entry.value().scope == scope && entry.value().scope_value == value)
                .map(|entry| entry.value().rate_percent)
        };
        if let Some(rate) = by_scope("vendor", vendor_id) {
            return rate;
        }
        if let Some(category) = category {
            if let Some(rate) = by_scope("category", category) {
                return rate;
            }
        }
        if let Some(rate) = by_scope("global", "") {
            return rate;
        }
        DEFAULT_COMMISSION_RATE_PERCENT
    }

    /// Resolve the vendor that owns a store order line: the approved
    /// product approval link first, then a vendor whose `company_id`
    /// matches the line's `company_id`. Returns the vendor id and the
    /// product category captured on the approval, when known.
    fn vendor_for_order_line(
        &self,
        product_id: &str,
        company_id: Option<&str>,
    ) -> Option<(String, Option<String>)> {
        if let Some(approval) = self
            .product_approvals
            .iter()
            .find(|entry| entry.value().product_id == product_id)
            .map(|entry| entry.value().clone())
        {
            if approval.status == "approved" {
                return Some((approval.vendor_id, approval.category));
            }
            // A product whose approval is pending or rejected earns no
            // commission for its vendor: the sale is not a marketplace
            // sale until approval.
            return None;
        }
        let company_id = company_id?;
        self.vendors
            .iter()
            .find(|entry| {
                entry.value().status == "approved"
                    && entry
                        .value()
                        .company_id
                        .as_deref()
                        .is_some_and(|id| id == company_id)
            })
            .map(|entry| (entry.key().clone(), None))
    }

    /// Build the structured render data for one table.
    fn render_data(&self, table: &PricingTable) -> PricingTableRender {
        let discount = table.yearly_discount_percent / 100.0;
        let tiers: Vec<TierRender> = self
            .tiers_of(&table.id)
            .into_iter()
            .map(|tier| {
                let yearly_total_discounted =
                    round_money(tier.monthly_price * 12.0 * (1.0 - discount));
                let yearly_monthly_equivalent = round_money(yearly_total_discounted / 12.0);
                let yearly_savings =
                    round_money(tier.monthly_price * 12.0 - yearly_total_discounted);
                TierRender {
                    features: self.features_of(&tier.id),
                    yearly_total_discounted,
                    yearly_monthly_equivalent,
                    yearly_savings,
                    tier,
                }
            })
            .collect();
        PricingTableRender {
            table: table.clone(),
            tiers,
            default_cycle: "monthly".to_string(),
        }
    }

    /// Aggregate the dashboard for one vendor from the commission
    /// ledger, payouts, and product approvals.
    fn dashboard_for(&self, vendor: &Vendor) -> VendorDashboard {
        let vendor_id = vendor.id.as_str();
        let mut total_orders_set: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut total_sales = 0.0;
        let mut total_commission = 0.0;
        let mut pending_commission = 0.0;
        let mut paid_commission = 0.0;
        let mut reversed_commission = 0.0;
        for entry in self.commissions.iter() {
            let commission = entry.value();
            if commission.vendor_id != vendor_id {
                continue;
            }
            total_orders_set.insert(commission.order_id.clone());
            total_sales += commission.order_total;
            total_commission += commission.commission_amount;
            match commission.status.as_str() {
                "pending" => pending_commission += commission.commission_amount,
                "paid" => paid_commission += commission.commission_amount,
                "reversed" => reversed_commission += commission.commission_amount,
                _ => {}
            }
        }
        let mut payouts_requested = 0.0;
        let mut payouts_approved = 0.0;
        let mut payouts_paid = 0.0;
        let mut payouts_pending_count = 0u64;
        let mut invoice_ids: Vec<String> = Vec::new();
        for entry in self.payouts.iter() {
            let payout = entry.value();
            if payout.vendor_id != vendor_id {
                continue;
            }
            match payout.status.as_str() {
                "requested" => {
                    payouts_requested += payout.amount;
                    payouts_pending_count += 1;
                }
                "approved" => payouts_approved += payout.amount,
                "paid" => payouts_paid += payout.amount,
                _ => {}
            }
            if let Some(invoice_id) = &payout.invoice_id {
                invoice_ids.push(invoice_id.clone());
            }
        }
        invoice_ids.sort();
        invoice_ids.dedup();
        let mut approved_products = 0u64;
        let mut pending_products = 0u64;
        let mut rejected_products = 0u64;
        for entry in self.product_approvals.iter() {
            let approval = entry.value();
            if approval.vendor_id != vendor_id {
                continue;
            }
            match approval.status.as_str() {
                "approved" => approved_products += 1,
                "pending" => pending_products += 1,
                "rejected" => rejected_products += 1,
                _ => {}
            }
        }
        VendorDashboard {
            vendor_id: vendor.id.clone(),
            vendor_status: vendor.status.clone(),
            total_orders: total_orders_set.len() as u64,
            total_sales: round_money(total_sales),
            total_commission: round_money(total_commission),
            pending_commission: round_money(pending_commission),
            paid_commission: round_money(paid_commission),
            reversed_commission: round_money(reversed_commission),
            payouts_requested: round_money(payouts_requested),
            payouts_approved: round_money(payouts_approved),
            payouts_paid: round_money(payouts_paid),
            payouts_pending_count,
            approved_products,
            pending_products,
            rejected_products,
            invoice_ids,
        }
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

/// Round a money amount to 4 decimal places (NUMERIC(19,4)).
fn round_money(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
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

/// `My Pricing` -> `my-pricing`.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

fn valid_email(email: &str) -> bool {
    let mut parts = email.split('@');
    matches!((parts.next(), parts.next(), parts.next()), (Some(local), Some(domain), None)
        if !local.is_empty() && !domain.is_empty() && domain.contains('.'))
}

/// A CTA URL is a relative path (`/pricing`) or an `https://` URL.
/// Rejects `javascript:`, `data:`, `vbscript:`, protocol-relative
/// (`//host`), plain `http://`, and anything with whitespace or
/// control characters — no open redirects, no script URLs.
fn valid_cta_url(url: &str) -> bool {
    if url.is_empty() || url.len() > MAX_URL_LEN {
        return false;
    }
    if url.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        return false;
    }
    let lower = url.to_lowercase();
    if lower.starts_with("javascript:")
        || lower.starts_with("data:")
        || lower.starts_with("vbscript:")
    {
        return false;
    }
    if let Some(rest) = url.strip_prefix('/') {
        // A single leading slash is a relative path; `//host` is a
        // protocol-relative URL to another host and is rejected.
        return !rest.starts_with('/');
    }
    if let Some(rest) = lower.strip_prefix("https://") {
        // A host must follow the scheme; no userinfo tricks, no
        // trailing-only slash games beyond a normal path.
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        return !host.is_empty() && !host.contains('@') && !host.contains('\\');
    }
    false
}

/// `#0B6B3A` style hex colour, exactly 7 characters.
fn valid_hex_color(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value[1..].chars().all(|ch| ch.is_ascii_hexdigit())
}

/// ISO 4217 currency code: exactly three uppercase letters.
fn valid_currency(value: &str) -> bool {
    value.len() == 3 && value.chars().all(|ch| ch.is_ascii_uppercase())
}

fn valid_layout(layout: &str) -> bool {
    matches!(layout, "grid" | "columns" | "comparison")
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn optional_text(value: Option<String>, max: usize) -> UnifiedResult<String> {
    match value {
        Some(text) => require_text(&text, "value", max).map_err(|_| {
            UnifiedError::bad_request(format!("value must be at most {max} characters"))
        }),
        None => Ok(String::new()),
    }
}

fn valid_price(price: f64, field: &str) -> UnifiedResult<()> {
    if !price.is_finite() || price < 0.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite number >= 0"
        )));
    }
    Ok(())
}

fn valid_percent(value: f64, field: &str) -> UnifiedResult<()> {
    if !value.is_finite() || !(0.0..=100.0).contains(&value) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between 0 and 100"
        )));
    }
    Ok(())
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>, debug: bool) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Caller identity and capability checks
// ---------------------------------------------------------------------------

/// The caller, from the headers every capability-checked module in
/// this codebase uses: `X-User-Id` identifies the actor, `X-Role`
/// carries the role slug or id the capability check runs against.
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

/// A vendor acting on their own record: the caller must hold
/// `marketplace:vendor` and own the vendor record, or hold
/// `marketplace:manage` (admin).
fn require_vendor_self_or_admin(
    ctx: &AppContext,
    caller: &Caller,
    vendor: &Vendor,
) -> UnifiedResult<()> {
    if check_capability(ctx, &caller.role, PERM_MARKETPLACE_MANAGE) {
        return Ok(());
    }
    if check_capability(ctx, &caller.role, PERM_MARKETPLACE_VENDOR)
        && caller.actor == vendor.user_id
    {
        return Ok(());
    }
    Err(UnifiedError::forbidden(
        "Only the owning vendor or a marketplace administrator may perform this action",
    ))
}

// ---------------------------------------------------------------------------
// Handlers: pricing tables
// ---------------------------------------------------------------------------

async fn list_tables(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_READ)?;
        let params = query.into_inner();
        let mut items: Vec<PricingTable> = state
            .tables
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|table| {
                matches_search(&format!("{} {} {}", table.name, table.slug, table.description), &params)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| table.status == *status)
                    && params
                        .filters
                        .get("currency")
                        .is_none_or(|currency| table.currency == *currency)
            })
            .collect();
        let sort = params.allowed_sort(&["name", "slug", "currency", "status", "created_at"], "created_at");
        match sort.as_str() {
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
            "slug" => items.sort_by(|a, b| a.slug.cmp(&b.slug)),
            "currency" => items.sort_by(|a, b| a.currency.cmp(&b.currency)),
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn create_table(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    body: web::Json<CreatePricingTable>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTable> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let slug = match input.slug {
            Some(slug) if !slug.trim().is_empty() => slug.trim().to_lowercase(),
            _ => slugify(&name),
        };
        if !valid_slug(&slug) {
            return Err(UnifiedError::bad_request(
                "slug must be lowercase letters, digits, and hyphens",
            ));
        }
        if state
            .tables
            .iter()
            .any(|entry| entry.value().slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "A pricing table with slug \"{slug}\" already exists"
            )));
        }
        let currency = input.currency.unwrap_or_else(|| "USD".to_string());
        if !valid_currency(&currency) {
            return Err(UnifiedError::bad_request(
                "currency must be a 3-letter uppercase ISO 4217 code",
            ));
        }
        let layout = input.layout.unwrap_or_else(|| "grid".to_string());
        if !valid_layout(&layout) {
            return Err(UnifiedError::bad_request(
                "layout must be one of: grid, columns, comparison",
            ));
        }
        let theme_accent = input.theme_accent.unwrap_or_else(|| "#0B6B3A".to_string());
        if !valid_hex_color(&theme_accent) {
            return Err(UnifiedError::bad_request(
                "theme_accent must be a #rrggbb hex colour",
            ));
        }
        let yearly_discount_percent = input.yearly_discount_percent.unwrap_or(0.0);
        valid_percent(yearly_discount_percent, "yearly_discount_percent")?;
        let description = optional_text(input.description, MAX_TEXT_LEN)?;
        let table = PricingTable {
            id: new_id(),
            name,
            slug,
            description,
            currency,
            layout,
            theme_accent,
            status: "draft".to_string(),
            yearly_discount_percent,
            created_at: now(),
            updated_at: now(),
        };
        state.tables.insert(table.id.clone(), table.clone());
        let value = serde_json::to_value(&table).unwrap_or(Value::Null);
        state.emit("pricing_table.created", value);
        Ok(table)
    })();
    respond_created(result, debug)
}

async fn get_table(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTable> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_READ)?;
        state
            .tables
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing table not found"))
    })();
    json::respond(result, debug)
}

async fn update_table(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<UpdatePricingTable>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTable> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        if let Some(slug) = &input.slug {
            if !slug.trim().is_empty() && !valid_slug(&slug.trim().to_lowercase()) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and hyphens",
                ));
            }
            if !slug.trim().is_empty()
                && state.tables.iter().any(|entry| {
                    entry.key() != path.as_str() && entry.value().slug == slug.trim().to_lowercase()
                })
            {
                return Err(UnifiedError::conflict(format!(
                    "A pricing table with slug \"{}\" already exists",
                    slug.trim().to_lowercase()
                )));
            }
        }
        if let Some(currency) = &input.currency {
            if !valid_currency(currency) {
                return Err(UnifiedError::bad_request(
                    "currency must be a 3-letter uppercase ISO 4217 code",
                ));
            }
        }
        if let Some(layout) = &input.layout {
            if !valid_layout(layout) {
                return Err(UnifiedError::bad_request(
                    "layout must be one of: grid, columns, comparison",
                ));
            }
        }
        if let Some(theme_accent) = &input.theme_accent {
            if !valid_hex_color(theme_accent) {
                return Err(UnifiedError::bad_request(
                    "theme_accent must be a #rrggbb hex colour",
                ));
            }
        }
        if let Some(discount) = input.yearly_discount_percent {
            valid_percent(discount, "yearly_discount_percent")?;
        }
        if let Some(description) = &input.description {
            if description.len() > MAX_TEXT_LEN {
                return Err(UnifiedError::bad_request(
                    "description must be at most 2000 characters",
                ));
            }
        }
        let mut table = state
            .tables
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Pricing table not found"))?;
        if let Some(name) = input.name {
            table.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(slug) = input.slug {
            if !slug.trim().is_empty() {
                table.slug = slug.trim().to_lowercase();
            }
        }
        if let Some(description) = input.description {
            table.description = description;
        }
        if let Some(currency) = input.currency {
            table.currency = currency;
        }
        if let Some(layout) = input.layout {
            table.layout = layout;
        }
        if let Some(theme_accent) = input.theme_accent {
            table.theme_accent = theme_accent;
        }
        if let Some(discount) = input.yearly_discount_percent {
            table.yearly_discount_percent = discount;
        }
        table.updated_at = now();
        Ok(table.clone())
    })();
    json::respond(result, debug)
}

async fn delete_table(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let table_id = path.as_str().to_string();
        let table = state
            .tables
            .remove(&table_id)
            .map(|(_, table)| table)
            .ok_or_else(|| UnifiedError::not_found("Pricing table not found"))?;
        // Tiers and their features belong to the table; they go with it.
        let tier_ids: Vec<String> = state
            .tiers
            .iter()
            .filter(|entry| entry.value().table_id == table_id)
            .map(|entry| entry.key().clone())
            .collect();
        for tier_id in &tier_ids {
            state.tiers.remove(tier_id);
            state
                .features
                .retain(|_, feature| feature.tier_id != *tier_id);
        }
        state.emit(
            "pricing_table.deleted",
            json!({ "table_id": table.id, "slug": table.slug }),
        );
        Ok(json!({ "deleted": true, "id": table_id }))
    })();
    json::respond(result, debug)
}

async fn publish_table(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTable> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let mut table = state
            .tables
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Pricing table not found"))?;
        if state.tiers_of(&table.id).is_empty() {
            return Err(UnifiedError::unprocessable(
                "Cannot publish a pricing table without at least one tier",
            ));
        }
        table.status = "published".to_string();
        table.updated_at = now();
        let updated = table.clone();
        drop(table);
        let value = serde_json::to_value(&updated).unwrap_or(Value::Null);
        state.emit("pricing_table.published", value);
        Ok(updated)
    })();
    json::respond(result, debug)
}

async fn unpublish_table(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTable> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let mut table = state
            .tables
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Pricing table not found"))?;
        table.status = "draft".to_string();
        table.updated_at = now();
        let updated = table.clone();
        drop(table);
        let value = serde_json::to_value(&updated).unwrap_or(Value::Null);
        state.emit("pricing_table.unpublished", value);
        Ok(updated)
    })();
    json::respond(result, debug)
}

/// Structured render data for a table. Public for published tables
/// (a page renders published content without an admin session);
/// draft tables answer 404 unless the caller holds
/// `pricing_tables:read`.
async fn render_table(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTableRender> = (|| {
        let table = state
            .tables
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing table not found"))?;
        if table.status != "published" {
            // Drafts are admin-only; a missing permission and a
            // missing table both answer 404 so draft ids are not
            // enumerable through this public endpoint.
            let allowed = caller(&req)
                .map(|caller| check_capability(&ctx, &caller.role, PERM_PRICING_READ))
                .unwrap_or(false);
            if !allowed {
                return Err(UnifiedError::not_found("Pricing table not found"));
            }
        }
        Ok(state.render_data(&table))
    })();
    json::respond(result, debug)
}

/// Embed snippet data: the table id and the render URL a page uses
/// to fetch the structured data, plus a placeholder snippet the
/// embedder can drop into a page.
async fn embed_table(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_READ)?;
        let table = state
            .tables
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing table not found"))?;
        let render_url = format!("/api/v1/pricing-tables/tables/{}/render", table.id);
        Ok(json!({
            "table_id": table.id,
            "slug": table.slug,
            "status": table.status,
            "render_url": render_url,
            "snippet": format!(
                "<div data-pricing-table=\"{}\" data-render-url=\"{}\"></div>",
                table.id, render_url
            ),
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: pricing tiers
// ---------------------------------------------------------------------------

async fn list_tiers(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_READ)?;
        let table_id = path.as_str().to_string();
        if !state.tables.contains_key(table_id.as_str()) {
            return Err(UnifiedError::not_found("Pricing table not found"));
        }
        let params = query.into_inner();
        let items: Vec<PricingTier> = state
            .tiers_of(&table_id)
            .into_iter()
            .filter(|tier| {
                matches_search(&format!("{} {}", tier.name, tier.description), &params)
                    && params
                        .filters
                        .get("highlighted")
                        .is_none_or(|flag| tier.highlighted == (flag == "true"))
            })
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn create_tier(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<CreatePricingTier>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTier> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let table_id = path.as_str().to_string();
        if !state.tables.contains_key(table_id.as_str()) {
            return Err(UnifiedError::not_found("Pricing table not found"));
        }
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        valid_price(input.monthly_price, "monthly_price")?;
        let yearly_price = input.yearly_price.unwrap_or(input.monthly_price * 12.0);
        valid_price(yearly_price, "yearly_price")?;
        let cta_label = input
            .cta_label
            .map(|label| require_text(&label, "cta_label", MAX_NAME_LEN))
            .transpose()?
            .unwrap_or_else(|| "Get started".to_string());
        let cta_url = input.cta_url.unwrap_or_else(|| "/pricing".to_string());
        if !valid_cta_url(&cta_url) {
            return Err(UnifiedError::bad_request(
                "cta_url must be a relative path or an https:// URL",
            ));
        }
        let description = optional_text(input.description, MAX_TEXT_LEN)?;
        let existing = state.tiers_of(&table_id);
        // The first tier of a table is always highlighted; an explicit
        // highlight clears the flag on every other tier.
        let highlighted = if existing.is_empty() {
            true
        } else {
            input.highlighted.unwrap_or(false)
        };
        let sort_order = input.sort_order.unwrap_or_else(|| {
            existing
                .iter()
                .map(|tier| tier.sort_order)
                .max()
                .map(|max| max + 1)
                .unwrap_or(0)
        });
        let tier = PricingTier {
            id: new_id(),
            table_id: table_id.clone(),
            name,
            description,
            monthly_price: input.monthly_price,
            yearly_price,
            cta_label,
            cta_url,
            highlighted,
            sort_order,
            created_at: now(),
            updated_at: now(),
        };
        state.tiers.insert(tier.id.clone(), tier.clone());
        if highlighted {
            state.clear_other_highlights(&table_id, &tier.id);
        }
        Ok(tier)
    })();
    respond_created(result, debug)
}

async fn get_tier(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTier> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_READ)?;
        let (table_id, tier_id) = path.into_inner();
        state
            .tiers
            .get(tier_id.as_str())
            .filter(|entry| entry.value().table_id == table_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))
    })();
    json::respond(result, debug)
}

async fn update_tier(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<(String, String)>,
    body: web::Json<UpdatePricingTier>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTier> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let (table_id, tier_id) = path.into_inner();
        let input = body.into_inner();
        if let Some(price) = input.monthly_price {
            valid_price(price, "monthly_price")?;
        }
        if let Some(price) = input.yearly_price {
            valid_price(price, "yearly_price")?;
        }
        if let Some(cta_url) = &input.cta_url {
            if !valid_cta_url(cta_url) {
                return Err(UnifiedError::bad_request(
                    "cta_url must be a relative path or an https:// URL",
                ));
            }
        }
        // Clearing the only highlighted tier would leave the table
        // with none: reject it instead of breaking the invariant.
        if input.highlighted == Some(false) {
            let is_highlighted = state
                .tiers
                .get(tier_id.as_str())
                .map(|entry| entry.value().highlighted)
                .unwrap_or(false);
            let others_highlighted = state
                .tiers_of(&table_id)
                .iter()
                .any(|tier| tier.id != tier_id && tier.highlighted);
            if is_highlighted && !others_highlighted {
                return Err(UnifiedError::unprocessable(
                    "Cannot clear the highlighted flag on the only highlighted tier; highlight another tier first",
                ));
            }
        }
        let mut tier = state
            .tiers
            .get_mut(tier_id.as_str())
            .filter(|entry| entry.table_id == table_id)
            .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))?;
        if let Some(name) = input.name {
            tier.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = input.description {
            if description.len() > MAX_TEXT_LEN {
                return Err(UnifiedError::bad_request(
                    "description must be at most 2000 characters",
                ));
            }
            tier.description = description;
        }
        if let Some(price) = input.monthly_price {
            tier.monthly_price = price;
        }
        if let Some(price) = input.yearly_price {
            tier.yearly_price = price;
        }
        if let Some(cta_label) = input.cta_label {
            tier.cta_label = require_text(&cta_label, "cta_label", MAX_NAME_LEN)?;
        }
        if let Some(cta_url) = input.cta_url {
            tier.cta_url = cta_url;
        }
        if let Some(highlighted) = input.highlighted {
            tier.highlighted = highlighted;
        }
        if let Some(sort_order) = input.sort_order {
            tier.sort_order = sort_order;
        }
        tier.updated_at = now();
        let updated = tier.clone();
        let highlight_now = updated.highlighted;
        drop(tier);
        if highlight_now {
            state.clear_other_highlights(&table_id, &tier_id);
        }
        Ok(updated)
    })();
    json::respond(result, debug)
}

async fn delete_tier(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let (table_id, tier_id) = path.into_inner();
        let tier = state
            .tiers
            .get(tier_id.as_str())
            .filter(|entry| entry.value().table_id == table_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))?;
        let was_highlighted = tier.highlighted;
        state.tiers.remove(tier_id.as_str());
        state
            .features
            .retain(|_, feature| feature.tier_id != tier_id);
        if was_highlighted {
            state.promote_next_highlight(&table_id);
        }
        Ok(json!({ "deleted": true, "id": tier_id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: tier features
// ---------------------------------------------------------------------------

fn tier_belongs(
    state: &PricingTablesState,
    table_id: &str,
    tier_id: &str,
) -> bool {
    state
        .tiers
        .get(tier_id)
        .is_some_and(|entry| entry.value().table_id == table_id)
}

async fn list_features(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_READ)?;
        let (table_id, tier_id) = path.into_inner();
        if !tier_belongs(&state, &table_id, &tier_id) {
            return Err(UnifiedError::not_found("Pricing tier not found"));
        }
        let params = query.into_inner();
        let items: Vec<TierFeature> = state
            .features_of(&tier_id)
            .into_iter()
            .filter(|feature| {
                matches_search(&format!("{} {}", feature.feature_text, feature.note), &params)
                    && params
                        .filters
                        .get("included")
                        .is_none_or(|flag| feature.included == (flag == "true"))
            })
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn create_feature(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<(String, String)>,
    body: web::Json<CreateTierFeature>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TierFeature> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let (table_id, tier_id) = path.into_inner();
        if !tier_belongs(&state, &table_id, &tier_id) {
            return Err(UnifiedError::not_found("Pricing tier not found"));
        }
        let input = body.into_inner();
        let feature_text = require_text(&input.feature_text, "feature_text", MAX_NAME_LEN)?;
        let note = optional_text(input.note, MAX_TEXT_LEN)?;
        let sort_order = input.sort_order.unwrap_or_else(|| {
            state
                .features_of(&tier_id)
                .iter()
                .map(|feature| feature.sort_order)
                .max()
                .map(|max| max + 1)
                .unwrap_or(0)
        });
        let feature = TierFeature {
            id: new_id(),
            tier_id,
            feature_text,
            included: input.included.unwrap_or(true),
            note,
            sort_order,
            created_at: now(),
            updated_at: now(),
        };
        state.features.insert(feature.id.clone(), feature.clone());
        Ok(feature)
    })();
    respond_created(result, debug)
}

async fn get_feature(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<(String, String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TierFeature> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_READ)?;
        let (table_id, tier_id, feature_id) = path.into_inner();
        if !tier_belongs(&state, &table_id, &tier_id) {
            return Err(UnifiedError::not_found("Pricing tier not found"));
        }
        state
            .features
            .get(feature_id.as_str())
            .filter(|entry| entry.value().tier_id == tier_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tier feature not found"))
    })();
    json::respond(result, debug)
}

async fn update_feature(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<(String, String, String)>,
    body: web::Json<UpdateTierFeature>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TierFeature> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let (table_id, tier_id, feature_id) = path.into_inner();
        if !tier_belongs(&state, &table_id, &tier_id) {
            return Err(UnifiedError::not_found("Pricing tier not found"));
        }
        let input = body.into_inner();
        let mut feature = state
            .features
            .get_mut(feature_id.as_str())
            .filter(|entry| entry.tier_id == tier_id)
            .ok_or_else(|| UnifiedError::not_found("Tier feature not found"))?;
        if let Some(feature_text) = input.feature_text {
            feature.feature_text = require_text(&feature_text, "feature_text", MAX_NAME_LEN)?;
        }
        if let Some(included) = input.included {
            feature.included = included;
        }
        if let Some(note) = input.note {
            if note.len() > MAX_TEXT_LEN {
                return Err(UnifiedError::bad_request(
                    "note must be at most 2000 characters",
                ));
            }
            feature.note = note;
        }
        if let Some(sort_order) = input.sort_order {
            feature.sort_order = sort_order;
        }
        feature.updated_at = now();
        Ok(feature.clone())
    })();
    json::respond(result, debug)
}

async fn delete_feature(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<(String, String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let (table_id, tier_id, feature_id) = path.into_inner();
        if !tier_belongs(&state, &table_id, &tier_id) {
            return Err(UnifiedError::not_found("Pricing tier not found"));
        }
        // Check the tier match before removing, so a feature of
        // another tier is never deleted by a mismatched path.
        let belongs = state
            .features
            .get(feature_id.as_str())
            .is_some_and(|entry| entry.value().tier_id == tier_id);
        if !belongs {
            return Err(UnifiedError::not_found("Tier feature not found"));
        }
        state.features.remove(feature_id.as_str());
        Ok(json!({ "deleted": true, "id": feature_id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: marketplace vendors
// ---------------------------------------------------------------------------

async fn list_vendors(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Vendor> = state
            .vendors
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|vendor| {
                matches_search(
                    &format!("{} {} {} {}", vendor.company_name, vendor.slug, vendor.email, vendor.user_id),
                    &params,
                ) && params
                    .filters
                    .get("status")
                    .is_none_or(|status| vendor.status == *status)
                    && params
                        .filters
                        .get("user_id")
                        .is_none_or(|user_id| vendor.user_id == *user_id)
            })
            .collect();
        let sort = params.allowed_sort(&["company_name", "slug", "status", "created_at"], "created_at");
        match sort.as_str() {
            "company_name" => items.sort_by(|a, b| a.company_name.cmp(&b.company_name)),
            "slug" => items.sort_by(|a, b| a.slug.cmp(&b.slug)),
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

/// Vendor application. Any authenticated caller holding
/// `marketplace:vendor` may apply for themselves; an admin holding
/// `marketplace:manage` may file an application for another user.
/// One vendor record per user.
async fn apply_vendor(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    body: web::Json<CreateVendor>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vendor> = (|| {
        let caller = caller(&req)?;
        let is_admin = check_capability(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE);
        let is_vendor = check_capability(&ctx, &caller.role, PERM_MARKETPLACE_VENDOR);
        if !is_admin && !is_vendor {
            return Err(UnifiedError::forbidden(
                "Role does not hold permission \"marketplace:vendor\" or \"marketplace:manage\"",
            ));
        }
        let input = body.into_inner();
        // A non-admin applicant applies for themselves only.
        let user_id = match input.user_id {
            Some(user_id) if is_admin => require_text(&user_id, "user_id", MAX_NAME_LEN)?,
            Some(_) => {
                return Err(UnifiedError::forbidden(
                    "Only a marketplace administrator may file an application for another user",
                ));
            }
            None => caller.actor.clone(),
        };
        if state
            .vendors
            .iter()
            .any(|entry| entry.value().user_id == user_id)
        {
            return Err(UnifiedError::conflict(format!(
                "User {user_id} already has a vendor record"
            )));
        }
        let company_name = require_text(&input.company_name, "company_name", MAX_NAME_LEN)?;
        let slug = match input.slug {
            Some(slug) if !slug.trim().is_empty() => slug.trim().to_lowercase(),
            _ => slugify(&company_name),
        };
        if !valid_slug(&slug) {
            return Err(UnifiedError::bad_request(
                "slug must be lowercase letters, digits, and hyphens",
            ));
        }
        if state.vendors.iter().any(|entry| entry.value().slug == slug) {
            return Err(UnifiedError::conflict(format!(
                "A vendor with slug \"{slug}\" already exists"
            )));
        }
        let email = require_text(&input.email, "email", MAX_NAME_LEN)?;
        if !valid_email(&email) {
            return Err(UnifiedError::bad_request("email must be a valid email address"));
        }
        let company_id = input
            .company_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let vendor = Vendor {
            id: new_id(),
            user_id: user_id.clone(),
            company_id,
            company_name,
            slug,
            email,
            status: "pending".to_string(),
            application_notes: optional_text(input.application_notes, MAX_TEXT_LEN)?,
            review_reason: String::new(),
            reviewed_by: None,
            reviewed_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.vendors.insert(vendor.id.clone(), vendor.clone());
        let value = serde_json::to_value(&vendor).unwrap_or(Value::Null);
        state.emit("marketplace.vendor_applied", value);
        Ok(vendor)
    })();
    respond_created(result, debug)
}

async fn get_vendor(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vendor> = (|| {
        let caller = caller(&req)?;
        let vendor = state
            .vendors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Vendor not found"))?;
        require_vendor_self_or_admin(&ctx, &caller, &vendor)
            .or_else(|_| {
                // Admins listing vendors use the read permission; a
                // vendor reading another vendor's record is refused.
                require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)
            })?;
        Ok(vendor)
    })();
    json::respond(result, debug)
}

async fn update_vendor(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<UpdateVendor>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vendor> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        let input = body.into_inner();
        if let Some(slug) = &input.slug {
            if !slug.trim().is_empty() && !valid_slug(&slug.trim().to_lowercase()) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and hyphens",
                ));
            }
            if !slug.trim().is_empty()
                && state.vendors.iter().any(|entry| {
                    entry.key() != path.as_str() && entry.value().slug == slug.trim().to_lowercase()
                })
            {
                return Err(UnifiedError::conflict(format!(
                    "A vendor with slug \"{}\" already exists",
                    slug.trim().to_lowercase()
                )));
            }
        }
        if let Some(email) = &input.email {
            if !email.trim().is_empty() && !valid_email(email.trim()) {
                return Err(UnifiedError::bad_request("email must be a valid email address"));
            }
        }
        let mut vendor = state
            .vendors
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Vendor not found"))?;
        if let Some(company_id) = input.company_id {
            vendor.company_id = if company_id.trim().is_empty() {
                None
            } else {
                Some(company_id.trim().to_string())
            };
        }
        if let Some(company_name) = input.company_name {
            vendor.company_name = require_text(&company_name, "company_name", MAX_NAME_LEN)?;
        }
        if let Some(slug) = input.slug {
            if !slug.trim().is_empty() {
                vendor.slug = slug.trim().to_lowercase();
            }
        }
        if let Some(email) = input.email {
            vendor.email = require_text(&email, "email", MAX_NAME_LEN)?;
        }
        if let Some(application_notes) = input.application_notes {
            if application_notes.len() > MAX_TEXT_LEN {
                return Err(UnifiedError::bad_request(
                    "application_notes must be at most 2000 characters",
                ));
            }
            vendor.application_notes = application_notes;
        }
        vendor.updated_at = now();
        Ok(vendor.clone())
    })();
    json::respond(result, debug)
}

/// Shared vendor status transition: pending -> approved/rejected,
/// approved -> suspended, suspended -> approved. Records the
/// reviewer, the reason, and the review time.
fn transition_vendor(
    state: &PricingTablesState,
    vendor_id: &str,
    from: &[&str],
    to: &str,
    event: &str,
    reviewer: &str,
    reason: &str,
) -> UnifiedResult<Vendor> {
    let mut vendor = state
        .vendors
        .get_mut(vendor_id)
        .ok_or_else(|| UnifiedError::not_found("Vendor not found"))?;
    if !from.contains(&vendor.status.as_str()) {
        return Err(UnifiedError::conflict(format!(
            "Vendor status is \"{}\"; expected one of: {}",
            vendor.status,
            from.join(", ")
        )));
    }
    vendor.status = to.to_string();
    vendor.review_reason = reason.trim().to_string();
    vendor.reviewed_by = Some(reviewer.to_string());
    vendor.reviewed_at = Some(now());
    vendor.updated_at = now();
    let updated = vendor.clone();
    drop(vendor);
    let value = serde_json::to_value(&updated).unwrap_or(Value::Null);
    state.emit(event, value);
    Ok(updated)
}

async fn approve_vendor(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<ReviewInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vendor> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        let reason = body.into_inner().reason.unwrap_or_default();
        transition_vendor(
            &state,
            path.as_str(),
            &["pending", "suspended"],
            "approved",
            "marketplace.vendor_approved",
            &caller.actor,
            &reason,
        )
    })();
    json::respond(result, debug)
}

async fn reject_vendor(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<ReviewInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vendor> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        let reason = body.into_inner().reason.unwrap_or_default();
        if reason.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A rejection reason is required",
            ));
        }
        transition_vendor(
            &state,
            path.as_str(),
            &["pending"],
            "rejected",
            "marketplace.vendor_rejected",
            &caller.actor,
            &reason,
        )
    })();
    json::respond(result, debug)
}

async fn suspend_vendor(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<ReviewInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vendor> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        let reason = body.into_inner().reason.unwrap_or_default();
        transition_vendor(
            &state,
            path.as_str(),
            &["approved"],
            "suspended",
            "marketplace.vendor_suspended",
            &caller.actor,
            &reason,
        )
    })();
    json::respond(result, debug)
}

async fn reactivate_vendor(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<ReviewInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vendor> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        let reason = body.into_inner().reason.unwrap_or_default();
        transition_vendor(
            &state,
            path.as_str(),
            &["suspended"],
            "approved",
            "marketplace.vendor_reactivated",
            &caller.actor,
            &reason,
        )
    })();
    json::respond(result, debug)
}

async fn vendor_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VendorDashboard> = (|| {
        let caller = caller(&req)?;
        let vendor = state
            .vendors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Vendor not found"))?;
        require_vendor_self_or_admin(&ctx, &caller, &vendor)?;
        Ok(state.dashboard_for(&vendor))
    })();
    json::respond(result, debug)
}

async fn list_vendor_commissions(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let vendor = state
            .vendors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Vendor not found"))?;
        require_vendor_self_or_admin(&ctx, &caller, &vendor)?;
        let params = query.into_inner();
        let mut items: Vec<Commission> = state
            .commissions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|commission| {
                commission.vendor_id == vendor.id
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| commission.status == *status)
                    && params
                        .filters
                        .get("order_id")
                        .is_none_or(|order_id| commission.order_id == *order_id)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn list_vendor_payouts(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let vendor = state
            .vendors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Vendor not found"))?;
        require_vendor_self_or_admin(&ctx, &caller, &vendor)?;
        let params = query.into_inner();
        let mut items: Vec<Payout> = state
            .payouts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|payout| {
                payout.vendor_id == vendor.id
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| payout.status == *status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

/// Public storefront data for one approved vendor. Non-approved
/// vendors answer 404 so pending or suspended storefronts are not
/// enumerable.
async fn vendor_storefront(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketplaceStorefront> = (|| {
        let slug = path.as_str();
        let vendor = state
            .vendors
            .iter()
            .find(|entry| entry.value().slug == slug && entry.value().status == "approved")
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Marketplace storefront not found"))?;
        let mut approved_product_ids: Vec<String> = state
            .product_approvals
            .iter()
            .filter(|entry| entry.value().vendor_id == vendor.id && entry.value().status == "approved")
            .map(|entry| entry.value().product_id.clone())
            .collect();
        approved_product_ids.sort();
        let product_count = approved_product_ids.len() as u64;
        Ok(MarketplaceStorefront {
            vendor,
            approved_product_ids,
            product_count,
        })
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: commission rules
// ---------------------------------------------------------------------------

fn valid_rule_scope(scope: &str, scope_value: &str) -> UnifiedResult<()> {
    match scope {
        "global" => {
            if !scope_value.is_empty() {
                return Err(UnifiedError::bad_request(
                    "A global commission rule must not carry a scope_value",
                ));
            }
        }
        "vendor" | "category" => {
            if scope_value.trim().is_empty() {
                return Err(UnifiedError::bad_request(format!(
                    "A {scope} commission rule requires a scope_value"
                )));
            }
        }
        _ => {
            return Err(UnifiedError::bad_request(
                "scope must be one of: vendor, category, global",
            ));
        }
    }
    Ok(())
}

async fn list_commission_rules(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        let params = query.into_inner();
        let mut items: Vec<CommissionRule> = state
            .commission_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| {
                params
                    .filters
                    .get("scope")
                    .is_none_or(|scope| rule.scope == *scope)
                    && params
                        .filters
                        .get("scope_value")
                        .is_none_or(|value| rule.scope_value == *value)
            })
            .collect();
        items.sort_by(|a, b| a.scope.cmp(&b.scope).then(a.scope_value.cmp(&b.scope_value)));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn create_commission_rule(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    body: web::Json<CreateCommissionRule>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommissionRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        let input = body.into_inner();
        let scope_value = input.scope_value.unwrap_or_default().trim().to_string();
        valid_rule_scope(&input.scope, &scope_value)?;
        valid_percent(input.rate_percent, "rate_percent")?;
        if input.scope == "vendor" && !state.vendors.contains_key(scope_value.as_str()) {
            return Err(UnifiedError::not_found(format!(
                "Vendor {scope_value} not found"
            )));
        }
        if state.commission_rules.iter().any(|entry| {
            entry.value().scope == input.scope && entry.value().scope_value == scope_value
        }) {
            return Err(UnifiedError::conflict(format!(
                "A commission rule for scope \"{}\" and value \"{scope_value}\" already exists",
                input.scope
            )));
        }
        let rule = CommissionRule {
            id: new_id(),
            scope: input.scope,
            scope_value,
            rate_percent: input.rate_percent,
            created_at: now(),
            updated_at: now(),
        };
        state
            .commission_rules
            .insert(rule.id.clone(), rule.clone());
        let value = serde_json::to_value(&rule).unwrap_or(Value::Null);
        state.emit("marketplace.commission_rule_created", value);
        Ok(rule)
    })();
    respond_created(result, debug)
}

async fn get_commission_rule(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommissionRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        state
            .commission_rules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Commission rule not found"))
    })();
    json::respond(result, debug)
}

async fn update_commission_rule(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<UpdateCommissionRule>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommissionRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        let input = body.into_inner();
        let existing = state
            .commission_rules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Commission rule not found"))?;
        let scope = input.scope.clone().unwrap_or(existing.scope);
        let scope_value = input
            .scope_value
            .clone()
            .map(|value| value.trim().to_string())
            .unwrap_or(existing.scope_value);
        valid_rule_scope(&scope, &scope_value)?;
        if let Some(rate) = input.rate_percent {
            valid_percent(rate, "rate_percent")?;
        }
        if scope == "vendor" && !state.vendors.contains_key(scope_value.as_str()) {
            return Err(UnifiedError::not_found(format!(
                "Vendor {scope_value} not found"
            )));
        }
        if state.commission_rules.iter().any(|entry| {
            entry.key() != path.as_str()
                && entry.value().scope == scope
                && entry.value().scope_value == scope_value
        }) {
            return Err(UnifiedError::conflict(format!(
                "A commission rule for scope \"{scope}\" and value \"{scope_value}\" already exists"
            )));
        }
        let mut rule = state
            .commission_rules
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Commission rule not found"))?;
        rule.scope = scope;
        rule.scope_value = scope_value;
        if let Some(rate) = input.rate_percent {
            rule.rate_percent = rate;
        }
        rule.updated_at = now();
        let updated = rule.clone();
        drop(rule);
        let value = serde_json::to_value(&updated).unwrap_or(Value::Null);
        state.emit("marketplace.commission_rule_updated", value);
        Ok(updated)
    })();
    json::respond(result, debug)
}

async fn delete_commission_rule(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        let rule = state
            .commission_rules
            .remove(path.as_str())
            .map(|(_, rule)| rule)
            .ok_or_else(|| UnifiedError::not_found("Commission rule not found"))?;
        let value = serde_json::to_value(&rule).unwrap_or(Value::Null);
        state.emit("marketplace.commission_rule_deleted", value);
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: commissions (read-only ledger)
// ---------------------------------------------------------------------------

async fn list_commissions(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Commission> = state
            .commissions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|commission| {
                params
                    .filters
                    .get("vendor_id")
                    .is_none_or(|vendor_id| commission.vendor_id == *vendor_id)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| commission.status == *status)
                    && params
                        .filters
                        .get("order_id")
                        .is_none_or(|order_id| commission.order_id == *order_id)
                    && params
                        .filters
                        .get("product_id")
                        .is_none_or(|product_id| commission.product_id == *product_id)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_commission(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Commission> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        state
            .commissions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Commission not found"))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: product approvals
// ---------------------------------------------------------------------------

/// A vendor submits one of their products for marketplace approval.
/// The Store module stays the source of truth for the product; this
/// endpoint records only the approval state and the vendor link. The
/// product must not already have an approval record.
async fn submit_product(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<SubmitProduct>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductApproval> = (|| {
        let caller = caller(&req)?;
        let input = body.into_inner();
        let product_id = path.as_str().to_string();
        if product_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("product_id must not be empty"));
        }
        let vendor = state
            .vendors
            .get(input.vendor_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Vendor not found"))?;
        require_vendor_self_or_admin(&ctx, &caller, &vendor)?;
        if vendor.status != "approved" {
            return Err(UnifiedError::conflict(format!(
                "Vendor status is \"{}\"; only an approved vendor may submit products",
                vendor.status
            )));
        }
        if state
            .product_approvals
            .iter()
            .any(|entry| entry.value().product_id == product_id)
        {
            return Err(UnifiedError::conflict(format!(
                "Product {product_id} already has an approval record"
            )));
        }
        let category = input
            .category
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let approval = ProductApproval {
            id: new_id(),
            product_id: product_id.clone(),
            vendor_id: vendor.id.clone(),
            category,
            status: "pending".to_string(),
            reason: String::new(),
            reviewed_by: None,
            reviewed_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .product_approvals
            .insert(approval.id.clone(), approval.clone());
        state.emit(
            "marketplace.product_approval_requested",
            json!({
                "approval_id": approval.id,
                "product_id": approval.product_id,
                "vendor_id": approval.vendor_id,
                "category": approval.category,
            }),
        );
        Ok(approval)
    })();
    respond_created(result, debug)
}

async fn list_product_approvals(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        let params = query.into_inner();
        let mut items: Vec<ProductApproval> = state
            .product_approvals
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|approval| {
                params
                    .filters
                    .get("status")
                    .is_none_or(|status| approval.status == *status)
                    && params
                        .filters
                        .get("vendor_id")
                        .is_none_or(|vendor_id| approval.vendor_id == *vendor_id)
                    && params
                        .filters
                        .get("product_id")
                        .is_none_or(|product_id| approval.product_id == *product_id)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_product_approval(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductApproval> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        state
            .product_approvals
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product approval not found"))
    })();
    json::respond(result, debug)
}

fn transition_product_approval(
    state: &PricingTablesState,
    approval_id: &str,
    to: &str,
    event: &str,
    reviewer: &str,
    reason: &str,
) -> UnifiedResult<ProductApproval> {
    let mut approval = state
        .product_approvals
        .get_mut(approval_id)
        .ok_or_else(|| UnifiedError::not_found("Product approval not found"))?;
    if approval.status != "pending" {
        return Err(UnifiedError::conflict(format!(
            "Product approval status is \"{}\"; only a pending approval can be reviewed",
            approval.status
        )));
    }
    approval.status = to.to_string();
    approval.reason = reason.trim().to_string();
    approval.reviewed_by = Some(reviewer.to_string());
    approval.reviewed_at = Some(now());
    approval.updated_at = now();
    let updated = approval.clone();
    drop(approval);
    state.emit(
        event,
        json!({
            "approval_id": updated.id,
            "product_id": updated.product_id,
            "vendor_id": updated.vendor_id,
            "status": updated.status,
            "reason": updated.reason,
        }),
    );
    Ok(updated)
}

async fn approve_product(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<ReviewInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductApproval> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        let reason = body.into_inner().reason.unwrap_or_default();
        transition_product_approval(
            &state,
            path.as_str(),
            "approved",
            "marketplace.product_approved",
            &caller.actor,
            &reason,
        )
    })();
    json::respond(result, debug)
}

async fn reject_product(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<ReviewInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductApproval> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        let reason = body.into_inner().reason.unwrap_or_default();
        if reason.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A rejection reason is required",
            ));
        }
        transition_product_approval(
            &state,
            path.as_str(),
            "rejected",
            "marketplace.product_rejected",
            &caller.actor,
            &reason,
        )
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: payouts
// ---------------------------------------------------------------------------

/// A vendor requests a payout of their pending commission balance.
/// The amount must be positive and must not exceed the vendor's
/// pending commission minus payouts already requested or approved.
async fn request_payout(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<CreatePayout>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Payout> = (|| {
        let caller = caller(&req)?;
        let vendor = state
            .vendors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Vendor not found"))?;
        require_vendor_self_or_admin(&ctx, &caller, &vendor)?;
        if vendor.status != "approved" {
            return Err(UnifiedError::conflict(format!(
                "Vendor status is \"{}\"; only an approved vendor may request payouts",
                vendor.status
            )));
        }
        let input = body.into_inner();
        valid_price(input.amount, "amount")?;
        if input.amount <= 0.0 {
            return Err(UnifiedError::bad_request("amount must be greater than zero"));
        }
        let currency = input.currency.unwrap_or_else(|| "USD".to_string());
        if !valid_currency(&currency) {
            return Err(UnifiedError::bad_request(
                "currency must be a 3-letter uppercase ISO 4217 code",
            ));
        }
        let dashboard = state.dashboard_for(&vendor);
        let outstanding: f64 = state
            .payouts
            .iter()
            .filter(|entry| {
                entry.value().vendor_id == vendor.id
                    && matches!(entry.value().status.as_str(), "requested" | "approved")
            })
            .map(|entry| entry.value().amount)
            .sum();
        let available = round_money(dashboard.pending_commission - outstanding);
        if round_money(input.amount) > available {
            return Err(UnifiedError::unprocessable(format!(
                "Requested amount exceeds the available pending commission balance of {available} {currency}"
            ))
            .with_metadata("available", json!(available)));
        }
        let payout = Payout {
            id: new_id(),
            vendor_id: vendor.id.clone(),
            amount: round_money(input.amount),
            currency: currency.clone(),
            status: "requested".to_string(),
            invoice_id: None,
            reference: String::new(),
            requested_by: caller.actor.clone(),
            reviewed_by: None,
            review_reason: String::new(),
            created_at: now(),
            updated_at: now(),
        };
        state.payouts.insert(payout.id.clone(), payout.clone());
        // Billing pays the payout out; the marketplace only records
        // the request and reconciles the invoice later.
        state.emit(
            "marketplace.payout_requested",
            json!({
                "payout_id": payout.id,
                "vendor_id": payout.vendor_id,
                "amount": payout.amount,
                "currency": payout.currency,
                "requested_by": payout.requested_by,
            }),
        );
        Ok(payout)
    })();
    respond_created(result, debug)
}

async fn list_payouts(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Payout> = state
            .payouts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|payout| {
                params
                    .filters
                    .get("vendor_id")
                    .is_none_or(|vendor_id| payout.vendor_id == *vendor_id)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| payout.status == *status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_payout(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Payout> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        state
            .payouts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Payout not found"))
    })();
    json::respond(result, debug)
}

fn transition_payout(
    state: &PricingTablesState,
    payout_id: &str,
    from: &[&str],
    to: &str,
    event: &str,
    reviewer: &str,
    reason: &str,
) -> UnifiedResult<Payout> {
    let mut payout = state
        .payouts
        .get_mut(payout_id)
        .ok_or_else(|| UnifiedError::not_found("Payout not found"))?;
    if !from.contains(&payout.status.as_str()) {
        return Err(UnifiedError::conflict(format!(
            "Payout status is \"{}\"; expected one of: {}",
            payout.status,
            from.join(", ")
        )));
    }
    payout.status = to.to_string();
    payout.reviewed_by = Some(reviewer.to_string());
    payout.review_reason = reason.trim().to_string();
    payout.updated_at = now();
    let updated = payout.clone();
    drop(payout);
    let value = serde_json::to_value(&updated).unwrap_or(Value::Null);
    state.emit(event, value);
    Ok(updated)
}

async fn approve_payout(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<ReviewInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Payout> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        let reason = body.into_inner().reason.unwrap_or_default();
        transition_payout(
            &state,
            path.as_str(),
            &["requested"],
            "approved",
            "marketplace.payout_approved",
            &caller.actor,
            &reason,
        )
    })();
    json::respond(result, debug)
}

async fn reject_payout(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<ReviewInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Payout> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        let reason = body.into_inner().reason.unwrap_or_default();
        if reason.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A rejection reason is required",
            ));
        }
        transition_payout(
            &state,
            path.as_str(),
            &["requested"],
            "rejected",
            "marketplace.payout_rejected",
            &caller.actor,
            &reason,
        )
    })();
    json::respond(result, debug)
}

/// Mark an approved payout as paid. The reference (bank transfer
/// id, billing payment id) is stored on the record. Paying a payout
/// also marks the vendor's oldest pending commissions as paid, up
/// to the payout amount, so the dashboard pending balance falls as
/// money goes out. Commissions are marked in creation order.
async fn mark_payout_paid(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<MarkPaidInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Payout> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        let reference = body
            .into_inner()
            .reference
            .unwrap_or_default()
            .trim()
            .to_string();
        let payout = transition_payout(
            &state,
            path.as_str(),
            &["approved"],
            "paid",
            "marketplace.payout_paid",
            &caller.actor,
            "",
        )?;
        if !reference.is_empty() {
            if let Some(mut entry) = state.payouts.get_mut(payout.id.as_str()) {
                entry.value_mut().reference = reference;
                entry.value_mut().updated_at = now();
            }
        }
        // Settle pending commissions oldest-first up to the payout.
        let mut remaining = payout.amount;
        let mut pending: Vec<Commission> = state
            .commissions
            .iter()
            .filter(|entry| {
                entry.value().vendor_id == payout.vendor_id && entry.value().status == "pending"
            })
            .map(|entry| entry.value().clone())
            .collect();
        pending.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        for commission in pending {
            if remaining <= 0.0 {
                break;
            }
            if commission.commission_amount > remaining {
                break;
            }
            remaining = round_money(remaining - commission.commission_amount);
            if let Some(mut entry) = state.commissions.get_mut(commission.id.as_str()) {
                entry.value_mut().status = "paid".to_string();
                entry.value_mut().updated_at = now();
            }
        }
        let updated = state
            .payouts
            .get(payout.id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Payout not found"))?;
        Ok(updated)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Hook handlers: store order events (commissions)
// ---------------------------------------------------------------------------

/// Compute commissions for one paid order. Shared by the `order.paid`
/// and `store.order.completed` handlers: the payload is the Store
/// module's Order value, so each line carries `product_id`,
/// `line_total` (falling back to `unit_price * quantity`), and
/// `company_id`. Idempotent per (order, product): a commission
/// already recorded for the same order + product is never computed
/// twice, so an order that emits both events yields one ledger entry
/// per line.
fn on_order_paid(state: Arc<PricingTablesState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(order_id) = data.get("id").and_then(|v| v.as_str()) else {
        return;
    };
    let order_id = order_id.to_string();
    let currency = data
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("USD")
        .to_string();
    let Some(items) = data.get("items").and_then(|v| v.as_array()) else {
        return;
    };
    for item in items {
        let Some(product_id) = item.get("product_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let product_id = product_id.to_string();
        let line_total = item
            .get("line_total")
            .and_then(|v| v.as_f64())
            .or_else(|| {
                let unit = item.get("unit_price").and_then(|v| v.as_f64())?;
                let quantity = item.get("quantity").and_then(|v| v.as_u64())? as f64;
                Some(unit * quantity)
            })
            .unwrap_or(0.0);
        if line_total < 0.0 {
            continue;
        }
        if state.commissions.iter().any(|entry| {
            entry.value().order_id == order_id && entry.value().product_id == product_id
        }) {
            continue;
        }
        let company_id = item.get("company_id").and_then(|v| v.as_str());
        let Some((vendor_id, category)) =
            state.vendor_for_order_line(&product_id, company_id)
        else {
            continue;
        };
        let rate = state.commission_rate_for(&vendor_id, category.as_deref());
        let amount = round_money(line_total * rate / 100.0);
        let commission = Commission {
            id: new_id(),
            order_id: order_id.clone(),
            vendor_id: vendor_id.clone(),
            product_id: product_id.clone(),
            order_total: round_money(line_total),
            rate_percent: rate,
            commission_amount: amount,
            currency: currency.clone(),
            status: "pending".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .commissions
            .insert(commission.id.clone(), commission.clone());
        state.emit(
            "marketplace.commission_computed",
            json!({
                "commission_id": commission.id,
                "order_id": commission.order_id,
                "vendor_id": commission.vendor_id,
                "product_id": commission.product_id,
                "order_total": commission.order_total,
                "rate_percent": commission.rate_percent,
                "commission_amount": commission.commission_amount,
                "currency": commission.currency,
                "status": "pending",
            }),
        );
    }
}

/// On `store.order.refunded`, reverse the order's pending
/// commissions. Commissions already paid out are left untouched and
/// reported in a single `marketplace.commission_computed` event with
/// status `paid_reversal_review`, so billing can reconcile the
/// clawback instead of this module silently rewriting paid money.
fn on_order_refunded(state: Arc<PricingTablesState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(order_id) = data.get("id").and_then(|v| v.as_str()) else {
        return;
    };
    let order_id = order_id.to_string();
    let mut paid_ids: Vec<String> = Vec::new();
    let mut reversed: Vec<Commission> = Vec::new();
    for mut entry in state.commissions.iter_mut() {
        if entry.value().order_id != order_id {
            continue;
        }
        match entry.value().status.as_str() {
            "pending" => {
                entry.value_mut().status = "reversed".to_string();
                entry.value_mut().updated_at = now();
                reversed.push(entry.value().clone());
            }
            "paid" => paid_ids.push(entry.key().clone()),
            _ => {}
        }
    }
    for commission in reversed {
        state.emit(
            "marketplace.commission_computed",
            json!({
                "commission_id": commission.id,
                "order_id": commission.order_id,
                "vendor_id": commission.vendor_id,
                "product_id": commission.product_id,
                "order_total": commission.order_total,
                "rate_percent": commission.rate_percent,
                "commission_amount": commission.commission_amount,
                "currency": commission.currency,
                "status": "reversed",
            }),
        );
    }
    if !paid_ids.is_empty() {
        state.emit(
            "marketplace.commission_computed",
            json!({
                "order_id": order_id,
                "status": "paid_reversal_review",
                "paid_commission_ids": paid_ids,
            }),
        );
    }
}

/// On `invoice.created` (Billing), attach the invoice id to the
/// payout the invoice was raised for, when the payload names one;
/// otherwise keep the invoice reference in the reconciliation log.
fn on_invoice_created(state: Arc<PricingTablesState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let invoice_id = data
        .get("id")
        .or_else(|| data.get("invoice_id"))
        .or_else(|| data.get("number"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let payout_id = data
        .get("payout_id")
        .or_else(|| data.get("metadata").and_then(|m| m.get("payout_id")))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if let (Some(invoice_id), Some(payout_id)) = (invoice_id.clone(), payout_id) {
        if let Some(mut payout) = state.payouts.get_mut(payout_id.as_str()) {
            payout.value_mut().invoice_id = Some(invoice_id.clone());
            payout.value_mut().updated_at = now();
        }
    }
    if let Some(invoice_id) = invoice_id {
        state.invoice_refs.insert(invoice_id, data);
    }
}

// ---------------------------------------------------------------------------
// Hook handlers: answering other modules
// ---------------------------------------------------------------------------

/// Answer `pricing_table.render_requested` with the render data for
/// a published table. Draft tables do not answer: a page must not
/// render unpublished pricing.
fn on_render_requested(state: Arc<PricingTablesState>, ctx: &mut HookContext) {
    let Some(table_id) = ctx.get("table_id").and_then(|v| v.as_str()) else {
        return;
    };
    let Some(table) = state
        .tables
        .get(table_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    if table.status != "published" {
        return;
    }
    let render = state.render_data(&table);
    let value = serde_json::to_value(&render).unwrap_or(Value::Null);
    ctx.add_result(value);
}

/// Answer `marketplace.vendor_check` with the vendor id and status
/// for a user, when the user owns a vendor record.
fn on_vendor_check(state: Arc<PricingTablesState>, ctx: &mut HookContext) {
    let Some(user_id) = ctx.get("user_id").and_then(|v| v.as_str()) else {
        return;
    };
    if let Some(vendor) = state
        .vendors
        .iter()
        .find(|entry| entry.value().user_id == user_id)
        .map(|entry| entry.value().clone())
    {
        ctx.add_result(json!({
            "user_id": user_id,
            "vendor_id": vendor.id,
            "status": vendor.status,
            "company_name": vendor.company_name,
            "slug": vendor.slug,
        }));
    }
}

/// Answer `marketplace.commission_quote` with the applicable rate and
/// the computed commission for an amount, without writing anything.
/// Resolution is vendor rule, then category rule, then global rule,
/// then the built-in default — the same order the paid-order
/// handler uses.
fn on_commission_quote(state: Arc<PricingTablesState>, ctx: &mut HookContext) {
    let Some(amount) = ctx.get("amount").and_then(|v| v.as_f64()) else {
        return;
    };
    if !amount.is_finite() || amount < 0.0 {
        return;
    }
    let vendor_id = ctx.get("vendor_id").and_then(|v| v.as_str());
    let category = ctx.get("category").and_then(|v| v.as_str());
    let rate = match vendor_id {
        Some(vendor_id) => state.commission_rate_for(vendor_id, category),
        None => {
            if let Some(category) = category {
                state
                    .commission_rules
                    .iter()
                    .find(|entry| {
                        entry.value().scope == "category"
                            && entry.value().scope_value == category
                    })
                    .map(|entry| entry.value().rate_percent)
                    .or_else(|| {
                        state
                            .commission_rules
                            .iter()
                            .find(|entry| {
                                entry.value().scope == "global"
                                    && entry.value().scope_value.is_empty()
                            })
                            .map(|entry| entry.value().rate_percent)
                    })
                    .unwrap_or(DEFAULT_COMMISSION_RATE_PERCENT)
            } else {
                state.commission_rate_for("", None)
            }
        }
    };
    ctx.add_result(json!({
        "amount": round_money(amount),
        "rate_percent": rate,
        "commission_amount": round_money(amount * rate / 100.0),
    }));
}

/// Answer `marketplace.product_approval_status` with the approval
/// status for a product, when an approval record exists.
fn on_product_approval_status(state: Arc<PricingTablesState>, ctx: &mut HookContext) {
    let Some(product_id) = ctx.get("product_id").and_then(|v| v.as_str()) else {
        return;
    };
    if let Some(approval) = state
        .product_approvals
        .iter()
        .find(|entry| entry.value().product_id == product_id)
        .map(|entry| entry.value().clone())
    {
        ctx.add_result(json!({
            "product_id": approval.product_id,
            "vendor_id": approval.vendor_id,
            "approval_id": approval.id,
            "status": approval.status,
        }));
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct PricingTablesModule {
    state: Arc<PricingTablesState>,
}

impl PricingTablesModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(PricingTablesState::new()),
        }
    }
}

impl Default for PricingTablesModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PricingTablesModule {
    fn id(&self) -> &'static str {
        "pricing_tables"
    }

    fn name(&self) -> &'static str {
        "Pricing Tables & Marketplace Builder"
    }

    fn description(&self) -> &'static str {
        "Reusable pricing tables with tiers, feature lists, and a monthly/yearly billing-cycle toggle, plus a marketplace builder: vendor onboarding, product approval, commissions computed from store orders, vendor dashboards, and payouts through billing."
    }

    fn requires(&self) -> &[&'static str] {
        &["products", "store", "billing", "site_builder", "notifications"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("commerce")
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
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_order_paid(state.clone(), ctx));
        bus.register("order.paid", "pricing_tables", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_order_paid(state.clone(), ctx));
        bus.register("store.order.completed", "pricing_tables", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_order_refunded(state.clone(), ctx));
        bus.register("store.order.refunded", "pricing_tables", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_invoice_created(state.clone(), ctx));
        bus.register("invoice.created", "pricing_tables", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_render_requested(state.clone(), ctx));
        bus.register("pricing_table.render_requested", "pricing_tables", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_vendor_check(state.clone(), ctx));
        bus.register("marketplace.vendor_check", "pricing_tables", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_commission_quote(state.clone(), ctx));
        bus.register("marketplace.commission_quote", "pricing_tables", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_product_approval_status(state.clone(), ctx)
        });
        bus.register(
            "marketplace.product_approval_status",
            "pricing_tables",
            handler,
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/pricing-tables")
                // Tables.
                .route("/tables", web::get().to(list_tables))
                .route("/tables", web::post().to(create_table))
                .route("/tables/{id}", web::get().to(get_table))
                .route("/tables/{id}", web::put().to(update_table))
                .route("/tables/{id}", web::delete().to(delete_table))
                .route("/tables/{id}/publish", web::post().to(publish_table))
                .route("/tables/{id}/unpublish", web::post().to(unpublish_table))
                .route("/tables/{id}/render", web::get().to(render_table))
                .route("/tables/{id}/embed", web::get().to(embed_table))
                // Tiers.
                .route("/tables/{id}/tiers", web::get().to(list_tiers))
                .route("/tables/{id}/tiers", web::post().to(create_tier))
                .route("/tables/{id}/tiers/{tier_id}", web::get().to(get_tier))
                .route("/tables/{id}/tiers/{tier_id}", web::put().to(update_tier))
                .route("/tables/{id}/tiers/{tier_id}", web::delete().to(delete_tier))
                // Tier features.
                .route(
                    "/tables/{id}/tiers/{tier_id}/features",
                    web::get().to(list_features),
                )
                .route(
                    "/tables/{id}/tiers/{tier_id}/features",
                    web::post().to(create_feature),
                )
                .route(
                    "/tables/{id}/tiers/{tier_id}/features/{feature_id}",
                    web::get().to(get_feature),
                )
                .route(
                    "/tables/{id}/tiers/{tier_id}/features/{feature_id}",
                    web::put().to(update_feature),
                )
                .route(
                    "/tables/{id}/tiers/{tier_id}/features/{feature_id}",
                    web::delete().to(delete_feature),
                )
                // Marketplace vendors.
                .route("/marketplace/vendors", web::get().to(list_vendors))
                .route("/marketplace/vendors", web::post().to(apply_vendor))
                .route("/marketplace/vendors/{id}", web::get().to(get_vendor))
                .route("/marketplace/vendors/{id}", web::put().to(update_vendor))
                .route(
                    "/marketplace/vendors/{id}/approve",
                    web::post().to(approve_vendor),
                )
                .route(
                    "/marketplace/vendors/{id}/reject",
                    web::post().to(reject_vendor),
                )
                .route(
                    "/marketplace/vendors/{id}/suspend",
                    web::post().to(suspend_vendor),
                )
                .route(
                    "/marketplace/vendors/{id}/reactivate",
                    web::post().to(reactivate_vendor),
                )
                .route(
                    "/marketplace/vendors/{id}/dashboard",
                    web::get().to(vendor_dashboard),
                )
                .route(
                    "/marketplace/vendors/{id}/commissions",
                    web::get().to(list_vendor_commissions),
                )
                .route(
                    "/marketplace/vendors/{id}/payouts",
                    web::get().to(list_vendor_payouts),
                )
                .route(
                    "/marketplace/vendors/{id}/payouts",
                    web::post().to(request_payout),
                )
                // Marketplace storefront.
                .route(
                    "/marketplace/storefronts/{slug}",
                    web::get().to(vendor_storefront),
                )
                // Commission rules.
                .route(
                    "/marketplace/commission-rules",
                    web::get().to(list_commission_rules),
                )
                .route(
                    "/marketplace/commission-rules",
                    web::post().to(create_commission_rule),
                )
                .route(
                    "/marketplace/commission-rules/{id}",
                    web::get().to(get_commission_rule),
                )
                .route(
                    "/marketplace/commission-rules/{id}",
                    web::put().to(update_commission_rule),
                )
                .route(
                    "/marketplace/commission-rules/{id}",
                    web::delete().to(delete_commission_rule),
                )
                // Commissions (read-only ledger).
                .route("/marketplace/commissions", web::get().to(list_commissions))
                .route(
                    "/marketplace/commissions/{id}",
                    web::get().to(get_commission),
                )
                // Product approvals.
                .route(
                    "/marketplace/products/{product_id}/submit",
                    web::post().to(submit_product),
                )
                .route(
                    "/marketplace/product-approvals",
                    web::get().to(list_product_approvals),
                )
                .route(
                    "/marketplace/product-approvals/{id}",
                    web::get().to(get_product_approval),
                )
                .route(
                    "/marketplace/product-approvals/{id}/approve",
                    web::post().to(approve_product),
                )
                .route(
                    "/marketplace/product-approvals/{id}/reject",
                    web::post().to(reject_product),
                )
                // Payouts (admin).
                .route("/marketplace/payouts", web::get().to(list_payouts))
                .route("/marketplace/payouts/{id}", web::get().to(get_payout))
                .route(
                    "/marketplace/payouts/{id}/approve",
                    web::post().to(approve_payout),
                )
                .route(
                    "/marketplace/payouts/{id}/reject",
                    web::post().to(reject_payout),
                )
                .route(
                    "/marketplace/payouts/{id}/mark-paid",
                    web::post().to(mark_payout_paid),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/tables", owner: "pricing_tables", description: "List pricing tables (paginated; filter by status, currency)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/tables", owner: "pricing_tables", description: "Create a pricing table." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/tables/{id}", owner: "pricing_tables", description: "Get one pricing table." },
            RouteInfo { method: "PUT", path: "/api/v1/pricing-tables/tables/{id}", owner: "pricing_tables", description: "Update a pricing table." },
            RouteInfo { method: "DELETE", path: "/api/v1/pricing-tables/tables/{id}", owner: "pricing_tables", description: "Delete a pricing table with its tiers and features." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/tables/{id}/publish", owner: "pricing_tables", description: "Publish a pricing table (requires at least one tier)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/tables/{id}/unpublish", owner: "pricing_tables", description: "Unpublish a pricing table back to draft." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/tables/{id}/render", owner: "pricing_tables", description: "Structured render data for a table (public when published; drafts need pricing_tables:read)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/tables/{id}/embed", owner: "pricing_tables", description: "Embed snippet data: table id, render URL, and placeholder snippet." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/tables/{id}/tiers", owner: "pricing_tables", description: "List a table's tiers in sort order." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/tables/{id}/tiers", owner: "pricing_tables", description: "Create a tier (first tier is highlighted; highlighting clears other tiers)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/tables/{id}/tiers/{tier_id}", owner: "pricing_tables", description: "Get one tier." },
            RouteInfo { method: "PUT", path: "/api/v1/pricing-tables/tables/{id}/tiers/{tier_id}", owner: "pricing_tables", description: "Update a tier (clearing the only highlight is rejected)." },
            RouteInfo { method: "DELETE", path: "/api/v1/pricing-tables/tables/{id}/tiers/{tier_id}", owner: "pricing_tables", description: "Delete a tier with its features (deleting the highlighted tier promotes the next one)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/tables/{id}/tiers/{tier_id}/features", owner: "pricing_tables", description: "List a tier's feature rows in sort order." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/tables/{id}/tiers/{tier_id}/features", owner: "pricing_tables", description: "Create a tier feature row." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/tables/{id}/tiers/{tier_id}/features/{feature_id}", owner: "pricing_tables", description: "Get one tier feature row." },
            RouteInfo { method: "PUT", path: "/api/v1/pricing-tables/tables/{id}/tiers/{tier_id}/features/{feature_id}", owner: "pricing_tables", description: "Update a tier feature row." },
            RouteInfo { method: "DELETE", path: "/api/v1/pricing-tables/tables/{id}/tiers/{tier_id}/features/{feature_id}", owner: "pricing_tables", description: "Delete a tier feature row." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/vendors", owner: "pricing_tables", description: "List vendors (paginated; filter by status, user)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/vendors", owner: "pricing_tables", description: "Apply as a vendor (self-service; admin may file for another user)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/vendors/{id}", owner: "pricing_tables", description: "Get one vendor (own record or marketplace:read)." },
            RouteInfo { method: "PUT", path: "/api/v1/pricing-tables/marketplace/vendors/{id}", owner: "pricing_tables", description: "Update a vendor (admin)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/vendors/{id}/approve", owner: "pricing_tables", description: "Approve a pending or suspended vendor." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/vendors/{id}/reject", owner: "pricing_tables", description: "Reject a pending vendor (reason required)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/vendors/{id}/suspend", owner: "pricing_tables", description: "Suspend an approved vendor." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/vendors/{id}/reactivate", owner: "pricing_tables", description: "Reactivate a suspended vendor." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/vendors/{id}/dashboard", owner: "pricing_tables", description: "Vendor dashboard: sales, orders, commissions, payouts, products (aggregated live)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/vendors/{id}/commissions", owner: "pricing_tables", description: "List one vendor's commission ledger entries." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/vendors/{id}/payouts", owner: "pricing_tables", description: "List one vendor's payouts." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/vendors/{id}/payouts", owner: "pricing_tables", description: "Request a payout (capped at the available pending commission balance)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/storefronts/{slug}", owner: "pricing_tables", description: "Public storefront data for an approved vendor (vendor + approved product ids)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/commission-rules", owner: "pricing_tables", description: "List commission rules (paginated; filter by scope)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/commission-rules", owner: "pricing_tables", description: "Create a commission rule (vendor, category, or global scope)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/commission-rules/{id}", owner: "pricing_tables", description: "Get one commission rule." },
            RouteInfo { method: "PUT", path: "/api/v1/pricing-tables/marketplace/commission-rules/{id}", owner: "pricing_tables", description: "Update a commission rule." },
            RouteInfo { method: "DELETE", path: "/api/v1/pricing-tables/marketplace/commission-rules/{id}", owner: "pricing_tables", description: "Delete a commission rule." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/commissions", owner: "pricing_tables", description: "List the commission ledger (paginated; filter by vendor, order, product, status)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/commissions/{id}", owner: "pricing_tables", description: "Get one commission ledger entry." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/products/{product_id}/submit", owner: "pricing_tables", description: "Vendor submits a product for marketplace approval." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/product-approvals", owner: "pricing_tables", description: "List product approvals (paginated; filter by status, vendor, product)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/product-approvals/{id}", owner: "pricing_tables", description: "Get one product approval." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/product-approvals/{id}/approve", owner: "pricing_tables", description: "Approve a pending product." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/product-approvals/{id}/reject", owner: "pricing_tables", description: "Reject a pending product (reason required)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/payouts", owner: "pricing_tables", description: "List payouts (paginated; filter by vendor, status)." },
            RouteInfo { method: "GET", path: "/api/v1/pricing-tables/marketplace/payouts/{id}", owner: "pricing_tables", description: "Get one payout." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/payouts/{id}/approve", owner: "pricing_tables", description: "Approve a requested payout." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/payouts/{id}/reject", owner: "pricing_tables", description: "Reject a requested payout (reason required)." },
            RouteInfo { method: "POST", path: "/api/v1/pricing-tables/marketplace/payouts/{id}/mark-paid", owner: "pricing_tables", description: "Mark an approved payout paid and settle pending commissions oldest-first up to the amount." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PricingTablesModule::new()));
}
