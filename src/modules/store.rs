// Cart / Store / Marketplace module.
//
// Products linked to software releases, pricing tiers and editions,
// coupons, a per-session cart with server-side totals, checkout that
// creates an order and emits `order.created` (licensing and billing listen
// to it) as well as `store.order.created`, payment initiation, refunds,
// cancellations, signed expiring download links with per-license limits,
// download entitlements, a WordPress-style update API, and per-company
// payout attribution.
//
// Subscription checkout: a paid order holding exactly one
// subscription product creates a `Subscription` (`trialing` when the
// product has trial days, else `active`), snapshotting the product's
// plan fields (proration policy, grace period, dunning settings).
// Plan changes mid-cycle apply the proration policy, cancellation
// is immediate or at period end, and the billing module drives the
// lifecycle (renewals, dunning, grace, reminders) through
// `subscription.lifecycle.apply` events, which this module applies
// to the authoritative record. A subscription to a product bound to
// a sellable module counts as a module entitlement through
// `subscription.module_entitlement.check` (see the integration
// notes for the licensing convergence point).
//
// Digital products (ebooks and other downloadable digital goods):
// a store product gains a digital profile (an admin-extensible
// type from the digital type registry, file metadata such as
// author, ISBN, pages, language, publisher and edition, a cover
// image reference, and its deliverable files as storage
// references). Digital products sell through the normal checkout
// like any other product. When an order containing one becomes
// paid, this module mints one download entitlement per file and
// accrues the per-sale royalty for the author or vendor. Delivery
// reuses the existing signed expiring download-link flow: the
// purchase grant id plays the `license_id` role in the signed URL
// and the file id plays the `release_id` role, so no second
// download system exists. Free samples are served from preview
// files the seller allows, rate-limited and logged. Watermarking
// is metadata-level only: the buyer identity is embedded into the
// delivered file's metadata where the format supports it (see the
// watermark format registry); it is NOT unbreakable DRM and must
// never be claimed as such. Royalties accrue here; payout requests
// only emit `store.digital.payout.requested` for billing, which
// owns moving money. Digital-products selling is a sellable store
// capability, category `commerce` (see
// `DIGITAL_PRODUCTS_AVAILABLE_AS_PRODUCT`); the store module
// itself is the marketplace, not a sellable module.
//
// Interconnection happens only through the hook bus: this module never
// calls another module directly. License issuance is done by the
// licensing module through the `order.created` hook; this module reads
// the hook results back and records the download entitlement for each
// license that was issued.
//
// All data lives in memory in this module; persistence of
// products, tiers, coupons, orders, entitlements and download
// records is a wiring step for the database layer. The digital
// products tables (`digital_products`, `digital_product_files`,
// `digital_product_previews`, `digital_royalties`,
// `digital_downloads`, plus the `digital_product_types` registry
// and `digital_payout_requests`) exist as persistence targets in
// migration 2026-11-22-000075 and are not yet this module's read
// path: the in-memory maps stay authoritative until the database
// layer is wired.

use crate::core::config::AppConfig;
use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::reference_id::{self, ReferenceIdSettings};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

fn default_product_status() -> String {
    "published".to_string()
}

fn default_pricing_model() -> String {
    "one_time".to_string()
}

fn default_proration_policy() -> String {
    "prorate".to_string()
}

fn default_dunning_max_retries() -> u32 {
    3
}

fn default_dunning_retry_interval_days() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Product {
    pub id: String,
    pub name: String,
    pub description: String,
    pub price: f64,
    pub currency: String,
    /// Product kind: `theme`, `plugin`, or `software`.
    pub kind: String,
    pub license_product: bool,
    pub file_id: Option<String>,
    /// URL slug used by the update API (`my-plugin`).
    #[serde(default)]
    pub slug: Option<String>,
    /// Id of the linked software record, when this product sells software.
    #[serde(default)]
    pub software_id: Option<String>,
    /// Slug of the linked software record.
    #[serde(default)]
    pub software_slug: Option<String>,
    /// `draft` or `published`. Only published products can be bought.
    #[serde(default = "default_product_status")]
    pub status: String,
    /// Vendor / company that owns the software; orders inherit it so
    /// payouts can be attributed per company.
    #[serde(default)]
    pub company_id: Option<String>,
    /// `one_time` or `subscription`.
    #[serde(default = "default_pricing_model")]
    pub pricing_model: String,
    /// `monthly` or `yearly` for subscriptions.
    #[serde(default)]
    pub billing_period: Option<String>,
    /// Price charged on each renewal (defaults to the base price).
    #[serde(default)]
    pub renewal_price: Option<f64>,
    /// Free trial length in days (0 means no trial).
    #[serde(default)]
    pub trial_days: u32,
    /// Mid-cycle plan-change policy: `prorate`, `no_prorate`, or
    /// `credit_only`. Mirrored from the products catalogue on
    /// `product.listed`; snapshotted into a subscription at checkout.
    #[serde(default = "default_proration_policy")]
    pub proration_policy: String,
    /// Days of continued access after a failed renewal payment
    /// before the subscription is cancelled (0 = cancel as soon as
    /// dunning is exhausted).
    #[serde(default)]
    pub grace_period_days: u32,
    /// Maximum failed-payment (dunning) retries before the
    /// subscription is cancelled.
    #[serde(default = "default_dunning_max_retries")]
    pub dunning_max_retries: u32,
    /// Days between dunning retries.
    #[serde(default = "default_dunning_retry_interval_days")]
    pub dunning_retry_interval_days: u32,
    /// Sellable module this product sells access to (a module id
    /// from the module registry, for example `meetings`), when the
    /// product is a module offering. A subscription to this product
    /// then counts as a module entitlement for that module (see
    /// `subscription.module_entitlement.check`).
    #[serde(default)]
    pub module_id: Option<String>,
    /// Years of updates included with a one-time purchase (default 1).
    #[serde(default)]
    pub updates_years: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProduct {
    pub name: String,
    pub description: Option<String>,
    pub price: f64,
    pub currency: Option<String>,
    pub kind: String,
    pub license_product: Option<bool>,
    pub file_id: Option<String>,
    pub slug: Option<String>,
    pub software_id: Option<String>,
    pub software_slug: Option<String>,
    pub status: Option<String>,
    pub company_id: Option<String>,
    pub pricing_model: Option<String>,
    pub billing_period: Option<String>,
    pub renewal_price: Option<f64>,
    pub trial_days: Option<u32>,
    pub proration_policy: Option<String>,
    pub grace_period_days: Option<u32>,
    pub dunning_max_retries: Option<u32>,
    pub dunning_retry_interval_days: Option<u32>,
    pub module_id: Option<String>,
    pub updates_years: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProduct {
    pub name: Option<String>,
    pub description: Option<String>,
    pub price: Option<f64>,
    pub currency: Option<String>,
    pub kind: Option<String>,
    pub license_product: Option<bool>,
    pub file_id: Option<String>,
    pub slug: Option<String>,
    pub software_id: Option<String>,
    pub software_slug: Option<String>,
    pub status: Option<String>,
    pub company_id: Option<String>,
    pub pricing_model: Option<String>,
    pub billing_period: Option<String>,
    pub renewal_price: Option<f64>,
    pub trial_days: Option<u32>,
    pub proration_policy: Option<String>,
    pub grace_period_days: Option<u32>,
    pub dunning_max_retries: Option<u32>,
    pub dunning_retry_interval_days: Option<u32>,
    pub module_id: Option<String>,
    pub updates_years: Option<u32>,
}

/// One pricing tier / edition of a product, for example `single-site`,
/// `multi-site`, or `agency`, with its own price, seat count, trial,
/// renewal terms, and feature list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingTier {
    pub id: String,
    pub product_id: String,
    pub slug: String,
    pub label: String,
    pub price: f64,
    pub renewal_price: Option<f64>,
    pub billing_period: Option<String>,
    pub trial_days: u32,
    /// Activation seats included with this tier.
    pub seats: i32,
    /// Maximum downloads allowed for the entitlement of this tier.
    pub download_limit: u32,
    pub features: Vec<String>,
    /// `active` or `disabled`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTier {
    pub slug: String,
    pub label: Option<String>,
    pub price: f64,
    pub renewal_price: Option<f64>,
    pub billing_period: Option<String>,
    pub trial_days: Option<u32>,
    pub seats: Option<i32>,
    pub download_limit: Option<u32>,
    pub features: Option<Vec<String>>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTier {
    pub slug: Option<String>,
    pub label: Option<String>,
    pub price: Option<f64>,
    pub renewal_price: Option<f64>,
    pub billing_period: Option<String>,
    pub trial_days: Option<u32>,
    pub seats: Option<i32>,
    pub download_limit: Option<u32>,
    pub features: Option<Vec<String>>,
    pub status: Option<String>,
}

/// A discount coupon: a percentage or a fixed amount off, optionally
/// limited to specific products, with an expiry and a usage limit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Coupon {
    pub id: String,
    /// Uppercase code, unique across coupons.
    pub code: String,
    /// `percent` or `fixed`.
    pub discount_type: String,
    /// Percentage (0 < value <= 100) or fixed amount in the order currency.
    pub discount_value: f64,
    pub expires_at: Option<DateTime<Utc>>,
    pub usage_limit: Option<u32>,
    pub used_count: u32,
    /// Empty means the coupon applies to every product.
    pub product_ids: Vec<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCoupon {
    pub code: String,
    pub discount_type: String,
    pub discount_value: f64,
    pub expires_at: Option<DateTime<Utc>>,
    pub usage_limit: Option<u32>,
    pub product_ids: Option<Vec<String>>,
    pub active: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateCoupon {
    pub discount_type: Option<String>,
    pub discount_value: Option<f64>,
    pub expires_at: Option<DateTime<Utc>>,
    pub usage_limit: Option<u32>,
    pub product_ids: Option<Vec<String>>,
    pub active: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PriceCalculateRequest {
    pub product_id: String,
    pub tier_id: Option<String>,
    pub quantity: Option<u32>,
    pub coupon_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CartItem {
    pub product_id: String,
    pub name: String,
    pub unit_price: f64,
    pub quantity: u32,
    pub line_total: f64,
    #[serde(default)]
    pub tier_id: Option<String>,
    #[serde(default)]
    pub tier_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cart {
    pub owner: String,
    pub items: Vec<CartItem>,
    pub currency: String,
    pub total: f64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddCartItem {
    pub product_id: String,
    pub quantity: Option<u32>,
    pub tier_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoveCartItem {
    pub product_id: Option<String>,
    pub tier_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderItem {
    pub product_id: String,
    pub name: String,
    pub unit_price: f64,
    pub quantity: u32,
    pub line_total: f64,
    pub license_product: bool,
    pub file_id: Option<String>,
    #[serde(default)]
    pub tier_id: Option<String>,
    #[serde(default)]
    pub tier_name: Option<String>,
    #[serde(default)]
    pub software_id: Option<String>,
    #[serde(default)]
    pub software_slug: Option<String>,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default = "default_pricing_model")]
    pub pricing_model: String,
    #[serde(default)]
    pub billing_period: Option<String>,
    /// Activation seats granted by the tier (1 when no tier is used).
    #[serde(default = "default_seats")]
    pub seats: i32,
    /// License expiry computed from the pricing model at checkout.
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// Maximum downloads granted by the tier.
    #[serde(default = "default_download_limit")]
    pub download_limit: u32,
    /// E-learning course this line buys a seat in (see
    /// `CheckoutItem::course_id`). Serialized on the order, so the
    /// `order.paid` / `store.order.completed` payloads carry it
    /// for the e-learning module's activation listener.
    #[serde(default)]
    pub course_id: Option<String>,
    /// Subscription plan snapshot, resolved at checkout from the
    /// product (or tier) so the subscription lifecycle can run
    /// without reading the catalogue again. Only meaningful for
    /// subscription items.
    #[serde(default)]
    pub trial_days: u32,
    #[serde(default = "default_proration_policy")]
    pub proration_policy: String,
    #[serde(default)]
    pub grace_period_days: u32,
    #[serde(default = "default_dunning_max_retries")]
    pub dunning_max_retries: u32,
    #[serde(default = "default_dunning_retry_interval_days")]
    pub dunning_retry_interval_days: u32,
    #[serde(default)]
    pub module_id: Option<String>,
}

fn default_seats() -> i32 {
    1
}

fn default_download_limit() -> u32 {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    /// Public payment reference for this order, in the single shared
    /// reference format (`PREFIX-12hex-12hex`, 30 characters; see
    /// `crate::core::reference_id`). Issued once at checkout and
    /// never changed afterwards: gateways reconcile against this
    /// value, while `id` stays the internal UUID primary key.
    /// `#[serde(default)]` keeps orders serialized before this field
    /// existed readable; such orders carry an empty reference until
    /// the integrator backfills them (see the batch report).
    #[serde(default)]
    pub reference: String,
    pub user_id: String,
    pub items: Vec<OrderItem>,
    pub subtotal: f64,
    pub discount_total: f64,
    pub total: f64,
    pub currency: String,
    #[serde(default)]
    pub coupon_code: Option<String>,
    /// The discount evaluation that priced this order: the pricing
    /// module's answer (full trace, `evaluator: "pricing"`) when
    /// it answered, or the store-local fallback trace
    /// (`evaluator: "store-local"`) with the reason the shared
    /// evaluator could not price it. Exactly one evaluator's math
    /// is ever applied to an order; this field records which one.
    /// Store money is float major units (pre-existing); the pricing
    /// answer arrives as integer scaled units and is converted to
    /// cents with half-up rounding at this boundary.
    #[serde(default)]
    pub discount_evaluation: Option<Value>,
    /// Vendor / company of the first line item, for payout attribution.
    #[serde(default)]
    pub company_id: Option<String>,
    /// End of the free trial, when any line item has trial days.
    #[serde(default)]
    pub trial_until: Option<DateTime<Utc>>,
    /// `pending`, `payment_pending`, `pending_verification`, `paid`,
    /// `completed`, `refund_pending`, `refunded`, `cancelled`.
    pub status: String,
    pub provider: Option<String>,
    /// Gateway / bank-transfer transaction id that paid this order,
    /// recorded by the `payment.captured` listener. Absent until the
    /// order is paid.
    #[serde(default)]
    pub payment_reference: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One paid subscription: the commercial record of a recurring
/// purchase. Created by the store when a subscription order becomes
/// paid (see `activate_subscription_for_paid_order`); the billing
/// module runs the lifecycle (renewals, dunning, grace, reminders)
/// against this record through `subscription.lifecycle.apply`
/// events, and this module applies those decisions here so there is
/// exactly one authoritative copy.
///
/// Statuses: `trialing` (inside the free trial), `active`,
/// `past_due` (a renewal payment failed and the subscription is
/// inside its grace period or dunning window), `cancelled`.
/// A subscription cancelled at period end keeps its status
/// (`active` or `trialing`) with `cancel_at_period_end` set until
/// the period ends, so access continues until then.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub user_id: String,
    #[serde(default)]
    pub company_id: Option<String>,
    pub product_id: String,
    #[serde(default)]
    pub tier_id: Option<String>,
    pub order_id: String,
    /// Sellable module this subscription entitles the buyer to,
    /// when the product is a module offering.
    #[serde(default)]
    pub module_id: Option<String>,
    /// `trialing`, `active`, `past_due`, or `cancelled`.
    pub status: String,
    /// `weekly`, `monthly`, or `yearly`.
    pub billing_period: String,
    /// Price charged per period, in major units.
    pub price: f64,
    pub currency: String,
    /// Free trial length in days (0 means no trial).
    pub trial_days: u32,
    /// End of the free trial, when trialing.
    #[serde(default)]
    pub trial_end: Option<DateTime<Utc>>,
    pub current_period_start: DateTime<Utc>,
    pub current_period_end: DateTime<Utc>,
    /// Mid-cycle plan-change policy: `prorate`, `no_prorate`, or
    /// `credit_only`.
    pub proration_policy: String,
    pub grace_period_days: u32,
    pub dunning_max_retries: u32,
    pub dunning_retry_interval_days: u32,
    /// True when the subscription was cancelled with effect at the
    /// end of the current period.
    #[serde(default)]
    pub cancel_at_period_end: bool,
    #[serde(default)]
    pub cancelled_at: Option<DateTime<Utc>>,
    /// Plan change waiting to take effect at the next period end
    /// (`no_prorate` and `credit_only` changes, and a `prorate`
    /// change is applied immediately instead, so it never lands
    /// here).
    #[serde(default)]
    pub pending_product_id: Option<String>,
    #[serde(default)]
    pub pending_tier_id: Option<String>,
    #[serde(default)]
    pub pending_price: Option<f64>,
    #[serde(default)]
    pub pending_billing_period: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChangePlanRequest {
    pub product_id: String,
    #[serde(default)]
    pub tier_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CancelSubscriptionRequest {
    /// When true (default) the subscription stays active until the
    /// end of the current period; when false it ends immediately.
    #[serde(default = "default_true")]
    pub cancel_at_period_end: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckoutItem {
    pub product_id: String,
    pub quantity: Option<u32>,
    pub tier_id: Option<String>,
    /// E-learning course this line buys a seat in, when the buyer
    /// is completing a paid course enrollment. The course id is
    /// carried onto the order item and then onto the `order.paid` /
    /// `store.order.completed` payloads, which is what activates
    /// the pending enrollment in the e-learning module. The store
    /// never invents this id: it comes from the enrollment
    /// purchase flow, and it must be a UUID.
    #[serde(default)]
    pub course_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckoutRequest {
    pub user_id: Option<String>,
    pub items: Option<Vec<CheckoutItem>>,
    pub coupon_code: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PayRequest {
    pub provider: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateOrder {
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RefundRequest {
    pub reason: Option<String>,
}

/// A download entitlement created from an issued license: it links a
/// license to a software release and caps how many times the release
/// may be downloaded with that license.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadEntitlement {
    pub id: String,
    pub license_id: Option<String>,
    pub license_key: Option<String>,
    pub order_id: String,
    pub user_id: String,
    pub product_id: String,
    #[serde(default)]
    pub tier_id: Option<String>,
    #[serde(default)]
    pub software_id: Option<String>,
    #[serde(default)]
    pub software_slug: Option<String>,
    #[serde(default)]
    pub release_id: Option<String>,
    pub download_limit: u32,
    pub downloads_used: u32,
    /// `active`, `expired`, or `revoked`.
    pub status: String,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadRecord {
    pub id: String,
    pub license_id: String,
    pub release_id: String,
    #[serde(default)]
    pub software_slug: Option<String>,
    #[serde(default)]
    pub order_id: Option<String>,
    pub downloaded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DownloadLinkRequest {
    pub release_id: Option<String>,
}

/// Mirror of the licenses this store caused to be issued (from the
/// `order.created` hook results), so refunds can suspend them locally
/// and report them to the licensing module through `store.order.refunded`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssuedLicense {
    pub license_id: String,
    pub license_key: String,
    pub product: String,
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub company_id: Option<String>,
    /// `active` or `suspended`.
    pub status: String,
    pub created_at: DateTime<Utc>,
}

/// One software entry for the update API: the latest known version of a
/// plugin, theme, or other software package, with its compatibility
/// metadata and the release/file a valid license may download.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub slug: String,
    pub name: String,
    /// `plugin`, `theme`, or `software`.
    pub kind: String,
    #[serde(default)]
    pub product_id: Option<String>,
    /// Product name as it appears on the issued license; the update
    /// check validates the license key against this name.
    pub product_name: String,
    pub latest_version: String,
    #[serde(default)]
    pub release_id: Option<String>,
    #[serde(default)]
    pub file_id: Option<String>,
    pub changelog: String,
    #[serde(default)]
    pub requires_wp: Option<String>,
    #[serde(default)]
    pub tested_wp: Option<String>,
    #[serde(default)]
    pub requires_php: Option<String>,
    pub license_required: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCatalogEntry {
    pub slug: String,
    pub name: String,
    pub kind: String,
    pub product_id: Option<String>,
    pub product_name: Option<String>,
    pub latest_version: String,
    pub release_id: Option<String>,
    pub file_id: Option<String>,
    pub changelog: Option<String>,
    pub requires_wp: Option<String>,
    pub tested_wp: Option<String>,
    pub requires_php: Option<String>,
    pub license_required: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateCatalogEntry {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub product_id: Option<String>,
    pub product_name: Option<String>,
    pub latest_version: Option<String>,
    pub release_id: Option<String>,
    pub file_id: Option<String>,
    pub changelog: Option<String>,
    pub requires_wp: Option<String>,
    pub tested_wp: Option<String>,
    pub requires_php: Option<String>,
    pub license_required: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateCheckRequest {
    pub slug: String,
    pub current_version: String,
    pub license_key: Option<String>,
    pub instance_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SignedDownloadQuery {
    pub license_id: String,
    pub release_id: String,
    pub expires: i64,
    pub sig: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PackageQuery {
    pub license_key: Option<String>,
}

// ---------------------------------------------------------------------------
// Digital products (ebooks and other downloadable digital goods)
// ---------------------------------------------------------------------------

/// Capability declaration for the product registry, for the
/// integrator: digital-products selling is a sellable store
/// capability, category `commerce`. The store module itself stays
/// `available_as_product() == false` (see the note on the `Module`
/// implementation below): the store is the marketplace, not a
/// sellable module, so the flag lives here as data for the
/// integrator instead of on the module.
pub const DIGITAL_PRODUCTS_AVAILABLE_AS_PRODUCT: bool = true;
pub const DIGITAL_PRODUCTS_PRODUCT_CATEGORY: &str = "commerce";

/// Capability names checked through the `role.check` hook for the
/// digital-products routes (same fail-closed shape as the products
/// module: no answer means not allowed).
const PERM_DIGITAL_READ: &str = "store.digital:read";
const PERM_DIGITAL_MANAGE: &str = "store.digital:manage";

/// Configurable defaults for digital products. These are seed values
/// only: an administrator can change every one of them through
/// `PUT /api/v1/store/digital/settings`, and the live values live in
/// `StoreState::digital_defaults`.
const DEFAULT_DIGITAL_DOWNLOAD_LIMIT: u32 = 10;
const DEFAULT_SAMPLE_RATE_LIMIT_PER_HOUR: u32 = 5;
const DEFAULT_SAMPLE_RATE_LIMIT_WINDOW_MINUTES: u32 = 60;
const DEFAULT_ROYALTY_KIND: &str = "percent";
/// No royalty accrues unless the seller configures one: the default
/// royalty value is zero on purpose, so money is never promised to
/// an author or vendor by accident.
const DEFAULT_ROYALTY_VALUE: f64 = 0.0;

fn default_royalty_kind() -> String {
    DEFAULT_ROYALTY_KIND.to_string()
}

/// One digital product type from the admin-extensible registry.
/// The type list is data, not a hard-coded enum: administrators add,
/// rename, disable, or remove types through the type endpoints, and
/// the seed rows below are configurable defaults, not a closed set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigitalProductType {
    /// Slug, for example `ebook`.
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateDigitalProductType {
    pub id: String,
    pub label: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateDigitalProductType {
    pub label: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
}

/// A digital profile on a store product: the product stays the
/// sellable record (price, currency, checkout), and this profile
/// carries everything that makes it a digital good.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigitalProduct {
    pub id: String,
    /// The store product this profile sells.
    pub product_id: String,
    /// Type slug from the admin-extensible digital type registry
    /// (seeded with ebook, audiobook, template, course-pack, and
    /// digital-asset; administrators can add more).
    pub type_id: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub edition: Option<String>,
    /// ISBN-10 or ISBN-13, hyphens and spaces allowed.
    #[serde(default)]
    pub isbn: Option<String>,
    /// Language tag, for example `en` or `id`.
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub pages: Option<u32>,
    /// Delivered formats, for example `["pdf", "epub"]`.
    #[serde(default)]
    pub formats: Vec<String>,
    /// Storage reference of the cover image (a tokenized storage
    /// id, never file bytes).
    #[serde(default)]
    pub cover_storage_id: Option<String>,
    /// When true, anyone may download the free sample file
    /// (rate-limited and logged).
    #[serde(default)]
    pub sample_allowed: bool,
    /// Downloads allowed per purchased file. 0 means "use the
    /// configured default".
    #[serde(default)]
    pub download_limit: u32,
    /// Royalty rule for this product: `percent` or `fixed`.
    #[serde(default = "default_royalty_kind")]
    pub royalty_kind: String,
    /// Percentage (0..=100) or fixed amount per unit sold, in the
    /// order currency.
    #[serde(default)]
    pub royalty_value: f64,
    /// Author / vendor who earns the royalty (user id).
    #[serde(default)]
    pub royalty_payee: Option<String>,
    /// Vendor company that earns the royalty, when the payee is a
    /// company rather than a single author.
    #[serde(default)]
    pub royalty_payee_company_id: Option<String>,
    /// `draft` or `published`. Only published products are listed
    /// on the public storefront.
    #[serde(default = "default_product_status")]
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateDigitalProduct {
    pub product_id: String,
    pub type_id: String,
    pub author: Option<String>,
    pub publisher: Option<String>,
    pub edition: Option<String>,
    pub isbn: Option<String>,
    pub language: Option<String>,
    pub pages: Option<u32>,
    pub formats: Option<Vec<String>>,
    pub cover_storage_id: Option<String>,
    pub sample_allowed: Option<bool>,
    pub download_limit: Option<u32>,
    pub royalty_kind: Option<String>,
    pub royalty_value: Option<f64>,
    pub royalty_payee: Option<String>,
    pub royalty_payee_company_id: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateDigitalProduct {
    pub type_id: Option<String>,
    pub author: Option<String>,
    pub publisher: Option<String>,
    pub edition: Option<String>,
    pub isbn: Option<String>,
    pub language: Option<String>,
    pub pages: Option<u32>,
    pub formats: Option<Vec<String>>,
    pub cover_storage_id: Option<String>,
    pub sample_allowed: Option<bool>,
    pub download_limit: Option<u32>,
    pub royalty_kind: Option<String>,
    pub royalty_value: Option<f64>,
    pub royalty_payee: Option<String>,
    pub royalty_payee_company_id: Option<String>,
    pub status: Option<String>,
}

/// One deliverable file of a digital product, stored in the storage
/// module and referenced here by its storage id only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigitalProductFile {
    pub id: String,
    pub digital_product_id: String,
    /// Storage reference of the delivered file (a tokenized storage
    /// id, never file bytes). This id doubles as the `release_id`
    /// in the signed download flow, so the existing signed-link
    /// machinery serves digital files without a second system.
    pub storage_file_id: String,
    pub format: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub file_size_bytes: Option<u64>,
    /// Content hash (hex digest) of the file, when the seller
    /// supplied one. Only the digest is stored, never the content.
    #[serde(default)]
    pub checksum: Option<String>,
    #[serde(default)]
    pub is_primary: bool,
    #[serde(default)]
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateDigitalFile {
    pub storage_file_id: String,
    pub format: String,
    pub label: Option<String>,
    pub file_size_bytes: Option<u64>,
    pub checksum: Option<String>,
    pub is_primary: Option<bool>,
    pub sort_order: Option<i32>,
}

/// One free excerpt / sample file of a digital product, served
/// publicly when the seller allows it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigitalProductPreview {
    pub id: String,
    pub digital_product_id: String,
    pub storage_file_id: String,
    pub format: String,
    #[serde(default)]
    pub label: Option<String>,
    /// The seller allows public download of this sample.
    #[serde(default)]
    pub allowed: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateDigitalPreview {
    pub storage_file_id: String,
    pub format: String,
    pub label: Option<String>,
    pub allowed: Option<bool>,
}

/// One accrued royalty: a per-sale share owed to an author or
/// vendor for a digital product. The ledger only records what is
/// owed; moving money is the billing module's job (see
/// `DigitalPayoutRequest`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigitalRoyalty {
    pub id: String,
    pub digital_product_id: String,
    pub product_id: String,
    pub order_id: String,
    pub buyer_user_id: String,
    #[serde(default)]
    pub payee: Option<String>,
    #[serde(default)]
    pub payee_company_id: Option<String>,
    pub amount: f64,
    pub currency: String,
    /// Royalty rule snapshot: `percent` or `fixed`.
    pub royalty_kind: String,
    pub royalty_value: f64,
    /// `accrued`, `payout_requested`, `approved`, `paid`,
    /// `rejected`, or `reversed`.
    pub status: String,
    #[serde(default)]
    pub payout_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One payout request grouping royalty entries for a single payee.
/// Lifecycle: `requested` -> `approved` -> `paid`, or `rejected`
/// from `requested` / `approved`. Requesting and approving only
/// emit events for billing (`store.digital.payout.requested` and
/// friends); the payout itself (moving money) happens in billing,
/// and `mark-paid` only records that billing moved it. This mirrors
/// the affiliates payout precedent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigitalPayoutRequest {
    pub id: String,
    #[serde(default)]
    pub payee: Option<String>,
    #[serde(default)]
    pub payee_company_id: Option<String>,
    pub currency: String,
    pub amount: f64,
    pub royalty_ids: Vec<String>,
    /// `requested`, `approved`, `paid`, or `rejected`.
    pub status: String,
    pub requested_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateDigitalPayout {
    pub royalty_ids: Vec<String>,
}

/// One download of a digital file, for the per-buyer download
/// history: purchases (through the signed download flow) and free
/// samples alike. The watermark columns record what buyer identity
/// was embedded into the delivered file's metadata, when the
/// format supports metadata at all.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigitalDownload {
    pub id: String,
    pub digital_product_id: String,
    #[serde(default)]
    pub file_id: Option<String>,
    #[serde(default)]
    pub preview_id: Option<String>,
    pub buyer_user_id: String,
    #[serde(default)]
    pub order_id: Option<String>,
    /// Purchase grant id (the `license_id` carried by the signed
    /// download URL) for purchases; absent for samples.
    #[serde(default)]
    pub grant_id: Option<String>,
    /// `purchase` or `sample`.
    pub kind: String,
    /// Buyer identity embedded into the delivered file metadata
    /// (watermark subject), when the format supports metadata.
    #[serde(default)]
    pub watermark_subject: Option<String>,
    pub watermark_applied: bool,
    #[serde(default)]
    pub watermark_formats: Vec<String>,
    pub downloaded_at: DateTime<Utc>,
}

/// One watermarkable format, from the admin-editable registry.
/// `metadata_supported` says whether the format has standard
/// document metadata a buyer identity can be embedded into; the
/// registry is data, so new formats are added without code changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatermarkFormat {
    pub format: String,
    pub metadata_supported: bool,
    pub metadata_fields: Vec<String>,
    pub notes: String,
    pub updated_at: DateTime<Utc>,
}

/// Admin-editable digital defaults. Every field is a configurable
/// default: the values below seed the state at startup and the live
/// values are changed through `PUT /api/v1/store/digital/settings`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigitalDefaults {
    pub default_download_limit: u32,
    pub sample_rate_limit_per_hour: u32,
    pub sample_rate_limit_window_minutes: u32,
    pub default_royalty_kind: String,
    pub default_royalty_value: f64,
    pub watermark_enabled: bool,
}

impl Default for DigitalDefaults {
    fn default() -> Self {
        Self {
            default_download_limit: DEFAULT_DIGITAL_DOWNLOAD_LIMIT,
            sample_rate_limit_per_hour: DEFAULT_SAMPLE_RATE_LIMIT_PER_HOUR,
            sample_rate_limit_window_minutes: DEFAULT_SAMPLE_RATE_LIMIT_WINDOW_MINUTES,
            default_royalty_kind: DEFAULT_ROYALTY_KIND.to_string(),
            default_royalty_value: DEFAULT_ROYALTY_VALUE,
            watermark_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateDigitalSettings {
    pub default_download_limit: Option<u32>,
    pub sample_rate_limit_per_hour: Option<u32>,
    pub sample_rate_limit_window_minutes: Option<u32>,
    pub default_royalty_kind: Option<String>,
    pub default_royalty_value: Option<f64>,
    pub watermark_enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateWatermarkFormat {
    pub metadata_supported: Option<bool>,
    pub metadata_fields: Option<Vec<String>>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DigitalDownloadLinkRequest {
    pub file_id: Option<String>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct StoreState {
    hooks: RwLock<Arc<HookBus>>,
    products: DashMap<String, Product>,
    tiers: DashMap<String, PricingTier>,
    coupons: DashMap<String, Coupon>,
    carts: DashMap<String, Cart>,
    orders: DashMap<String, Order>,
    /// Paid subscriptions, keyed by subscription id. Authoritative
    /// for the subscription lifecycle; billing keeps a mirror and
    /// drives decisions through `subscription.lifecycle.apply`.
    subscriptions: DashMap<String, Subscription>,
    entitlements: DashMap<String, DownloadEntitlement>,
    downloads: DashMap<String, DownloadRecord>,
    catalog: DashMap<String, CatalogEntry>,
    issued_licenses: DashMap<String, IssuedLicense>,
    digital_products: DashMap<String, DigitalProduct>,
    /// Admin-extensible digital type registry, seeded with the
    /// configurable default types (see `seed_digital_types`).
    digital_types: DashMap<String, DigitalProductType>,
    digital_files: DashMap<String, DigitalProductFile>,
    digital_previews: DashMap<String, DigitalProductPreview>,
    digital_royalties: DashMap<String, DigitalRoyalty>,
    digital_payouts: DashMap<String, DigitalPayoutRequest>,
    digital_downloads: DashMap<String, DigitalDownload>,
    /// Admin-editable watermark format registry, seeded with the
    /// configurable default formats (see `seed_watermark_formats`).
    watermark_formats: DashMap<String, WatermarkFormat>,
    digital_defaults: RwLock<DigitalDefaults>,
    /// Sample-download timestamps per identity and preview, for the
    /// sample rate limit. Entries are pruned on each check.
    sample_downloads: DashMap<String, Vec<DateTime<Utc>>>,
    /// Admin-editable payment reference settings (default prefix and
    /// per-product prefix map; see `crate::core::reference_id`).
    /// Read at checkout; changed through
    /// `GET/PUT /api/v1/store/reference-settings`.
    reference_settings: RwLock<ReferenceIdSettings>,
}

impl StoreState {
    fn new() -> Self {
        let state = Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            products: DashMap::new(),
            tiers: DashMap::new(),
            coupons: DashMap::new(),
            carts: DashMap::new(),
            orders: DashMap::new(),
            subscriptions: DashMap::new(),
            entitlements: DashMap::new(),
            downloads: DashMap::new(),
            catalog: DashMap::new(),
            issued_licenses: DashMap::new(),
            digital_products: DashMap::new(),
            digital_types: DashMap::new(),
            digital_files: DashMap::new(),
            digital_previews: DashMap::new(),
            digital_royalties: DashMap::new(),
            digital_payouts: DashMap::new(),
            digital_downloads: DashMap::new(),
            watermark_formats: DashMap::new(),
            digital_defaults: RwLock::new(DigitalDefaults::default()),
            sample_downloads: DashMap::new(),
            reference_settings: RwLock::new(ReferenceIdSettings::default()),
        };
        for digital_type in seed_digital_types() {
            state
                .digital_types
                .insert(digital_type.id.clone(), digital_type);
        }
        for format in seed_watermark_formats() {
            state
                .watermark_formats
                .insert(format.format.clone(), format);
        }
        state
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    fn emit_with_results(&self, event: &str, data: Value) -> HookContext {
        self.bus().emit(event, data)
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

// ---------------------------------------------------------------------------
// Batch 18: payment reference ids (BEGIN — append-only section)
// ---------------------------------------------------------------------------
//
// Every order issued by this module carries one public payment
// reference in the single shared format (`PREFIX-12hex-12hex`, 30
// characters; see `crate::core::reference_id`). The internal `id`
// stays a UUID; the reference is what gateways see and reconcile
// against. The default prefix and the per-product prefix map are
// admin-editable settings held on `StoreState` (durable form: the
// `reference_id_settings` table in the migration snippet in the
// batch report). A single-product order uses that product's
// configured prefix; a mixed order uses the default prefix.
// References are issued once at checkout and never changed: the
// update-order endpoint deliberately has no reference field.

/// Capability needed to read the reference settings.
const PERM_REFERENCE_READ: &str = "store.reference:read";

/// Capability needed to change the reference settings.
const PERM_REFERENCE_MANAGE: &str = "store.reference:manage";

/// Snapshot of the current reference settings.
fn reference_settings(state: &StoreState) -> ReferenceIdSettings {
    state
        .reference_settings
        .read()
        .expect("reference settings lock poisoned")
        .clone()
}

/// Issue the reference for a checkout: the single product's
/// configured prefix when the order holds exactly one product,
/// otherwise the default prefix.
fn new_order_reference(state: &StoreState, items: &[OrderItem]) -> String {
    let settings = reference_settings(state);
    let product_id = if items.len() == 1 {
        Some(items[0].product_id.as_str())
    } else {
        None
    };
    reference_id::generate_for(&settings, product_id, None)
}

/// Admin input for `PUT /api/v1/store/reference-settings`. All fields
/// are optional and applied together: `default_prefix` replaces the
/// default prefix, `product_prefixes` replaces the whole per-product
/// map, and `remove_product_prefixes` deletes individual overrides.
/// Every prefix is normalized with `format_prefix` before it is
/// stored, so an administrator can send `hstg` and the stored value
/// is `HSTG`.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateReferenceSettings {
    pub default_prefix: Option<String>,
    pub product_prefixes: Option<HashMap<String, String>>,
    pub remove_product_prefixes: Option<Vec<String>>,
}

/// Read the reference settings (capability `store.reference:read`).
async fn get_reference_settings(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<ReferenceIdSettings> = (|| {
        require_digital_permission(&state, &req, PERM_REFERENCE_READ)?;
        Ok(reference_settings(&state))
    })();
    json::respond(result, false)
}

/// Update the reference settings (capability
/// `store.reference:manage`). Already-issued references keep
/// validating: changing a prefix never rewrites an issued id.
async fn update_reference_settings(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<UpdateReferenceSettings>,
) -> HttpResponse {
    let result: UnifiedResult<ReferenceIdSettings> = (|| {
        require_digital_permission(&state, &req, PERM_REFERENCE_MANAGE)?;
        let input = body.into_inner();
        let mut candidate = reference_settings(&state);
        if let Some(prefix) = input.default_prefix {
            candidate.default_prefix = reference_id::format_prefix(&prefix);
        }
        if let Some(prefixes) = input.product_prefixes {
            let mut next = ReferenceIdSettings::default();
            next.default_prefix = candidate.default_prefix.clone();
            for (key, prefix) in prefixes {
                next.set_product_prefix(&key, &prefix).map_err(|problem| {
                    UnifiedError::bad_request(problem)
                })?;
            }
            candidate = next;
        }
        if let Some(keys) = input.remove_product_prefixes {
            for key in keys {
                candidate.remove_product_prefix(&key);
            }
        }
        let problems = candidate.validate();
        if !problems.is_empty() {
            return Err(UnifiedError::unprocessable(format!(
                "Reference settings are invalid: {}",
                problems.join("; ")
            )));
        }
        {
            let mut settings = state
                .reference_settings
                .write()
                .expect("reference settings lock poisoned");
            *settings = candidate.clone();
        }
        state.emit(
            reference_id::EVENT_REFERENCE_SETTINGS_CHANGED,
            serde_json::to_value(&candidate).unwrap_or(Value::Null),
        );
        Ok(candidate)
    })();
    json::respond(result, false)
}

// Batch 18: payment reference ids (END)
// ---------------------------------------------------------------------------

fn round_money(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
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

fn valid_kind(kind: &str) -> bool {
    matches!(kind, "theme" | "plugin" | "software")
}

fn valid_currency(currency: &str) -> bool {
    currency.len() == 3 && currency.chars().all(|c| c.is_ascii_alphabetic())
}

fn valid_pricing_model(model: &str) -> bool {
    matches!(model, "one_time" | "subscription")
}

fn valid_billing_period(period: &str) -> bool {
    matches!(period, "monthly" | "yearly" | "weekly")
}

/// Subscription plan settings as resolved at checkout: the
/// proration policy must be one of `prorate`, `no_prorate`,
/// `credit_only`; grace days 0..=90; dunning retries 0..=10;
/// retry interval 1..=30 days. Bounds match the products module.
fn validate_subscription_settings(
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

fn valid_product_status(status: &str) -> bool {
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
fn allowed_transition(from: &str, to: &str) -> bool {
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
fn license_expiry_for(
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
            _ => Some(current + chrono::Duration::days(30)),
        },
        _ => Some(current + chrono::Duration::days(365 * updates_years.unwrap_or(1) as i64)),
    }
}

/// Length of one billing period in days, using the same convention
/// as `license_expiry_for`: weekly 7, monthly 30, yearly 365.
fn period_days(billing_period: &str) -> i64 {
    match billing_period {
        "weekly" => 7,
        "yearly" => 365,
        _ => 30,
    }
}

/// End of the billing period that starts at `start`.
fn period_end(start: DateTime<Utc>, billing_period: &str) -> DateTime<Utc> {
    start + chrono::Duration::days(period_days(billing_period))
}

/// True when the subscription currently grants access: trialing or
/// active inside the current period; past-due inside the grace
/// period; cancelled at period end inside the current period.
/// A subscription cancelled immediately, or past its grace period,
/// grants nothing.
fn subscription_grants_access(subscription: &Subscription, at: DateTime<Utc>) -> bool {
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

/// Proration for a mid-cycle plan change, in major units.
///
/// `remaining_days` is the time left in the current period and
/// `period_total_days` its full length. The credit is the unused
/// share of the old price; the charge is the new price for the same
/// remaining share of the new period. Both are rounded to cents;
/// the net amount is `charge - credit` and may be negative (a
/// downgrade credits more than it charges). `no_prorate` changes
/// return zeroes: nothing is charged or credited mid-cycle.
fn proration_amounts(
    policy: &str,
    old_price: f64,
    new_price: f64,
    remaining_days: f64,
    period_total_days: f64,
    new_period_total_days: f64,
) -> (f64, f64, f64) {
    if policy == "no_prorate" || remaining_days <= 0.0 || period_total_days <= 0.0 {
        return (0.0, 0.0, 0.0);
    }
    let share = (remaining_days / period_total_days).clamp(0.0, 1.0);
    let credit = round_money(old_price * share);
    let new_share = if new_period_total_days > 0.0 {
        (remaining_days / new_period_total_days).clamp(0.0, 1.0)
    } else {
        0.0
    };
    // `credit_only` credits the unused time but charges nothing
    // until the next period; `prorate` charges the new plan for the
    // remaining time as well.
    let charge = if policy == "credit_only" {
        0.0
    } else {
        round_money(new_price * new_share)
    };
    let net = round_money(charge - credit);
    (credit, charge, net)
}

/// Cart owner: `x-session-id` header, then `x-user-id`, then `guest`.
fn owner_of(req: &HttpRequest) -> String {
    for header in ["x-session-id", "x-user-id"] {
        if let Some(value) = req
            .headers()
            .get(header)
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.trim().is_empty())
        {
            return value.trim().to_string();
        }
    }
    "guest".to_string()
}

/// Caller identities from the request headers (`x-user-id` and
/// `x-session-id`). A caller owns a record when one of these matches
/// the record owner. This is an ownership check on self-asserted
/// header identity, not authentication: it stops cross-user mistakes
/// and becomes real enforcement once the API layer validates the
/// headers against a session or API key. A caller with no identity
/// header matches only `guest`-owned records, so anonymous calls can
/// no longer act on another user's order; operator calls must send
/// `x-user-id` until the API layer provides a real admin identity.
fn caller_identities(req: &HttpRequest) -> Vec<String> {
    ["x-user-id", "x-session-id"]
        .into_iter()
        .filter_map(|header| {
            req.headers()
                .get(header)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .collect()
}

/// True when the caller may act on a record owned by `owner`: no
/// identity headers (see `caller_identities`), or one of the caller
/// identities matches the owner. An anonymous `guest` owner matches a
/// caller with no identity headers.
fn caller_owns(req: &HttpRequest, owner: &str) -> bool {
    let identities = caller_identities(req);
    if identities.is_empty() {
        return owner == "guest";
    }
    identities.iter().any(|identity| identity == owner)
}

/// Reject a mutating order call from a caller who does not own the
/// order (see `caller_owns`).
fn require_order_owner(req: &HttpRequest, order: &Order) -> UnifiedResult<()> {
    if caller_owns(req, &order.user_id) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Order belongs to a different user",
        ))
    }
}

// ---------------------------------------------------------------------------
// Digital products: seeds, capability checks, validation, watermarking
// ---------------------------------------------------------------------------

/// Seed rows for the digital type registry. Configurable defaults
/// only: administrators can add, rename, disable, or remove types
/// through the type endpoints, and the migration seeds the same
/// five rows as data so the database copy matches this seed.
fn seed_digital_types() -> Vec<DigitalProductType> {
    let timestamp = now();
    [
        (
            "ebook",
            "Ebook",
            "A digital book delivered as a document file.",
        ),
        (
            "audiobook",
            "Audiobook",
            "A narrated audio edition of a book.",
        ),
        (
            "template",
            "Template",
            "A reusable document, design, or code template.",
        ),
        (
            "course-pack",
            "Course pack",
            "A bundled set of course materials.",
        ),
        (
            "digital-asset",
            "Digital asset",
            "A downloadable digital asset such as graphics, fonts, or data.",
        ),
    ]
    .into_iter()
    .map(|(id, label, description)| DigitalProductType {
        id: id.to_string(),
        label: label.to_string(),
        description: description.to_string(),
        enabled: true,
        created_at: timestamp,
        updated_at: timestamp,
    })
    .collect()
}

/// Seed rows for the watermark format registry. Configurable
/// defaults only: administrators can add formats or change what
/// metadata is embedded through the watermark-format endpoints, and
/// the migration seeds the same rows as data.
fn seed_watermark_formats() -> Vec<WatermarkFormat> {
    let timestamp = now();
    [
        (
            "pdf",
            true,
            vec!["Author", "Creator", "Keywords"],
            "Buyer identity is written into the PDF document information dictionary.",
        ),
        (
            "epub",
            true,
            vec!["dc:identifier", "dc:creator"],
            "Buyer identity is written into the EPUB package metadata.",
        ),
        (
            "mobi",
            true,
            vec!["EXTH author", "EXTH identifier"],
            "Buyer identity is written into the MOBI EXTH records where the writer supports them.",
        ),
        (
            "mp3",
            true,
            vec!["ID3 comment", "ID3 artist"],
            "Buyer identity is written into the ID3 comment frame.",
        ),
        (
            "m4b",
            true,
            vec!["copyright", "comment"],
            "Buyer identity is written into the audiobook metadata atoms.",
        ),
        (
            "mp4",
            true,
            vec!["copyright", "comment"],
            "Buyer identity is written into the video metadata atoms.",
        ),
        (
            "zip",
            false,
            Vec::<&str>::new(),
            "Archives carry no standard document metadata; the buyer is recorded in the delivery manifest instead.",
        ),
    ]
    .into_iter()
    .map(|(format, metadata_supported, metadata_fields, notes)| WatermarkFormat {
        format: format.to_string(),
        metadata_supported,
        metadata_fields: metadata_fields
            .into_iter()
            .map(str::to_string)
            .collect(),
        notes: notes.to_string(),
        updated_at: timestamp,
    })
    .collect()
}

/// Caller identity for the capability-checked digital routes, from
/// the headers every capability-checked module in this codebase
/// uses: `X-User-Id` identifies the actor, `X-Role` carries the
/// role slug or id the capability check runs against.
struct DigitalCaller {
    actor: String,
    role: String,
}

fn digital_caller(req: &HttpRequest) -> UnifiedResult<DigitalCaller> {
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
    Ok(DigitalCaller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
/// The store keeps only the hook bus (no `AppContext`), so the check
/// runs through `state.bus().emit`, the same hook other modules
/// reach through the context.
fn digital_has_capability(state: &StoreState, role: &str, permission: &str) -> bool {
    let context = state.bus().emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_digital_permission(
    state: &StoreState,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<DigitalCaller> {
    let caller = digital_caller(req)?;
    if digital_has_capability(state, &caller.role, permission) {
        Ok(caller)
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{}\" does not hold permission \"{}\"",
            caller.role, permission
        )))
    }
}

fn require_digital_manage(
    state: &StoreState,
    req: &HttpRequest,
) -> UnifiedResult<DigitalCaller> {
    require_digital_permission(state, req, PERM_DIGITAL_MANAGE)
}

fn require_digital_read(
    state: &StoreState,
    req: &HttpRequest,
) -> UnifiedResult<DigitalCaller> {
    require_digital_permission(state, req, PERM_DIGITAL_READ)
}

/// True when the caller holds the manage permission (used to widen
/// a self-service read to another user's records). Missing identity
/// headers mean "no", never an error: this is a widening check, not
/// the gate itself.
fn digital_can_manage(state: &StoreState, req: &HttpRequest) -> bool {
    let Ok(caller) = digital_caller(req) else {
        return false;
    };
    digital_has_capability(state, &caller.role, PERM_DIGITAL_MANAGE)
}

/// Gate for the intentionally public storefront reads (public
/// digital catalog, previews, sample download). The API layer still
/// denies these endpoints by default until an administrator creates
/// an allow rule (see `api_manager`); inside the handler, a caller
/// that presents a role must hold `store.digital:read`, while an
/// anonymous caller only ever sees published products and
/// seller-allowed samples. The check runs on every call so the
/// public surface is never an unguarded code path.
fn require_digital_public(state: &StoreState, req: &HttpRequest) -> UnifiedResult<()> {
    let Some(role) = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    if digital_has_capability(state, role, PERM_DIGITAL_READ) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{PERM_DIGITAL_READ}\""
        )))
    }
}

fn valid_type_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_format(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 12
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn valid_isbn(value: &str) -> bool {
    let compact: String = value
        .chars()
        .filter(|c| *c != '-' && *c != ' ')
        .collect();
    if compact.len() != 10 && compact.len() != 13 {
        return false;
    }
    let (head, tail) = compact.split_at(compact.len() - 1);
    if !head.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let tail = tail.chars().next().expect("split leaves one char");
    if compact.len() == 10 {
        tail.is_ascii_digit() || tail == 'X' || tail == 'x'
    } else {
        tail.is_ascii_digit()
    }
}

fn valid_language(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    if parts.len() > 2 {
        return false;
    }
    let language = parts[0];
    if language.len() < 2
        || language.len() > 3
        || !language.chars().all(|c| c.is_ascii_lowercase())
    {
        return false;
    }
    match parts.get(1) {
        None => true,
        Some(region) => {
            region.len() == 2 && region.chars().all(|c| c.is_ascii_uppercase())
        }
    }
}

fn valid_royalty_kind(kind: &str) -> bool {
    matches!(kind, "percent" | "fixed")
}

fn validate_royalty(kind: &str, value: f64) -> UnifiedResult<()> {
    if !valid_royalty_kind(kind) {
        return Err(UnifiedError::bad_request(
            "Royalty kind must be one of: percent, fixed",
        ));
    }
    if !value.is_finite() || value < 0.0 {
        return Err(UnifiedError::bad_request(
            "Royalty value must be a non-negative number",
        ));
    }
    if kind == "percent" && value > 100.0 {
        return Err(UnifiedError::bad_request(
            "Percentage royalty must be at most 100",
        ));
    }
    Ok(())
}

fn validate_digital_metadata(
    isbn: Option<&str>,
    language: Option<&str>,
    pages: Option<u32>,
    formats: Option<&[String]>,
) -> UnifiedResult<()> {
    if let Some(isbn) = isbn {
        if !isbn.trim().is_empty() && !valid_isbn(isbn.trim()) {
            return Err(UnifiedError::bad_request(
                "ISBN must be a valid ISBN-10 or ISBN-13 (digits, with an optional trailing X for ISBN-10)",
            ));
        }
    }
    if let Some(language) = language {
        if !language.trim().is_empty() && !valid_language(language.trim()) {
            return Err(UnifiedError::bad_request(
                "Language must be a tag such as en or id-ID",
            ));
        }
    }
    if let Some(pages) = pages {
        if pages == 0 {
            return Err(UnifiedError::bad_request(
                "Page count must be at least 1 when set",
            ));
        }
    }
    if let Some(formats) = formats {
        if formats.len() > 16 {
            return Err(UnifiedError::bad_request(
                "At most 16 formats can be listed for a digital product",
            ));
        }
        for format in formats {
            if !valid_format(&format.to_lowercase()) {
                return Err(UnifiedError::bad_request(
                    "Formats must be lowercase alphanumeric tokens such as pdf or epub",
                ));
            }
        }
    }
    Ok(())
}

/// Searchable text for a digital product: its metadata plus the
/// linked store product's name and description.
fn digital_search_text(state: &StoreState, digital: &DigitalProduct) -> String {
    let product_text = state
        .products
        .get(&digital.product_id)
        .map(|entry| format!("{} {}", entry.value().name, entry.value().description))
        .unwrap_or_default();
    format!(
        "{} {} {} {} {} {}",
        product_text,
        digital.type_id,
        digital.author.clone().unwrap_or_default(),
        digital.publisher.clone().unwrap_or_default(),
        digital.edition.clone().unwrap_or_default(),
        digital.isbn.clone().unwrap_or_default()
    )
}

/// Purchase grant id carried as the `license_id` of a digital
/// download entitlement. Digital products have no license; the
/// grant id (one per order line) plays the `license_id` role in the
/// signed URL so the existing HMAC-signed download flow serves
/// digital files unchanged, with per-file download limits.
fn digital_grant_id(order_id: &str, product_id: &str) -> String {
    format!("digital:{order_id}:{product_id}")
}

/// Storage reference of a digital file by its file id, for the
/// signed-download response (the catalog lookup only knows software
/// releases, so digital files resolve here).
fn digital_file_storage_id(state: &StoreState, file_id: &str) -> Option<String> {
    state
        .digital_files
        .get(file_id)
        .map(|entry| entry.value().storage_file_id.clone())
}

/// Watermark descriptor for one delivered digital file: the metadata
/// fields the delivery layer must embed, carrying the buyer identity.
///
/// HONESTY NOTE (read before changing this): watermarking is
/// metadata-level only. It embeds the buyer identity into file
/// metadata where the format supports it (see the watermark format
/// registry, seeded with PDF, EPUB, MOBI, MP3, M4B and MP4, and
/// editable by administrators). It does NOT prevent copying or
/// redistribution, it is NOT unbreakable DRM, and it must never be
/// described as DRM anywhere. Formats without standard document
/// metadata (archives, installers) record the buyer in the delivery
/// manifest instead and say so in the descriptor.
fn watermark_descriptor(
    state: &StoreState,
    format: &str,
    buyer_id: &str,
    order_id: &str,
) -> Value {
    const LIMITATION: &str = "Watermarking embeds the buyer identity into file metadata only. It does not prevent copying or redistribution and is not unbreakable DRM.";
    let format = format.to_lowercase();
    let enabled = state
        .digital_defaults
        .read()
        .map(|defaults| defaults.watermark_enabled)
        .unwrap_or(true);
    let Some(entry) = state.watermark_formats.get(format.as_str()) else {
        return json!({
            "format": format,
            "metadata_supported": false,
            "buyer": buyer_id,
            "order_id": order_id,
            "fields": {},
            "limitation": "Unknown file format: no metadata watermark is defined for this format. The buyer is recorded in the download history only. Watermarking is metadata-level only and is not unbreakable DRM.",
        });
    };
    let format_entry = entry.value().clone();
    drop(entry);
    if !enabled || !format_entry.metadata_supported {
        return json!({
            "format": format,
            "metadata_supported": false,
            "buyer": buyer_id,
            "order_id": order_id,
            "fields": {},
            "limitation": if enabled { format_entry.notes } else { "Watermarking is disabled in the digital settings.".to_string() },
        });
    }
    let mut fields = serde_json::Map::new();
    for field in &format_entry.metadata_fields {
        fields.insert(
            field.clone(),
            json!(format!("Licensed to {buyer_id} - order {order_id}")),
        );
    }
    json!({
        "format": format,
        "metadata_supported": true,
        "buyer": buyer_id,
        "order_id": order_id,
        "fields": fields,
        "limitation": LIMITATION,
    })
}

/// Mint (or return the existing) download entitlement for one
/// digital file of a paid order. One entitlement per file, so each
/// file has its own download limit; the purchase grant id is shared
/// across the files of the same order line. Returns the entitlement
/// and whether it was newly created.
fn ensure_digital_entitlement(
    state: &StoreState,
    order: &Order,
    digital: &DigitalProduct,
    file: &DigitalProductFile,
) -> (DownloadEntitlement, bool) {
    let grant_id = digital_grant_id(&order.id, &digital.product_id);
    if let Some(existing) = state
        .entitlements
        .iter()
        .find(|entry| {
            let entitlement = entry.value();
            entitlement.order_id == order.id
                && entitlement.product_id == digital.product_id
                && entitlement.release_id.as_deref() == Some(file.id.as_str())
        })
        .map(|entry| entry.value().clone())
    {
        return (existing, false);
    }
    let limit = if digital.download_limit > 0 {
        digital.download_limit
    } else {
        state
            .digital_defaults
            .read()
            .map(|defaults| defaults.default_download_limit)
            .unwrap_or(DEFAULT_DIGITAL_DOWNLOAD_LIMIT)
    };
    let entitlement = DownloadEntitlement {
        id: new_id(),
        license_id: Some(grant_id),
        license_key: None,
        order_id: order.id.clone(),
        user_id: order.user_id.clone(),
        product_id: digital.product_id.clone(),
        tier_id: None,
        software_id: None,
        software_slug: None,
        release_id: Some(file.id.clone()),
        download_limit: limit,
        downloads_used: 0,
        status: "active".to_string(),
        expires_at: None,
        created_at: now(),
        updated_at: now(),
    };
    state
        .entitlements
        .insert(entitlement.id.clone(), entitlement.clone());
    (entitlement, true)
}

/// Grant downloads and accrue royalties for the digital products in
/// a paid order. Called from every place an order becomes paid (the
/// `payment.captured` listener, the manual pay path, and the manual
/// status update), so a digital purchase always unlocks its files
/// and always accrues its royalty exactly once: minting is
/// idempotent (an existing entitlement for the same order line and
/// file is returned, never duplicated) and royalty accrual skips
/// lines that already accrued for the order (one royalty row per
/// digital product per order). Royalties accrue on the net line
/// total: the order discount is shared across lines in proportion
/// to their line totals, and a fixed royalty is charged per unit
/// sold. A product with a zero royalty value accrues nothing.
fn grant_digital_purchases_for_paid_order(state: &StoreState, order: &Order) {
    let mut files_granted = 0usize;
    let mut royalties_accrued = 0usize;
    for item in &order.items {
        let Some(digital) = state
            .digital_products
            .iter()
            .find(|entry| entry.value().product_id == item.product_id)
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        let files: Vec<DigitalProductFile> = state
            .digital_files
            .iter()
            .filter(|entry| entry.value().digital_product_id == digital.id)
            .map(|entry| entry.value().clone())
            .collect();
        for file in &files {
            let (_, created) = ensure_digital_entitlement(state, order, &digital, file);
            if created {
                files_granted += 1;
            }
        }
        if digital.royalty_value <= 0.0 {
            continue;
        }
        let already_accrued = state.digital_royalties.iter().any(|entry| {
            entry.value().order_id == order.id
                && entry.value().digital_product_id == digital.id
        });
        if already_accrued {
            continue;
        }
        let share = if order.subtotal > 0.0 {
            item.line_total / order.subtotal
        } else {
            0.0
        };
        let net = round_money(item.line_total - order.discount_total * share);
        let amount = match digital.royalty_kind.as_str() {
            "fixed" => round_money(digital.royalty_value * item.quantity as f64),
            _ => round_money(net * digital.royalty_value / 100.0),
        };
        if amount <= 0.0 {
            continue;
        }
        let royalty = DigitalRoyalty {
            id: new_id(),
            digital_product_id: digital.id.clone(),
            product_id: item.product_id.clone(),
            order_id: order.id.clone(),
            buyer_user_id: order.user_id.clone(),
            payee: digital.royalty_payee.clone(),
            payee_company_id: digital
                .royalty_payee_company_id
                .clone()
                .or_else(|| item.company_id.clone()),
            amount,
            currency: order.currency.clone(),
            royalty_kind: digital.royalty_kind.clone(),
            royalty_value: digital.royalty_value,
            status: "accrued".to_string(),
            payout_id: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .digital_royalties
            .insert(royalty.id.clone(), royalty.clone());
        royalties_accrued += 1;
        state.emit(
            "store.digital.royalty.accrued",
            serde_json::to_value(&royalty).unwrap_or(Value::Null),
        );
    }
    if files_granted > 0 || royalties_accrued > 0 {
        state.emit(
            "store.digital.downloads.granted",
            json!({
                "order_id": order.id,
                "user_id": order.user_id,
                "files_granted": files_granted,
                "royalties_accrued": royalties_accrued,
            }),
        );
    }
}

/// Reverse still-unpaid royalties of a refunded order. Only
/// `accrued` royalties reverse automatically: royalties already
/// attached to a payout request (`payout_requested` or `approved`)
/// or already paid keep their status and need a manual adjustment,
/// because money may already have been approved or moved. Called
/// from `transition_order` next to the license suspension, so every
/// refund path reverses the same way. Returns the reversed count.
fn reverse_digital_royalties_for_order(state: &StoreState, order_id: &str) -> usize {
    let mut reversed = 0usize;
    for mut entry in state.digital_royalties.iter_mut() {
        if entry.value().order_id == order_id && entry.value().status == "accrued" {
            entry.value().status = "reversed".to_string();
            entry.value().updated_at = now();
            reversed += 1;
        }
    }
    reversed
}

/// Sample-download rate limit: at most `limit` downloads per window
/// for one identity and one preview. Returns an error when the
/// window is exhausted. Old entries are pruned on each check so the
/// map does not grow without bound.
fn check_sample_rate_limit(
    state: &StoreState,
    identity: &str,
    preview_id: &str,
) -> UnifiedResult<()> {
    let (limit, window_minutes) = state
        .digital_defaults
        .read()
        .map(|defaults| {
            (
                defaults.sample_rate_limit_per_hour,
                defaults.sample_rate_limit_window_minutes,
            )
        })
        .unwrap_or((
            DEFAULT_SAMPLE_RATE_LIMIT_PER_HOUR,
            DEFAULT_SAMPLE_RATE_LIMIT_WINDOW_MINUTES,
        ));
    let key = format!("{identity}:{preview_id}");
    let cutoff = now() - chrono::Duration::minutes(i64::from(window_minutes));
    let mut timestamps = state.sample_downloads.entry(key).or_default();
    timestamps.retain(|timestamp| *timestamp > cutoff);
    if timestamps.len() as u32 >= limit {
        return Err(UnifiedError::new(
            format!(
                "Sample download rate limit reached: at most {limit} sample downloads per {window_minutes} minutes for this preview"
            ),
            429,
        ));
    }
    timestamps.push(now());
    Ok(())
}

/// True for zero-decimal currencies (no minor units), matching the
/// payments module's list.
fn is_zero_decimal_currency(currency: &str) -> bool {
    matches!(
        currency,
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "UGX" | "XAF" | "XOF" | "XPF"
    )
}

/// Convert a major-unit amount (order totals) to integer minor units.
fn major_to_minor(amount_major: f64, currency: &str) -> i64 {
    if is_zero_decimal_currency(currency) {
        amount_major.round() as i64
    } else {
        (amount_major * 100.0).round() as i64
    }
}

/// Convert integer minor units (as the products catalog reports
/// prices) to major units (as store prices are stored).
fn minor_to_major(amount_minor: i64, currency: &str) -> f64 {
    if is_zero_decimal_currency(currency) {
        amount_minor as f64
    } else {
        round_money(amount_minor as f64 / 100.0)
    }
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

fn resolve_tier(state: &StoreState, product_id: &str, tier_id: &str) -> UnifiedResult<PricingTier> {
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
fn validate_coupon_usable(state: &StoreState, code: &str) -> UnifiedResult<Coupon> {
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

/// Discount a coupon gives on one line subtotal, or 0.0 when the coupon
/// does not cover that product. The discount never exceeds the subtotal.
fn coupon_discount_for(coupon: &Coupon, product_id: &str, subtotal: f64) -> f64 {
    if !coupon.product_ids.is_empty() && !coupon.product_ids.iter().any(|id| id == product_id) {
        return 0.0;
    }
    match coupon.discount_type.as_str() {
        "percent" => round_money(subtotal * coupon.discount_value / 100.0),
        _ => coupon.discount_value.min(subtotal),
    }
}

/// Validate a coupon code for one product and return the discount it
/// gives on the given subtotal.
fn coupon_discount(
    state: &StoreState,
    code: &str,
    product_id: &str,
    subtotal: f64,
) -> UnifiedResult<(Coupon, f64)> {
    let coupon = validate_coupon_usable(state, code)?;
    if !coupon.product_ids.is_empty() && !coupon.product_ids.iter().any(|id| id == product_id) {
        return Err(UnifiedError::unprocessable(
            "Coupon does not apply to this product",
        ));
    }
    let discount = coupon_discount_for(&coupon, product_id, subtotal);
    Ok((coupon, round_money(discount)))
}

/// Convert pricing scaled units (exponent 4) back to integer cents
/// with half-up rounding: the store boundary conversion, mirroring
/// the pricing module's own helper without calling across modules
/// (this module interconnects only through the hook bus).
fn scaled_to_cents(scaled: i64) -> i64 {
    if scaled >= 0 {
        (scaled + 50) / 100
    } else {
        (scaled - 50) / 100
    }
}

/// Evaluate a store coupon through the shared pricing evaluator
/// (`pricing.discount.evaluate_requested`, which never mutates the
/// order). Returns the discount in major units plus the evaluation
/// trace to record on the order. The pricing answer is used only
/// when it is valid AND it priced this coupon (no
/// `coupon_problem`): pricing holds its own coupon records, so a
/// store coupon it does not know reports
/// `coupon_problem: "Coupon not found"` and must not zero out a
/// valid store coupon. When pricing does not answer, answers
/// invalid, does not hold the coupon, or no tenant context
/// resolves for the request, the store-local float math applies
/// instead and the trace says so. Exactly one evaluator's math is
/// ever applied to an order. The tenant id comes from the
/// request's resolved tenant context, never from a client header.
/// Store money is float major units (pre-existing); the pricing
/// discount arrives as integer scaled units and is converted to
/// cents with half-up rounding at this boundary.
fn evaluate_coupon_with_pricing(
    state: &StoreState,
    req: &HttpRequest,
    items: &[OrderItem],
    coupon: &Coupon,
    customer_id: &str,
    currency: &str,
) -> (f64, Value) {
    let mut local_discount = 0.0;
    for item in items {
        local_discount += coupon_discount_for(coupon, &item.product_id, item.line_total);
    }
    let local_discount = round_money(local_discount);
    let local_trace = |reason: &str, pricing_answer: Value| {
        json!({
            "evaluator": "store-local",
            "reason": reason,
            "coupon_code": coupon.code,
            "currency": currency,
            "local_discount": local_discount,
            "pricing_answer": pricing_answer,
        })
    };
    let Ok(tenant) = crate::core::tenancy::TenantContext::from_http_request(req) else {
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
                "unit_price": format!("{:.4}", item.unit_price),
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
    let discount_major = round_money(scaled_to_cents(discount_scaled) as f64 / 100.0);
    let mut trace = answer;
    if let Value::Object(map) = &mut trace {
        map.insert("evaluator".to_string(), Value::String("pricing".to_string()));
    }
    (discount_major, trace)
}

/// Application configuration stashed by `StoreModule::init`, so the
/// download signing secret can come from config instead of a fresh
/// random value on every start. See `signing_secret`.
static STORE_CONFIG: OnceLock<Arc<RwLock<AppConfig>>> = OnceLock::new();

// ---------------------------------------------------------------------------
// Signed download links (HMAC-SHA256, expiring)
// ---------------------------------------------------------------------------

/// HMAC key for signed download links. When
/// `store.download_signing_secret` is configured, that value is used,
/// so signed links survive restarts. Otherwise a random 32-byte
/// per-process secret is generated once and a warning is logged: links
/// signed before a restart stop verifying after one. The configured
/// secret is read once at first use; changing it requires a restart.
fn signing_secret() -> &'static [u8] {
    static SECRET: OnceLock<Vec<u8>> = OnceLock::new();
    SECRET.get_or_init(|| {
        if let Some(config) = STORE_CONFIG.get() {
            let secret = config
                .read()
                .expect("config lock poisoned")
                .store
                .download_signing_secret
                .clone();
            if !secret.trim().is_empty() {
                return secret.into_bytes();
            }
        }
        log::warn!(
            "store.download_signing_secret is not set; using a random per-process download signing secret, so signed download links will stop verifying after a restart"
        );
        use rand::Rng;
        let mut rng = rand::rng();
        (0..32).map(|_| rng.random::<u8>()).collect()
    })
}

fn sign_download(license_id: &str, release_id: &str, expires: i64) -> String {
    let message = format!("{license_id}|{release_id}|{expires}");
    let mut mac = HmacSha256::new_from_slice(signing_secret())
        .expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn signature_matches(expected: &str, actual: &str) -> bool {
    let expected = expected.as_bytes();
    let actual = actual.as_bytes();
    if expected.len() != actual.len() {
        return false;
    }
    expected
        .iter()
        .zip(actual.iter())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

fn build_signed_url(license_id: &str, release_id: &str) -> (String, DateTime<Utc>) {
    let expires_at = now() + chrono::Duration::hours(1);
    let expires = expires_at.timestamp();
    let sig = sign_download(license_id, release_id, expires);
    (
        format!(
            "/api/v1/store/downloads/signed?license_id={license_id}&release_id={release_id}&expires={expires}&sig={sig}"
        ),
        expires_at,
    )
}

fn catalog_release_file(state: &StoreState, release_id: &str) -> Option<String> {
    state
        .catalog
        .iter()
        .find(|entry| entry.value().release_id.as_deref() == Some(release_id))
        .and_then(|entry| entry.value().file_id.clone())
}

/// Record one download against an entitlement: counts it, stores the
/// history row, and emits `download.started`. Returns the updated
/// entitlement, or an error when the entitlement is missing, revoked,
/// expired, or out of downloads.
fn record_download(
    state: &StoreState,
    entitlement_id: &str,
    release_id: &str,
) -> UnifiedResult<DownloadEntitlement> {
    let mut entitlement = state
        .entitlements
        .get_mut(entitlement_id)
        .ok_or_else(|| UnifiedError::not_found("Download entitlement not found"))?;
    if entitlement.status == "revoked" {
        return Err(UnifiedError::forbidden(
            "Download entitlement has been revoked",
        ));
    }
    if let Some(expires_at) = entitlement.expires_at {
        if expires_at <= now() {
            entitlement.status = "expired".to_string();
            entitlement.updated_at = now();
            return Err(UnifiedError::forbidden(
                "Download entitlement has expired",
            ));
        }
    }
    if entitlement.downloads_used >= entitlement.download_limit {
        return Err(UnifiedError::forbidden(
            "Download limit reached for this license",
        ));
    }
    entitlement.downloads_used += 1;
    entitlement.updated_at = now();
    let updated = entitlement.clone();
    let license_id = updated.license_id.clone().unwrap_or_default();
    drop(entitlement);
    let record = DownloadRecord {
        id: new_id(),
        license_id: license_id.clone(),
        release_id: release_id.to_string(),
        software_slug: updated.software_slug.clone(),
        order_id: Some(updated.order_id.clone()),
        downloaded_at: now(),
    };
    state.downloads.insert(record.id.clone(), record);
    state.emit(
        events::DOWNLOAD_STARTED,
        json!({
            "license_id": license_id,
            "release_id": release_id,
            "order_id": updated.order_id,
            "downloads_used": updated.downloads_used,
            "download_limit": updated.download_limit,
        }),
    );
    Ok(updated)
}

/// Suspend every entitlement and issued license of one order (used by
/// refunds). Returns the affected license ids.
fn suspend_order_licenses(state: &StoreState, order_id: &str) -> Vec<String> {
    let mut license_ids: Vec<String> = Vec::new();
    for mut entry in state.entitlements.iter_mut() {
        if entry.value().order_id == order_id && entry.value().status == "active" {
            entry.value().status = "revoked".to_string();
            entry.value().updated_at = now();
            if let Some(license_id) = &entry.value().license_id {
                license_ids.push(license_id.clone());
            }
        }
    }
    for mut entry in state.issued_licenses.iter_mut() {
        if entry.value().order_id.as_deref() == Some(order_id) {
            entry.value().status = "suspended".to_string();
            let license_id = entry.value().license_id.clone();
            if !license_ids.contains(&license_id) {
                license_ids.push(license_id);
            }
        }
    }
    license_ids
}

/// Refresh the store catalog entry for a published software release
/// (`software.release.published`). The payload carries the software
/// slug, version, changelog, and requirements (added by the software
/// module when it publishes). The entry's latest version only moves
/// forward: publishing an older version refreshes the changelog but
/// never downgrades `latest_version`. When no catalog entry exists
/// for the slug this is a no-op, not an error: not every software
/// record is sold through the store.
fn on_software_release_published(state: Arc<StoreState>, ctx: &mut HookContext) {
    let slug = ctx
        .get("software_slug")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    if slug.trim().is_empty() {
        ctx.add_result(json!({
            "updated": false,
            "reason": "software_slug is missing from the published event",
        }));
        return;
    }
    let Some(version) = ctx
        .get("version")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        ctx.add_result(json!({
            "updated": false,
            "reason": "version is missing from the published event",
            "slug": slug,
        }));
        return;
    };
    let changelog = ctx
        .get("changelog")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let requirements = ctx.get("requirements").cloned().unwrap_or(Value::Null);
    let requires_wp = requirements
        .get("wordpress")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let requires_php = requirements
        .get("php")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let release_id = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut updated = false;
    for mut entry in state.catalog.iter_mut() {
        if entry.value().slug != slug {
            continue;
        }
        let current = entry.value().latest_version.clone();
        let newer = match (Version::parse(&version), Version::parse(&current)) {
            (Ok(new), Ok(old)) => new > old,
            // Unparseable versions are treated as authoritative: the
            // publish event wins over the stored value.
            _ => true,
        };
        if newer {
            entry.value().latest_version = version.clone();
        }
        if let Some(changelog) = &changelog {
            entry.value().changelog = changelog.clone();
        }
        if let Some(requires_wp) = &requires_wp {
            entry.value().requires_wp = Some(requires_wp.clone());
        }
        if let Some(requires_php) = &requires_php {
            entry.value().requires_php = Some(requires_php.clone());
        }
        if let Some(release_id) = &release_id {
            entry.value().release_id = Some(release_id.clone());
        }
        entry.value().updated_at = now();
        updated = true;
    }
    ctx.add_result(json!({
        "updated": updated,
        "slug": slug,
        "version": version,
    }));
}

/// Apply a validated order status transition. Refunds suspend the
/// order's entitlements and issued licenses as a side effect.
fn transition_order(
    state: &StoreState,
    order_id: &str,
    target: &str,
) -> UnifiedResult<Order> {
    let mut order = state
        .orders
        .get_mut(order_id)
        .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
    if !allowed_transition(&order.status, target) {
        return Err(UnifiedError::unprocessable(format!(
            "Cannot move order from {} to {}",
            order.status, target
        )));
    }
    order.status = target.to_string();
    order.updated_at = now();
    let updated = order.clone();
    drop(order);
    if target == "refunded" {
        suspend_order_licenses(state, order_id);
        reverse_digital_royalties_for_order(state, order_id);
    }
    Ok(updated)
}

// ---------------------------------------------------------------------------
// Payment hook listeners
// ---------------------------------------------------------------------------

/// Event names this module listens to (the payments module owns the
/// canonical constants; the literals are repeated here so the store
/// stays decoupled from the payments module).
const PAYMENT_CAPTURED: &str = "payment.captured";
const PAYMENT_REFUNDED: &str = "payment.refunded";
const BANK_TRANSFER_VERIFICATION_CREATED: &str = "bank_transfer.verification.created";
const BANK_TRANSFER_REJECTED: &str = "bank_transfer.rejected";

/// An approved payment (`payment.captured`, gateway or bank transfer)
/// moves the matching order to `paid`. Only `pending`,
/// `payment_pending`, and `pending_verification` may move to paid —
/// never `refunded`, `cancelled`, or `completed` — and an order that
/// is already paid is left alone, so redelivered captures are safe.
/// The payment reference (provider transaction id) is recorded on the
/// order, and the same downstream events the manual pay path emits
/// Serialize an order for the `order.paid` / `store.order.completed`
/// events, carrying the e-learning course id the activation listener
/// needs. The contract is a top-level `course_id` plus `module` =
/// `"elearning"`, or an `items` entry carrying them. Order items
/// already serialize their `course_id` (set at checkout); items
/// also serialize `module_id`, never `module`, so each item that
/// carries a course id gains an explicit `module: "elearning"`
/// here, and a single distinct course id is hoisted to the top
/// level. With several distinct course ids the top level stays
/// ambiguous and only the per-item entries carry them.
fn order_paid_value(order: &Order) -> Value {
    let mut value = serde_json::to_value(order).unwrap_or(Value::Null);
    let mut course_ids: Vec<String> = Vec::new();
    if let Some(items) = value
        .get_mut("items")
        .and_then(|items| items.as_array_mut())
    {
        for item in items.iter_mut() {
            let course_id = item
                .get("course_id")
                .and_then(|id| id.as_str())
                .map(str::to_string);
            if let Some(course_id) = course_id {
                if let Value::Object(map) = item {
                    map.insert(
                        "module".to_string(),
                        Value::String("elearning".to_string()),
                    );
                }
                if !course_ids.contains(&course_id) {
                    course_ids.push(course_id);
                }
            }
        }
    }
    if course_ids.len() == 1 {
        if let Value::Object(map) = &mut value {
            map.insert(
                "course_id".to_string(),
                Value::String(course_ids[0].clone()),
            );
            map.insert(
                "module".to_string(),
                Value::String("elearning".to_string()),
            );
        }
    }
    value
}

/// (`payment.completed`, `order.paid`) are emitted, so licensing,
/// hosting, and billing react exactly as they do for a manual payment.
///
/// Amount check: for gateway payments the captured amount must cover
/// the order total, so a partial capture (or one split part) never
/// marks the whole order paid. Bank-transfer captures skip the check:
/// the administrator already verified the amount when approving the
/// transfer, including approved mismatches. A currency mismatch is
/// never applied.
fn on_payment_captured(state: Arc<StoreState>, ctx: &mut HookContext) {
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|id| id.as_str())
                .map(str::to_string)
        });
    let Some(order_id) = order_id else {
        ctx.add_result(json!({
            "store": "skipped",
            "reason": "payment.captured carries no order id",
        }));
        return;
    };
    // An explicit non-captured status is never treated as a capture;
    // a missing status means the event name itself is the claim.
    if let Some(status) = ctx.get("status").and_then(|value| value.as_str()) {
        if status != "captured" {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": format!("payment.captured carries status \"{status}\", not \"captured\""),
            }));
            return;
        }
    }
    let provider = ctx
        .get("provider")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let reference = ctx
        .get("provider_transaction_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("transaction_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let amount_minor = ctx.get("amount_minor").and_then(|value| value.as_i64());
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Some(order) = state
        .orders
        .get(&order_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "store": "skipped",
            "order_id": order_id,
            "reason": "order not found",
        }));
        return;
    };
    match order.status.as_str() {
        "paid" | "completed" => {
            ctx.add_result(json!({
                "store": "already_paid",
                "order_id": order_id,
                "status": order.status,
            }));
            return;
        }
        "refunded" | "cancelled" => {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": format!("order is {} and can never move to paid", order.status),
            }));
            return;
        }
        "pending" | "payment_pending" | "pending_verification" => {}
        other => {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": format!("order in status {other} cannot move to paid"),
            }));
            return;
        }
    }
    if provider.as_deref() != Some("bank_transfer") {
        if let (Some(amount), Some(event_currency)) = (amount_minor, &currency) {
            if !event_currency.eq_ignore_ascii_case(&order.currency) {
                ctx.add_result(json!({
                    "store": "skipped",
                    "order_id": order_id,
                    "reason": format!(
                        "captured currency {event_currency} does not match order currency {}",
                        order.currency
                    ),
                }));
                return;
            }
            if amount < major_to_minor(order.total, &order.currency) {
                ctx.add_result(json!({
                    "store": "skipped",
                    "order_id": order_id,
                    "reason": "captured amount does not cover the order total (partial capture or split part)",
                }));
                return;
            }
        }
    }
    if let Some(reference) = &reference {
        if let Some(mut stored) = state.orders.get_mut(&order_id) {
            stored.payment_reference = Some(reference.clone());
            if let Some(provider) = &provider {
                stored.provider = Some(provider.clone());
            }
            stored.updated_at = now();
        }
    }
    match transition_order(&state, &order_id, "paid") {
        Ok(updated) => {
            let order_value = serde_json::to_value(&updated).unwrap_or(Value::Null);
            state.emit("payment.completed", order_value.clone());
            state.emit("order.paid", order_paid_value(&updated));
            let subscription = activate_subscription_for_paid_order(&state, &updated);
            grant_digital_purchases_for_paid_order(&state, &updated);
            ctx.add_result(json!({
                "store": "paid",
                "order_id": order_id,
                "payment_reference": reference,
                "subscription_id": subscription.map(|subscription| subscription.id),
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": error.message,
            }));
        }
    }
}

/// A completed refund (`payment.refunded` with a `refunded` status)
/// moves the matching order to `refunded`, suspends its licenses
/// through the transition, and emits `store.order.refunded` so
/// licensing and billing react. Pending refunds are never applied
/// here: while the refund is still pending the order stays in
/// `refund_pending` and only the refund-completion path finishes it.
/// Partial refunds leave the order paid. The refund identifiers are
/// forwarded on the emitted event so billing can deduplicate the
/// credit it records.
fn on_payment_refunded(state: Arc<StoreState>, ctx: &mut HookContext) {
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|id| id.as_str())
                .map(str::to_string)
        });
    let Some(order_id) = order_id else {
        ctx.add_result(json!({
            "store": "skipped",
            "reason": "payment.refunded carries no order id",
        }));
        return;
    };
    // Fail closed: without an explicit completed status the refund
    // must not move the order. Pending and failed refunds are
    // recorded, never applied.
    let status = ctx
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match status {
        "refunded" => {}
        "partially_refunded" => {
            ctx.add_result(json!({
                "store": "partial_refund",
                "order_id": order_id,
                "reason": "partial refund recorded; order stays paid",
            }));
            return;
        }
        other => {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": format!(
                    "refund status \"{other}\" is not a completed refund",
                ),
            }));
            return;
        }
    }
    let Some(order) = state
        .orders
        .get(&order_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "store": "skipped",
            "order_id": order_id,
            "reason": "order not found",
        }));
        return;
    };
    if order.status == "refunded" {
        ctx.add_result(json!({
            "store": "already_refunded",
            "order_id": order_id,
        }));
        return;
    }
    if !matches!(
        order.status.as_str(),
        "paid" | "completed" | "refund_pending"
    ) {
        ctx.add_result(json!({
            "store": "skipped",
            "order_id": order_id,
            "reason": format!("order in status {} cannot be refunded", order.status),
        }));
        return;
    }
    match transition_order(&state, &order_id, "refunded") {
        Ok(updated) => {
            let license_ids: Vec<String> = state
                .issued_licenses
                .iter()
                .filter(|entry| entry.value().order_id.as_deref() == Some(order_id.as_str()))
                .map(|entry| entry.value().license_id.clone())
                .collect();
            let cancelled_subscriptions =
                cancel_subscriptions_for_order(&state, &order_id, "order_refunded");
            state.emit(
                "store.order.refunded",
                json!({
                    "order": serde_json::to_value(&updated).unwrap_or(Value::Null),
                    "order_id": updated.id,
                    "license_ids": license_ids,
                    "cancelled_subscriptions": cancelled_subscriptions,
                    "reason": ctx.get("reason").cloned().unwrap_or(Value::Null),
                    "provider_refund_id": ctx.get("provider_refund_id").cloned().unwrap_or(Value::Null),
                    "transaction_id": ctx.get("transaction_id").cloned().unwrap_or(Value::Null),
                    "amount_minor": ctx.get("amount_minor").cloned().unwrap_or(Value::Null),
                    "currency": ctx.get("currency").cloned().unwrap_or(Value::Null),
                }),
            );
            ctx.add_result(json!({
                "store": "refunded",
                "order_id": order_id,
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": error.message,
            }));
        }
    }
}

/// A bank-transfer verification opened for an order moves it from
/// `pending` / `payment_pending` to `pending_verification`, so the
/// order visibly waits for manual verification.
fn on_bank_transfer_verification_created(state: Arc<StoreState>, ctx: &mut HookContext) {
    let Some(order_id) = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        ctx.add_result(json!({
            "store": "skipped",
            "reason": "bank_transfer.verification.created carries no order id",
        }));
        return;
    };
    let Some(order) = state
        .orders
        .get(&order_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "store": "skipped",
            "order_id": order_id,
            "reason": "order not found",
        }));
        return;
    };
    if order.status == "pending_verification" {
        ctx.add_result(json!({
            "store": "already_pending_verification",
            "order_id": order_id,
        }));
        return;
    }
    match transition_order(&state, &order_id, "pending_verification") {
        Ok(_) => {
            ctx.add_result(json!({
                "store": "pending_verification",
                "order_id": order_id,
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": error.message,
            }));
        }
    }
}

/// A rejected bank transfer cancels the order. The bank-transfer
/// module's own note suggested returning the order to `pending`
/// instead; the prescribed fallback for that contradiction is
/// cancellation on rejection, so rejection cancels here, while
/// `pending_verification` to `pending` stays allowed in the status
/// machine for an administrator who wants to put the order back to
/// awaiting payment for a retry.
fn on_bank_transfer_rejected(state: Arc<StoreState>, ctx: &mut HookContext) {
    let Some(order_id) = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        ctx.add_result(json!({
            "store": "skipped",
            "reason": "bank_transfer.rejected carries no order id",
        }));
        return;
    };
    let Some(order) = state
        .orders
        .get(&order_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "store": "skipped",
            "order_id": order_id,
            "reason": "order not found",
        }));
        return;
    };
    if order.status == "cancelled" {
        ctx.add_result(json!({
            "store": "already_cancelled",
            "order_id": order_id,
        }));
        return;
    }
    match transition_order(&state, &order_id, "cancelled") {
        Ok(_) => {
            let order_value = state
                .orders
                .get(&order_id)
                .map(|entry| serde_json::to_value(entry.value().clone()).unwrap_or(Value::Null))
                .unwrap_or(Value::Null);
            state.emit("store.order.cancelled", order_value);
            ctx.add_result(json!({
                "store": "cancelled",
                "order_id": order_id,
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": error.message,
            }));
        }
    }
}

const PRODUCT_LISTED: &str = "product.listed";
const PRODUCT_DELISTED: &str = "product.delisted";
const STORE_ENTITLEMENT_CHECK: &str = "store.entitlement.check";

/// Mirror a catalog listing into the store (`product.listed`).
///
/// The products module emits a `ProductDetail` with the product
/// flattened at top level: `id`, `slug`, `name`, `description`,
/// `kind`, `price: {amount_minor, currency}`, `pricing_model`,
/// `billing_period`, `renewal_price`, `trial_days`,
/// `vendor_company_id`, `software_id`, and
/// `license_template.requires_license`. The store listing is keyed
/// by the products product id, so orders, entitlements, and the
/// `store.entitlement.check` answer all line up on the same id.
/// A listing that already exists is refreshed in place (price,
/// name, and flags); its store-only data (tiers, file, slug
/// overrides) is kept.
///
/// Assumptions, recorded because the payload cannot supply them:
/// tier data is NOT in the payload (only `tier_count`), so tiers
/// are never mirrored here — a new listing starts with none and
/// sells at its base price. The store kind is kept verbatim from
/// the catalog: the store's own create/update validation only
/// knows `theme`/`plugin`/`software`, but checkout does not
/// depend on kind, and the catalog vocabulary (`digital`,
/// `meeting`, `hosting`, ...) is wider — the listing stays
/// truthful to its source. `pricing_model` values with no store
/// equivalent (`usage`, `per_seat`) fall back to `one_time`.
fn on_product_listed(state: Arc<StoreState>, ctx: &mut HookContext) {
    let product_id = ctx
        .get("id")
        .or_else(|| ctx.get("product_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Some(product_id) = product_id.filter(|id| !id.is_empty()) else {
        ctx.add_result(json!({
            "store": "product_listed",
            "skipped": true,
            "reason": "missing_product_id",
        }));
        return;
    };
    let name = ctx
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() {
        ctx.add_result(json!({
            "store": "product_listed",
            "product_id": product_id,
            "skipped": true,
            "reason": "missing_product_name",
        }));
        return;
    }
    let currency = ctx
        .get("price")
        .and_then(|value| value.get("currency"))
        .and_then(|value| value.as_str())
        .unwrap_or("USD")
        .to_uppercase();
    if !valid_currency(&currency) {
        ctx.add_result(json!({
            "store": "product_listed",
            "product_id": product_id,
            "skipped": true,
            "reason": "invalid_currency",
        }));
        return;
    }
    let amount_minor = ctx
        .get("price")
        .and_then(|value| value.get("amount_minor"))
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    if amount_minor < 0 {
        ctx.add_result(json!({
            "store": "product_listed",
            "product_id": product_id,
            "skipped": true,
            "reason": "negative_price",
        }));
        return;
    }
    let price = minor_to_major(amount_minor, &currency);
    let kind = ctx
        .get("kind")
        .and_then(|value| value.as_str())
        .unwrap_or("software")
        .to_string();
    let pricing_model = match ctx
        .get("pricing_model")
        .and_then(|value| value.as_str())
        .unwrap_or("one_time")
    {
        "subscription" => "subscription".to_string(),
        _ => "one_time".to_string(),
    };
    let billing_period = ctx
        .get("billing_period")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let renewal_price = ctx
        .get("renewal_price")
        .and_then(|value| value.get("amount_minor"))
        .and_then(|value| value.as_i64())
        .map(|minor| minor_to_major(minor, &currency));
    let trial_days = ctx
        .get("trial_days")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32;
    let proration_policy = ctx
        .get("proration_policy")
        .and_then(|value| value.as_str())
        .filter(|policy| matches!(*policy, "prorate" | "no_prorate" | "credit_only"))
        .unwrap_or("prorate")
        .to_string();
    let grace_period_days = ctx
        .get("grace_period_days")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32;
    let dunning_max_retries = ctx
        .get("dunning_max_retries")
        .and_then(|value| value.as_u64())
        .unwrap_or(3) as u32;
    let dunning_retry_interval_days = ctx
        .get("dunning_retry_interval_days")
        .and_then(|value| value.as_u64())
        .unwrap_or(3) as u32;
    let software_id = ctx
        .get("software_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let requires_license = ctx
        .get("license_template")
        .and_then(|value| value.get("requires_license"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let description = ctx
        .get("description")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let slug = ctx
        .get("slug")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let company_id = ctx
        .get("vendor_company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let license_product = requires_license || software_id.is_some();
    let created = !state.products.contains_key(&product_id);
    if let Some(mut existing) = state.products.get_mut(&product_id) {
        existing.name = name;
        existing.description = description;
        existing.price = price;
        existing.currency = currency;
        existing.kind = kind;
        if let Some(slug) = slug {
            existing.slug = Some(slug);
        }
        existing.software_id = software_id;
        existing.pricing_model = pricing_model;
        existing.billing_period = billing_period;
        existing.renewal_price = renewal_price;
        existing.trial_days = trial_days;
        existing.proration_policy = proration_policy;
        existing.grace_period_days = grace_period_days;
        existing.dunning_max_retries = dunning_max_retries;
        existing.dunning_retry_interval_days = dunning_retry_interval_days;
        existing.license_product = license_product;
        existing.company_id = company_id;
        existing.status = "published".to_string();
        existing.updated_at = now();
    } else {
        state.products.insert(
            product_id.clone(),
            Product {
                id: product_id.clone(),
                name,
                description,
                price,
                currency,
                kind,
                license_product,
                file_id: None,
                slug,
                software_id,
                software_slug: None,
                status: "published".to_string(),
                company_id,
                pricing_model,
                billing_period,
                renewal_price,
                trial_days,
                proration_policy,
                grace_period_days,
                dunning_max_retries,
                dunning_retry_interval_days,
                module_id: None,
                updates_years: None,
                created_at: now(),
                updated_at: now(),
            },
        );
    }
    ctx.add_result(json!({
        "store": "product_listed",
        "product_id": product_id,
        "created": created,
        "status": "published",
    }));
}

/// Retire a catalog listing from the store (`product.delisted`).
///
/// The listing moves to `draft` so it can no longer be bought,
/// while the record — and its order history — is kept. A later
/// `product.listed` republishes it. Nothing is deleted: deleting
/// would orphan the orders and entitlements that reference the
/// product id.
fn on_product_delisted(state: Arc<StoreState>, ctx: &mut HookContext) {
    let product_id = ctx
        .get("id")
        .or_else(|| ctx.get("product_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Some(product_id) = product_id.filter(|id| !id.is_empty()) else {
        ctx.add_result(json!({
            "store": "product_delisted",
            "skipped": true,
            "reason": "missing_product_id",
        }));
        return;
    };
    let Some(mut product) = state.products.get_mut(&product_id) else {
        ctx.add_result(json!({
            "store": "product_delisted",
            "product_id": product_id,
            "skipped": true,
            "reason": "not_listed",
        }));
        return;
    };
    if product.status == "draft" {
        ctx.add_result(json!({
            "store": "product_delisted",
            "product_id": product_id,
            "already_draft": true,
        }));
        return;
    }
    product.status = "draft".to_string();
    product.updated_at = now();
    ctx.add_result(json!({
        "store": "product_delisted",
        "product_id": product_id,
        "status": "draft",
    }));
}

/// Create a store order for a properties block purchase
/// (`properties.block.purchase_requested`
/// `{purchase_id, property_id, plan_id, block_id, buyer_id, price,
/// currency}`): one line item for the block at the payload's price,
/// buyer as the order owner, status `pending` — the block is NOT
/// marked sold here; the properties module owns that transition on
/// `payment.captured`. Emits `properties.order.created`
/// `{purchase_id, order_id, block_id, property_id, buyer_id,
/// total, currency}` so the properties module can link the order
/// id to the purchase record. A repeat request for the same
/// `purchase_id` returns the existing order instead of creating a
/// second one (the properties module emits the request once per
/// purchase, but a redelivery must never double-sell the block).
/// The block is not a store product, so the line carries the block
/// id as its `product_id` with a descriptive name; no catalogue
/// lookup is performed.
fn on_properties_purchase_requested(state: Arc<StoreState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let required = |key: &str| {
        data.get(key)
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .filter(|value| !value.trim().is_empty())
    };
    let Some(purchase_id) = required("purchase_id") else {
        ctx.add_result(json!({
            "store": "skipped",
            "reason": "properties.block.purchase_requested carries no purchase_id",
        }));
        return;
    };
    let Some(block_id) = required("block_id") else {
        ctx.add_result(json!({
            "store": "skipped",
            "purchase_id": purchase_id,
            "reason": "properties.block.purchase_requested carries no block_id",
        }));
        return;
    };
    let Some(property_id) = required("property_id") else {
        ctx.add_result(json!({
            "store": "skipped",
            "purchase_id": purchase_id,
            "reason": "properties.block.purchase_requested carries no property_id",
        }));
        return;
    };
    let Some(buyer_id) = required("buyer_id") else {
        ctx.add_result(json!({
            "store": "skipped",
            "purchase_id": purchase_id,
            "reason": "properties.block.purchase_requested carries no buyer_id",
        }));
        return;
    };
    let price = data.get("price").and_then(|value| value.as_f64());
    let Some(price) = price.filter(|value| value.is_finite() && *value >= 0.0) else {
        ctx.add_result(json!({
            "store": "skipped",
            "purchase_id": purchase_id,
            "reason": "properties.block.purchase_requested carries no valid price",
        }));
        return;
    };
    let currency = data
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("USD")
        .to_string();
    if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_alphabetic()) {
        ctx.add_result(json!({
            "store": "skipped",
            "purchase_id": purchase_id,
            "reason": "properties.block.purchase_requested currency must be a 3-letter code",
        }));
        return;
    }
    // Idempotency: orders carry no purchase-link field, so a
    // repeat request is detected by the block line — a pending
    // order owned by this buyer with this block as its only line.
    // That is the same purchase by construction (one block sells
    // once), so the existing order is returned instead of a
    // second one.
    if let Some(existing) = state
        .orders
        .iter()
        .find(|entry| {
            let order = entry.value();
            order.user_id == buyer_id
                && order.status == "pending"
                && order.items.len() == 1
                && order.items[0].product_id == block_id
        })
        .map(|entry| entry.value().clone())
    {
        ctx.add_result(json!({
            "store": "order_already_created",
            "purchase_id": purchase_id,
            "order_id": existing.id,
            "block_id": block_id,
        }));
        return;
    }
    let total = round_money(price);
    let item = OrderItem {
        product_id: block_id.clone(),
        name: format!("Property block {block_id}"),
        unit_price: total,
        quantity: 1,
        line_total: total,
        license_product: false,
        file_id: None,
        tier_id: None,
        tier_name: None,
        software_id: None,
        software_slug: None,
        company_id: None,
        pricing_model: "one_time".to_string(),
        billing_period: None,
        seats: 1,
        expires_at: None,
        download_limit: 0,
        course_id: None,
        trial_days: 0,
        proration_policy: default_proration_policy(),
        grace_period_days: 0,
        dunning_max_retries: default_dunning_max_retries(),
        dunning_retry_interval_days: default_dunning_retry_interval_days(),
        module_id: Some("properties".to_string()),
    };
    let items = vec![item];
    let order = Order {
        id: new_id(),
        reference: new_order_reference(&state, &items),
        user_id: buyer_id.clone(),
        items,
        subtotal: total,
        discount_total: 0.0,
        total,
        currency: currency.to_uppercase(),
        coupon_code: None,
        discount_evaluation: None,
        company_id: None,
        trial_until: None,
        status: "pending".to_string(),
        provider: None,
        payment_reference: None,
        created_at: now(),
        updated_at: now(),
    };
    state.orders.insert(order.id.clone(), order.clone());
    state.emit(
        "properties.order.created",
        json!({
            "purchase_id": purchase_id,
            "order_id": order.id,
            "block_id": block_id,
            "property_id": property_id,
            "buyer_id": buyer_id,
            "total": total,
            "currency": currency.to_uppercase(),
        }),
    );
    ctx.add_result(json!({
        "store": "order_created",
        "purchase_id": purchase_id,
        "order_id": order.id,
        "block_id": block_id,
        "property_id": property_id,
        "total": total,
        "currency": currency.to_uppercase(),
    }));
}

/// Answer `store.entitlement.check` with payload
/// `{"product_id", "user_id"}`.
///
/// Entitled when the user holds an active entitlement for the
/// product — the same `DownloadEntitlement` records the store
/// entitlements list endpoint reads (status `active` and not past
/// `expires_at`) — or an active issued license for it. Issued
/// licenses carry the product NAME, not its id, so the license
/// fallback resolves the name through the store product. The
/// answer carries both `"entitled": bool` and an `entitlements`
/// array with the matching active records, because the products
/// module accepts either shape. This closes the loop for
/// `product.entitlement.check` (products) and the meetings join
/// check, which previously failed closed with
/// `no_entitlement_source`.
fn on_store_entitlement_check(state: Arc<StoreState>, ctx: &mut HookContext) {
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
    if product_id.is_empty() || user_id.is_empty() {
        ctx.add_result(json!({
            "entitled": false,
            "product_id": product_id,
            "user_id": user_id,
            "entitlements": [],
            "reason": "missing_product_id_or_user_id",
        }));
        return;
    }
    let now = now();
    let mut matches: Vec<Value> = state
        .entitlements
        .iter()
        .filter(|entry| {
            let entitlement = entry.value();
            entitlement.product_id == product_id
                && entitlement.user_id == user_id
                && entitlement.status == "active"
                && entitlement.expires_at.is_none_or(|expiry| expiry > now)
        })
        .map(|entry| {
            let entitlement = entry.value();
            json!({
                "id": entitlement.id,
                "source": "entitlement",
                "status": "active",
                "expires_at": entitlement.expires_at,
            })
        })
        .collect();
    if let Some(product) = state.products.get(&product_id) {
        let product_name = product.name.clone();
        drop(product);
        for entry in state.issued_licenses.iter() {
            let license = entry.value();
            if license.user_id.as_deref() == Some(user_id.as_str())
                && license.status == "active"
                && license.product == product_name
            {
                matches.push(json!({
                    "id": license.license_id,
                    "source": "license",
                    "status": "active",
                }));
            }
        }
    }
    // An active subscription to the product also entitles the user,
    // for as long as it grants access (trialing/active inside the
    // period, past-due inside grace, cancelled at period end inside
    // the period).
    for entry in state.subscriptions.iter() {
        let subscription = entry.value();
        if subscription.product_id == product_id
            && subscription.user_id == user_id
            && subscription_grants_access(subscription, now)
        {
            matches.push(json!({
                "id": subscription.id,
                "source": "subscription",
                "status": subscription.status,
                "expires_at": subscription.current_period_end,
            }));
        }
    }
    let entitled = !matches.is_empty();
    ctx.add_result(json!({
        "entitled": entitled,
        "product_id": product_id,
        "user_id": user_id,
        "entitlements": matches,
        "reason": if entitled { "entitled" } else { "no_active_entitlement" },
    }));
}

// ---------------------------------------------------------------------------
// Product handlers
// ---------------------------------------------------------------------------

async fn list_products(
    state: web::Data<Arc<StoreState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let sort = params.allowed_sort(&["name", "price", "created_at"], "created_at");
    let mut items: Vec<Product> = state
        .products
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|product| {
            matches_search(
                &format!("{} {}", product.name, product.description),
                &params,
            ) && params
                .filters
                .get("kind")
                .is_none_or(|kind| product.kind == *kind)
                && params.filters.get("status").is_none_or(|status| product.status == *status)
                && params.filters.get("software_id").is_none_or(|id| {
                    product.software_id.as_deref() == Some(id.as_str())
                })
                && params.filters.get("software_slug").is_none_or(|slug| {
                    product.software_slug.as_deref() == Some(slug.as_str())
                })
                && params.filters.get("pricing_model").is_none_or(|model| {
                    product.pricing_model == *model
                })
                && params.filters.get("company_id").is_none_or(|company_id| {
                    product.company_id.as_deref() == Some(company_id.as_str())
                })
                && params
                    .filters
                    .get("currency")
                    .is_none_or(|currency| product.currency.eq_ignore_ascii_case(currency))
                && params.filters.get("license_product").is_none_or(|flag| {
                    product.license_product == (*flag == "true" || *flag == "1")
                })
        })
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "price" => items.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

fn validate_product_pricing(input_pricing_model: Option<&str>, input_billing_period: Option<&str>) -> UnifiedResult<()> {
    if let Some(model) = input_pricing_model {
        if !valid_pricing_model(model) {
            return Err(UnifiedError::bad_request(
                "Pricing model must be one of: one_time, subscription",
            ));
        }
    }
    if let Some(period) = input_billing_period {
        if !valid_billing_period(period) {
            return Err(UnifiedError::bad_request(
                "Billing period must be one of: monthly, yearly",
            ));
        }
    }
    Ok(())
}

async fn create_product(
    state: web::Data<Arc<StoreState>>,
    body: web::Json<CreateProduct>,
) -> HttpResponse {
    let result: UnifiedResult<Product> = (|| {
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Product name must not be empty"));
        }
        if !input.price.is_finite() || input.price < 0.0 {
            return Err(UnifiedError::bad_request(
                "Product price must be a non-negative number",
            ));
        }
        if !valid_kind(&input.kind) {
            return Err(UnifiedError::bad_request(
                "Product kind must be one of: theme, plugin, software",
            ));
        }
        let currency = input.currency.unwrap_or_else(|| "USD".to_string());
        if !valid_currency(&currency) {
            return Err(UnifiedError::bad_request(
                "Currency must be a 3-letter code such as USD",
            ));
        }
        if let Some(status) = &input.status {
            if !valid_product_status(status) {
                return Err(UnifiedError::bad_request(
                    "Product status must be one of: draft, published",
                ));
            }
        }
        validate_product_pricing(
            input.pricing_model.as_deref(),
            input.billing_period.as_deref(),
        )?;
        if let Some(renewal_price) = input.renewal_price {
            if !renewal_price.is_finite() || renewal_price < 0.0 {
                return Err(UnifiedError::bad_request(
                    "Renewal price must be a non-negative number",
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
        validate_subscription_settings(
            input.proration_policy.as_deref(),
            input.grace_period_days,
            input.dunning_max_retries,
            input.dunning_retry_interval_days,
        )?;
        let pricing_model = input
            .pricing_model
            .unwrap_or_else(default_pricing_model);
        if pricing_model == "subscription" && input.billing_period.is_none() {
            return Err(UnifiedError::bad_request(
                "Subscription products require a billing period",
            ));
        }
        let product = Product {
            id: new_id(),
            name: input.name.trim().to_string(),
            description: input.description.unwrap_or_default(),
            price: round_money(input.price),
            currency: currency.to_uppercase(),
            kind: input.kind,
            // A product linked to software is a license product unless
            // the caller explicitly says otherwise.
            license_product: input
                .license_product
                .unwrap_or_else(|| input.software_id.is_some()),
            file_id: input.file_id,
            slug: input.slug,
            software_id: input.software_id,
            software_slug: input.software_slug,
            status: input.status.unwrap_or_else(default_product_status),
            company_id: input.company_id,
            pricing_model,
            billing_period: input.billing_period,
            renewal_price: input.renewal_price.map(round_money),
            trial_days: input.trial_days.unwrap_or(0),
            proration_policy: input
                .proration_policy
                .unwrap_or_else(default_proration_policy),
            grace_period_days: input.grace_period_days.unwrap_or(0),
            dunning_max_retries: input
                .dunning_max_retries
                .unwrap_or_else(default_dunning_max_retries),
            dunning_retry_interval_days: input
                .dunning_retry_interval_days
                .unwrap_or_else(default_dunning_retry_interval_days),
            module_id: input.module_id,
            updates_years: input.updates_years,
            created_at: now(),
            updated_at: now(),
        };
        state.products.insert(product.id.clone(), product.clone());
        Ok(product)
    })();
    respond_created(result)
}

async fn get_product(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Product> = state
        .products
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Product not found"));
    json::respond(result, false)
}

async fn update_product(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
    body: web::Json<UpdateProduct>,
) -> HttpResponse {
    let result: UnifiedResult<Product> = (|| {
        let input = body.into_inner();
        // Validate pricing before touching the stored product so a
        // rejected update never leaves a half-mutated record behind.
        validate_product_pricing(
            input.pricing_model.as_deref(),
            input.billing_period.as_deref(),
        )?;
        let mut product = state
            .products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Product name must not be empty"));
            }
            product.name = name.trim().to_string();
        }
        if let Some(description) = input.description {
            product.description = description;
        }
        if let Some(price) = input.price {
            if !price.is_finite() || price < 0.0 {
                return Err(UnifiedError::bad_request(
                    "Product price must be a non-negative number",
                ));
            }
            product.price = round_money(price);
        }
        if let Some(currency) = input.currency {
            if !valid_currency(&currency) {
                return Err(UnifiedError::bad_request(
                    "Currency must be a 3-letter code such as USD",
                ));
            }
            product.currency = currency.to_uppercase();
        }
        if let Some(kind) = input.kind {
            if !valid_kind(&kind) {
                return Err(UnifiedError::bad_request(
                    "Product kind must be one of: theme, plugin, software",
                ));
            }
            product.kind = kind;
        }
        if let Some(license_product) = input.license_product {
            product.license_product = license_product;
        }
        if let Some(file_id) = input.file_id {
            product.file_id = Some(file_id);
        }
        if let Some(slug) = input.slug {
            product.slug = Some(slug);
        }
        if let Some(software_id) = input.software_id {
            product.software_id = Some(software_id);
        }
        if let Some(software_slug) = input.software_slug {
            product.software_slug = Some(software_slug);
        }
        if let Some(status) = input.status {
            if !valid_product_status(&status) {
                return Err(UnifiedError::bad_request(
                    "Product status must be one of: draft, published",
                ));
            }
            product.status = status;
        }
        if let Some(company_id) = input.company_id {
            product.company_id = Some(company_id);
        }
        if let Some(pricing_model) = input.pricing_model {
            product.pricing_model = pricing_model;
        }
        if let Some(billing_period) = input.billing_period {
            product.billing_period = Some(billing_period);
        }
        if let Some(renewal_price) = input.renewal_price {
            if !renewal_price.is_finite() || renewal_price < 0.0 {
                return Err(UnifiedError::bad_request(
                    "Renewal price must be a non-negative number",
                ));
            }
            product.renewal_price = Some(round_money(renewal_price));
        }
        if let Some(trial_days) = input.trial_days {
            product.trial_days = trial_days;
        }
        if let Some(proration_policy) = input.proration_policy {
            if !matches!(
                proration_policy.as_str(),
                "prorate" | "no_prorate" | "credit_only"
            ) {
                return Err(UnifiedError::bad_request(
                    "Proration policy must be one of: prorate, no_prorate, credit_only",
                ));
            }
            product.proration_policy = proration_policy;
        }
        if let Some(grace_period_days) = input.grace_period_days {
            if grace_period_days > 90 {
                return Err(UnifiedError::bad_request(
                    "Grace period must be at most 90 days",
                ));
            }
            product.grace_period_days = grace_period_days;
        }
        if let Some(dunning_max_retries) = input.dunning_max_retries {
            if dunning_max_retries > 10 {
                return Err(UnifiedError::bad_request(
                    "Dunning max retries must be at most 10",
                ));
            }
            product.dunning_max_retries = dunning_max_retries;
        }
        if let Some(dunning_retry_interval_days) = input.dunning_retry_interval_days {
            if dunning_retry_interval_days == 0 || dunning_retry_interval_days > 30 {
                return Err(UnifiedError::bad_request(
                    "Dunning retry interval must be between 1 and 30 days",
                ));
            }
            product.dunning_retry_interval_days = dunning_retry_interval_days;
        }
        if let Some(module_id) = input.module_id {
            if module_id.trim().is_empty() {
                return Err(UnifiedError::bad_request(
                    "Module id must not be empty",
                ));
            }
            product.module_id = Some(module_id.trim().to_string());
        }
        if let Some(updates_years) = input.updates_years {
            product.updates_years = Some(updates_years);
        }
        product.updated_at = now();
        Ok(product.clone())
    })();
    json::respond(result, false)
}

async fn delete_product(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let id = path.as_str().to_string();
        let has_tiers = state.tiers.iter().any(|entry| entry.value().product_id == id);
        if has_tiers {
            return Err(UnifiedError::conflict(
                "Product has pricing tiers; delete the tiers first",
            ));
        }
        state
            .products
            .remove(&id)
            .map(|_| json!({ "deleted": true, "id": id }))
            .ok_or_else(|| UnifiedError::not_found("Product not found"))
    })();
    json::respond(result, false)
}

async fn publish_product(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Product> = (|| {
        let mut product = state
            .products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if product.status == "published" {
            return Err(UnifiedError::conflict("Product is already published"));
        }
        product.status = "published".to_string();
        product.updated_at = now();
        let value = serde_json::to_value(&*product).unwrap_or(Value::Null);
        drop(product);
        state.emit("store.product.published", value);
        Ok(state
            .products
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("product still present"))
    })();
    json::respond(result, false)
}

async fn unpublish_product(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Product> = (|| {
        let mut product = state
            .products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if product.status == "draft" {
            return Err(UnifiedError::conflict("Product is already a draft"));
        }
        product.status = "draft".to_string();
        product.updated_at = now();
        let value = serde_json::to_value(&*product).unwrap_or(Value::Null);
        drop(product);
        state.emit("store.product.unpublished", value);
        Ok(state
            .products
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("product still present"))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Pricing tier handlers
// ---------------------------------------------------------------------------

async fn list_tiers(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let product_id = path.into_inner();
    if !state.products.contains_key(&product_id) {
        return json::error_response(&UnifiedError::not_found("Product not found"), false);
    }
    let mut items: Vec<PricingTier> = state
        .tiers
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|tier| {
            tier.product_id == product_id
                && matches_search(&format!("{} {}", tier.slug, tier.label), &params)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| tier.status == *status)
                && params
                    .filters
                    .get("slug")
                    .is_none_or(|slug| tier.slug == *slug)
        })
        .collect();
    items.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_tier(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
    body: web::Json<CreateTier>,
) -> HttpResponse {
    let result: UnifiedResult<PricingTier> = (|| {
        let product_id = path.into_inner();
        if !state.products.contains_key(&product_id) {
            return Err(UnifiedError::not_found("Product not found"));
        }
        let input = body.into_inner();
        if input.slug.trim().is_empty() {
            return Err(UnifiedError::bad_request("Tier slug must not be empty"));
        }
        if !input.price.is_finite() || input.price < 0.0 {
            return Err(UnifiedError::bad_request(
                "Tier price must be a non-negative number",
            ));
        }
        if let Some(billing_period) = &input.billing_period {
            if !valid_billing_period(billing_period) {
                return Err(UnifiedError::bad_request(
                    "Billing period must be one of: monthly, yearly",
                ));
            }
        }
        if let Some(renewal_price) = input.renewal_price {
            if !renewal_price.is_finite() || renewal_price < 0.0 {
                return Err(UnifiedError::bad_request(
                    "Renewal price must be a non-negative number",
                ));
            }
        }
        let seats = input.seats.unwrap_or(1);
        if seats < 1 {
            return Err(UnifiedError::bad_request("Tier seats must be at least 1"));
        }
        let slug = input.slug.trim().to_string();
        let duplicate = state
            .tiers
            .iter()
            .any(|entry| entry.value().product_id == product_id && entry.value().slug == slug);
        if duplicate {
            return Err(UnifiedError::conflict(
                "A tier with this slug already exists for the product",
            ));
        }
        let tier = PricingTier {
            id: new_id(),
            product_id,
            slug,
            label: input.label.unwrap_or_else(|| input.slug.trim().to_string()),
            price: round_money(input.price),
            renewal_price: input.renewal_price.map(round_money),
            billing_period: input.billing_period,
            trial_days: input.trial_days.unwrap_or(0),
            seats,
            download_limit: input.download_limit.unwrap_or(default_download_limit()),
            features: input.features.unwrap_or_default(),
            status: input.status.unwrap_or_else(|| "active".to_string()),
            created_at: now(),
            updated_at: now(),
        };
        if tier.status != "active" && tier.status != "disabled" {
            return Err(UnifiedError::bad_request(
                "Tier status must be one of: active, disabled",
            ));
        }
        state.tiers.insert(tier.id.clone(), tier.clone());
        Ok(tier)
    })();
    respond_created(result)
}

async fn get_tier(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PricingTier> = state
        .tiers
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"));
    json::respond(result, false)
}

async fn update_tier(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
    body: web::Json<UpdateTier>,
) -> HttpResponse {
    let result: UnifiedResult<PricingTier> = (|| {
        let input = body.into_inner();
        let mut tier = state
            .tiers
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))?;
        if let Some(slug) = input.slug {
            if slug.trim().is_empty() {
                return Err(UnifiedError::bad_request("Tier slug must not be empty"));
            }
            tier.slug = slug.trim().to_string();
        }
        if let Some(label) = input.label {
            tier.label = label;
        }
        if let Some(price) = input.price {
            if !price.is_finite() || price < 0.0 {
                return Err(UnifiedError::bad_request(
                    "Tier price must be a non-negative number",
                ));
            }
            tier.price = round_money(price);
        }
        if let Some(renewal_price) = input.renewal_price {
            if !renewal_price.is_finite() || renewal_price < 0.0 {
                return Err(UnifiedError::bad_request(
                    "Renewal price must be a non-negative number",
                ));
            }
            tier.renewal_price = Some(round_money(renewal_price));
        }
        if let Some(billing_period) = input.billing_period {
            if !valid_billing_period(&billing_period) {
                return Err(UnifiedError::bad_request(
                    "Billing period must be one of: monthly, yearly",
                ));
            }
            tier.billing_period = Some(billing_period);
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
        if let Some(download_limit) = input.download_limit {
            tier.download_limit = download_limit;
        }
        if let Some(features) = input.features {
            tier.features = features;
        }
        if let Some(status) = input.status {
            if status != "active" && status != "disabled" {
                return Err(UnifiedError::bad_request(
                    "Tier status must be one of: active, disabled",
                ));
            }
            tier.status = status;
        }
        tier.updated_at = now();
        Ok(tier.clone())
    })();
    json::respond(result, false)
}

async fn delete_tier(
    state: web::Data<Arc<StoreState>>,
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
// Coupon handlers and price calculation
// ---------------------------------------------------------------------------

async fn list_coupons(
    state: web::Data<Arc<StoreState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<Coupon> = state
        .coupons
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|coupon| {
            matches_search(&coupon.code, &params)
                && params
                    .filters
                    .get("active")
                    .is_none_or(|flag| coupon.active == (*flag == "true" || *flag == "1"))
                && params.filters.get("discount_type").is_none_or(|kind| {
                    coupon.discount_type == *kind
                })
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_coupon(
    state: web::Data<Arc<StoreState>>,
    body: web::Json<CreateCoupon>,
) -> HttpResponse {
    let result: UnifiedResult<Coupon> = (|| {
        let input = body.into_inner();
        let code = input.code.trim().to_uppercase();
        if code.is_empty() {
            return Err(UnifiedError::bad_request("Coupon code must not be empty"));
        }
        if input.discount_type != "percent" && input.discount_type != "fixed" {
            return Err(UnifiedError::bad_request(
                "Discount type must be one of: percent, fixed",
            ));
        }
        if !input.discount_value.is_finite() || input.discount_value <= 0.0 {
            return Err(UnifiedError::bad_request(
                "Discount value must be greater than zero",
            ));
        }
        if input.discount_type == "percent" && input.discount_value > 100.0 {
            return Err(UnifiedError::bad_request(
                "Percentage discount must be at most 100",
            ));
        }
        if let Some(expires_at) = input.expires_at {
            if expires_at <= now() {
                return Err(UnifiedError::bad_request(
                    "Coupon expiry must be in the future",
                ));
            }
        }
        if let Some(limit) = input.usage_limit {
            if limit == 0 {
                return Err(UnifiedError::bad_request(
                    "Coupon usage limit must be at least 1",
                ));
            }
        }
        let product_ids = input.product_ids.unwrap_or_default();
        for product_id in &product_ids {
            if !state.products.contains_key(product_id) {
                return Err(UnifiedError::not_found(format!(
                    "Product {product_id} not found"
                )));
            }
        }
        if state
            .coupons
            .iter()
            .any(|entry| entry.value().code == code)
        {
            return Err(UnifiedError::conflict(
                "A coupon with this code already exists",
            ));
        }
        let coupon = Coupon {
            id: new_id(),
            code,
            discount_type: input.discount_type,
            discount_value: round_money(input.discount_value),
            expires_at: input.expires_at,
            usage_limit: input.usage_limit,
            used_count: 0,
            product_ids,
            active: input.active.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state.coupons.insert(coupon.id.clone(), coupon.clone());
        Ok(coupon)
    })();
    respond_created(result)
}

async fn get_coupon(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Coupon> = state
        .coupons
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Coupon not found"));
    json::respond(result, false)
}

async fn update_coupon(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
    body: web::Json<UpdateCoupon>,
) -> HttpResponse {
    let result: UnifiedResult<Coupon> = (|| {
        let input = body.into_inner();
        let mut coupon = state
            .coupons
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Coupon not found"))?;
        if let Some(discount_type) = input.discount_type {
            if discount_type != "percent" && discount_type != "fixed" {
                return Err(UnifiedError::bad_request(
                    "Discount type must be one of: percent, fixed",
                ));
            }
            coupon.discount_type = discount_type;
        }
        if let Some(discount_value) = input.discount_value {
            if !discount_value.is_finite() || discount_value <= 0.0 {
                return Err(UnifiedError::bad_request(
                    "Discount value must be greater than zero",
                ));
            }
            if coupon.discount_type == "percent" && discount_value > 100.0 {
                return Err(UnifiedError::bad_request(
                    "Percentage discount must be at most 100",
                ));
            }
            coupon.discount_value = round_money(discount_value);
        }
        if let Some(expires_at) = input.expires_at {
            coupon.expires_at = Some(expires_at);
        }
        if let Some(usage_limit) = input.usage_limit {
            if usage_limit == 0 {
                return Err(UnifiedError::bad_request(
                    "Coupon usage limit must be at least 1",
                ));
            }
            coupon.usage_limit = Some(usage_limit);
        }
        if let Some(product_ids) = input.product_ids {
            for product_id in &product_ids {
                if !state.products.contains_key(product_id) {
                    return Err(UnifiedError::not_found(format!(
                        "Product {product_id} not found"
                    )));
                }
            }
            coupon.product_ids = product_ids;
        }
        if let Some(active) = input.active {
            coupon.active = active;
        }
        coupon.updated_at = now();
        Ok(coupon.clone())
    })();
    json::respond(result, false)
}

async fn delete_coupon(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .coupons
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Coupon not found"))
    })();
    json::respond(result, false)
}

/// Calculate the price of a product (optionally a tier and a coupon)
/// without creating an order. The coupon is applied honestly: the
/// response shows the subtotal, the discount, and the final total.
async fn calculate_price(
    state: web::Data<Arc<StoreState>>,
    body: web::Json<PriceCalculateRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        let quantity = input.quantity.unwrap_or(1);
        if quantity == 0 {
            return Err(UnifiedError::bad_request("Quantity must be at least 1"));
        }
        let product = state
            .products
            .get(&input.product_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        let (unit_price, tier_id, tier_name, seats, trial_days, download_limit, billing_period, renewal_price) =
            match &input.tier_id {
                Some(tier_id) => {
                    let tier = resolve_tier(&state, &product.id, tier_id)?;
                    (
                        tier.price,
                        Some(tier.id.clone()),
                        Some(tier.label.clone()),
                        tier.seats,
                        tier.trial_days,
                        tier.download_limit,
                        tier.billing_period
                            .or_else(|| product.billing_period.clone()),
                        tier.renewal_price.or(product.renewal_price),
                    )
                }
                None => (
                    product.price,
                    None,
                    None,
                    1,
                    product.trial_days,
                    default_download_limit(),
                    product.billing_period.clone(),
                    product.renewal_price,
                ),
            };
        let subtotal = round_money(unit_price * quantity as f64);
        let (discount, coupon_value) = match &input.coupon_code {
            Some(code) if !code.trim().is_empty() => {
                let (coupon, discount) =
                    coupon_discount(&state, code, &product.id, subtotal)?;
                (
                    discount,
                    Some(json!({
                        "code": coupon.code,
                        "discount_type": coupon.discount_type,
                        "discount_value": coupon.discount_value,
                    })),
                )
            }
            _ => (0.0, None),
        };
        let renewal_price = renewal_price.map(round_money);
        Ok(json!({
            "product_id": product.id,
            "product_name": product.name,
            "tier_id": tier_id,
            "tier_name": tier_name,
            "unit_price": round_money(unit_price),
            "quantity": quantity,
            "subtotal": subtotal,
            "discount": discount,
            "total": round_money(subtotal - discount),
            "currency": product.currency,
            "coupon": coupon_value,
            "pricing_model": product.pricing_model,
            "billing_period": billing_period,
            "renewal_price": renewal_price,
            "trial_days": trial_days,
            "seats": seats,
            "download_limit": download_limit,
        }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Cart handlers
// ---------------------------------------------------------------------------

fn empty_cart(owner: &str) -> Cart {
    Cart {
        owner: owner.to_string(),
        items: Vec::new(),
        currency: "USD".to_string(),
        total: 0.0,
        updated_at: now(),
    }
}

fn recalculate_cart(cart: &mut Cart) {
    cart.total = round_money(cart.items.iter().map(|item| item.line_total).sum());
    cart.updated_at = now();
}

async fn get_cart(state: web::Data<Arc<StoreState>>, req: HttpRequest) -> HttpResponse {
    let owner = owner_of(&req);
    let cart = state
        .carts
        .get(&owner)
        .map(|entry| entry.value().clone())
        .unwrap_or_else(|| empty_cart(&owner));
    json::success_response(cart, 200)
}

async fn add_to_cart(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<AddCartItem>,
) -> HttpResponse {
    let result: UnifiedResult<Cart> = (|| {
        let input = body.into_inner();
        let quantity = input.quantity.unwrap_or(1);
        if quantity == 0 {
            return Err(UnifiedError::bad_request("Quantity must be at least 1"));
        }
        let product = state
            .products
            .get(&input.product_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if product.status != "published" {
            return Err(UnifiedError::unprocessable(
                "Only published products can be added to the cart",
            ));
        }
        let (unit_price, tier_name) = match &input.tier_id {
            Some(tier_id) => {
                let tier = resolve_tier(&state, &product.id, tier_id)?;
                (tier.price, Some(tier.label))
            }
            None => (product.price, None),
        };
        let owner = owner_of(&req);
        let mut cart = state
            .carts
            .get(&owner)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| empty_cart(&owner));
        if !cart.items.is_empty() && cart.currency != product.currency {
            return Err(UnifiedError::unprocessable(
                "All cart items must use the same currency",
            ));
        }
        cart.currency = product.currency.clone();
        match cart
            .items
            .iter_mut()
            .find(|item| item.product_id == product.id && item.tier_id == input.tier_id)
        {
            Some(item) => {
                item.quantity += quantity;
                item.unit_price = unit_price;
                item.line_total = round_money(unit_price * item.quantity as f64);
            }
            None => cart.items.push(CartItem {
                product_id: product.id.clone(),
                name: product.name.clone(),
                unit_price,
                quantity,
                line_total: round_money(unit_price * quantity as f64),
                tier_id: input.tier_id.clone(),
                tier_name,
            }),
        }
        recalculate_cart(&mut cart);
        state.carts.insert(owner, cart.clone());
        Ok(cart)
    })();
    json::respond(result, false)
}

async fn remove_from_cart(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: Option<web::Json<RemoveCartItem>>,
) -> HttpResponse {
    let result: UnifiedResult<Cart> = (|| {
        let owner = owner_of(&req);
        let mut cart = state
            .carts
            .get(&owner)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| empty_cart(&owner));
        let (product_id, _tier_id) = match body {
            Some(payload) => {
                let inner = payload.into_inner();
                (inner.product_id, inner.tier_id)
            }
            None => (None, None),
        };
        match product_id {
            Some(product_id) => {
                let before = cart.items.len();
                cart.items.retain(|item| item.product_id != product_id);
                if cart.items.len() == before {
                    return Err(UnifiedError::not_found("Product is not in the cart"));
                }
            }
            None => cart.items.clear(),
        }
        if cart.items.is_empty() {
            cart.currency = "USD".to_string();
        }
        recalculate_cart(&mut cart);
        state.carts.insert(owner, cart.clone());
        Ok(cart)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Order handlers
// ---------------------------------------------------------------------------

async fn checkout(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<CheckoutRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Order> = (|| {
        let input = body.into_inner();
        let owner = input
            .user_id
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| owner_of(&req));
        // Resolve the items: an explicit list wins, otherwise the stored cart.
        let requested: Vec<CheckoutItem> = match input.items {
            Some(items) => items,
            None => state
                .carts
                .get(&owner)
                .map(|entry| {
                    entry
                        .value()
                        .items
                        .iter()
                        .map(|item| CheckoutItem {
                            product_id: item.product_id.clone(),
                            quantity: Some(item.quantity),
                            tier_id: item.tier_id.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        if requested.is_empty() {
            return Err(UnifiedError::unprocessable(
                "Cannot check out an empty cart",
            ));
        }
        let mut items: Vec<OrderItem> = Vec::new();
        let mut currency: Option<String> = None;
        let mut subtotal = 0.0;
        let mut company_id: Option<String> = None;
        let mut trial_until: Option<DateTime<Utc>> = None;
        for requested_item in requested {
            let quantity = requested_item.quantity.unwrap_or(1);
            if quantity == 0 {
                return Err(UnifiedError::bad_request("Quantity must be at least 1"));
            }
            let course_id = match &requested_item.course_id {
                Some(raw) if !raw.trim().is_empty() => {
                    Uuid::parse_str(raw.trim()).map_err(|_| {
                        UnifiedError::bad_request("course_id must be a UUID")
                    })?;
                    Some(raw.trim().to_string())
                }
                _ => None,
            };
            let product = state
                .products
                .get(&requested_item.product_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!(
                        "Product {} not found",
                        requested_item.product_id
                    ))
                })?;
            if product.status != "published" {
                return Err(UnifiedError::unprocessable(format!(
                    "Product {} is not published",
                    product.name
                )));
            }
            match &currency {
                Some(existing) if *existing != product.currency => {
                    return Err(UnifiedError::unprocessable(
                        "All order items must use the same currency",
                    ));
                }
                None => currency = Some(product.currency.clone()),
                _ => {}
            }
            let (unit_price, tier_id, tier_name, seats, trial_days, download_limit, billing_period) =
                match &requested_item.tier_id {
                    Some(tier_id) => {
                        let tier = resolve_tier(&state, &product.id, tier_id)?;
                        (
                            tier.price,
                            Some(tier.id.clone()),
                            Some(tier.label.clone()),
                            tier.seats,
                            tier.trial_days,
                            tier.download_limit,
                            tier.billing_period
                                .or_else(|| product.billing_period.clone()),
                        )
                    }
                    None => (
                        product.price,
                        None,
                        None,
                        1,
                        product.trial_days,
                        default_download_limit(),
                        product.billing_period.clone(),
                    ),
                };
            if trial_days > 0 {
                let ends = now() + chrono::Duration::days(trial_days as i64);
                trial_until = Some(match trial_until {
                    Some(existing) if existing > ends => existing,
                    _ => ends,
                });
            }
            if company_id.is_none() {
                company_id = product.company_id.clone();
            }
            let line_total = round_money(unit_price * quantity as f64);
            subtotal += line_total;
            items.push(OrderItem {
                product_id: product.id.clone(),
                name: product.name.clone(),
                unit_price,
                quantity,
                line_total,
                license_product: product.license_product,
                file_id: product.file_id.clone(),
                tier_id,
                tier_name,
                software_id: product.software_id.clone(),
                software_slug: product.software_slug.clone(),
                company_id: product.company_id.clone(),
                pricing_model: product.pricing_model.clone(),
                billing_period,
                seats,
                expires_at: license_expiry_for(
                    &product.pricing_model,
                    product.billing_period.as_deref(),
                    product.updates_years,
                ),
                download_limit,
                course_id,
                trial_days,
                proration_policy: product.proration_policy.clone(),
                grace_period_days: product.grace_period_days,
                dunning_max_retries: product.dunning_max_retries,
                dunning_retry_interval_days: product.dunning_retry_interval_days,
                module_id: product.module_id.clone(),
            });
        }
        // A subscription order holds exactly one subscription
        // product: a subscription is one recurring purchase, and
        // mixing it with other items would leave the subscription
        // created at payment ambiguous about which item it covers.
        let subscription_items = items
            .iter()
            .filter(|item| item.pricing_model == "subscription")
            .count();
        if subscription_items > 0 {
            if items.len() != 1 || subscription_items != 1 {
                return Err(UnifiedError::unprocessable(
                    "A subscription order must reference exactly one subscription product",
                ));
            }
            if items[0].quantity != 1 {
                return Err(UnifiedError::bad_request(
                    "A subscription product is bought one at a time (quantity must be 1)",
                ));
            }
            if items[0].billing_period.is_none() {
                return Err(UnifiedError::unprocessable(
                    "A subscription product requires a billing period",
                ));
            }
        }
        subtotal = round_money(subtotal);
        // A coupon discounts only the line items it covers; a coupon
        // that covers nothing in the order is rejected below. The
        // discount is evaluated through the shared pricing
        // evaluator first, with the store-local float math as the
        // fallback when pricing cannot price this coupon — exactly
        // one evaluator's math is applied, and the trace is
        // recorded on the order.
        let mut discount_total = 0.0;
        let mut coupon_code: Option<String> = None;
        let mut applied_coupon: Option<Coupon> = None;
        let mut discount_evaluation: Option<Value> = None;
        if let Some(code) = input.coupon_code.filter(|code| !code.trim().is_empty()) {
            let coupon = validate_coupon_usable(&state, &code)?;
            let (order_discount, trace) = evaluate_coupon_with_pricing(
                &state,
                &req,
                &items,
                &coupon,
                &owner,
                currency.as_deref().unwrap_or("USD"),
            );
            if order_discount <= 0.0 {
                return Err(UnifiedError::unprocessable(
                    "Coupon does not apply to any item in this order",
                ));
            }
            discount_total = round_money(order_discount.min(subtotal));
            discount_evaluation = Some(trace);
            coupon_code = Some(code.trim().to_uppercase());
            applied_coupon = Some(coupon);
        }
        // Single payment reference for the order (batch 18): issued
        // once here, in the shared format, and never changed. The
        // reference travels with the order into `order.created` and
        // `payment.initiated`, so billing and the gateway see the
        // same value.
        let order_reference = new_order_reference(&state, &items);
        let order = Order {
            id: new_id(),
            reference: order_reference,
            user_id: owner.clone(),
            items,
            subtotal,
            discount_total,
            total: round_money(subtotal - discount_total),
            currency: currency.unwrap_or_else(|| "USD".to_string()),
            coupon_code: coupon_code.clone(),
            discount_evaluation: discount_evaluation.clone(),
            company_id: company_id.clone(),
            trial_until,
            status: "pending".to_string(),
            provider: None,
            payment_reference: None,
            created_at: now(),
            updated_at: now(),
        };
        state.orders.insert(order.id.clone(), order.clone());
        // The cart is consumed by the checkout.
        state.carts.remove(&owner);
        // Emit the legacy event first (licensing and billing listen to
        // it), then the namespaced store event. The licensing results
        // carry the issued license ids, which become entitlements.
        let order_value = serde_json::to_value(&order).unwrap_or(Value::Null);
        let hook_context = state.emit_with_results("order.created", order_value.clone());
        state.emit("store.order.created", order_value);
        if let Some(coupon) = applied_coupon {
            if let Some(mut entry) = state.coupons.get_mut(&coupon.id) {
                entry.used_count += 1;
                entry.updated_at = now();
            }
            state.emit(
                "store.coupon.used",
                json!({
                    "coupon_id": coupon.id,
                    "code": coupon.code,
                    "order_id": order.id,
                    "discount_total": discount_total,
                }),
            );
        }
        create_entitlements_from_hook(&state, &order, &hook_context);
        Ok(order)
    })();
    respond_created(result)
}

/// Turn the licensing hook results of `order.created` into download
/// entitlements and a local mirror of the issued licenses.
fn create_entitlements_from_hook(state: &StoreState, order: &Order, hook_context: &HookContext) {
    for result in &hook_context.results {
        if result.get("issued").and_then(|value| value.as_bool()) != Some(true) {
            continue;
        }
        let license_id = result
            .get("license_id")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let license_key = result
            .get("license_key")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let product = result
            .get("product")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let Some(item) = order.items.iter().find(|item| item.name == product) else {
            continue;
        };
        let release_id = result
            .get("release_id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| {
                state
                    .catalog
                    .iter()
                    .find(|entry| entry.value().product_id.as_deref() == Some(item.product_id.as_str()))
                    .and_then(|entry| entry.value().release_id.clone())
            });
        let software_slug = result
            .get("software_slug")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| item.software_slug.clone());
        let entitlement = DownloadEntitlement {
            id: new_id(),
            license_id: license_id.clone(),
            license_key: license_key.clone(),
            order_id: order.id.clone(),
            user_id: order.user_id.clone(),
            product_id: item.product_id.clone(),
            tier_id: item.tier_id.clone(),
            software_id: item.software_id.clone(),
            software_slug,
            release_id,
            download_limit: item.download_limit,
            downloads_used: 0,
            status: "active".to_string(),
            expires_at: item.expires_at,
            created_at: now(),
            updated_at: now(),
        };
        state
            .entitlements
            .insert(entitlement.id.clone(), entitlement);
        if let (Some(license_id), Some(license_key)) = (license_id, license_key) {
            state.issued_licenses.insert(
                license_id.clone(),
                IssuedLicense {
                    license_id,
                    license_key,
                    product,
                    order_id: Some(order.id.clone()),
                    user_id: Some(order.user_id.clone()),
                    company_id: order.company_id.clone(),
                    status: "active".to_string(),
                    created_at: now(),
                },
            );
        }
    }
}

/// Emit `subscription.entitlement_changed` for a subscription whose
/// access changed, so the licensing module (and any listener) can
/// grant or revoke the module the subscription covers. Only
/// subscriptions bound to a sellable module (`module_id`) emit it.
fn emit_subscription_entitlement_changed(
    state: &StoreState,
    subscription: &Subscription,
    entitled: bool,
    reason: &str,
) {
    let Some(module_id) = &subscription.module_id else {
        return;
    };
    state.emit(
        "subscription.entitlement_changed",
        json!({
            "subscription_id": subscription.id,
            "user_id": subscription.user_id,
            "company_id": subscription.company_id,
            "product_id": subscription.product_id,
            "module_id": module_id,
            "order_id": subscription.order_id,
            "entitled": entitled,
            "reason": reason,
        }),
    );
}

/// Create the subscription for a paid subscription order, or return
/// the existing one when the order already has one (redelivered
/// `payment.captured` events and manual re-pays must never create a
/// second subscription). A trial starts as `trialing` with the
/// trial end recorded; otherwise the subscription starts `active`.
/// The first period runs from now to one billing period after the
/// trial ends (trialing) or from now (no trial). The recurring price
/// is the product renewal price when one is set, else the price paid.
/// Emits `subscription.created` and, for module products,
/// `subscription.entitlement_changed` with `entitled: true`.
fn activate_subscription_for_paid_order(
    state: &StoreState,
    order: &Order,
) -> Option<Subscription> {
    let item = order
        .items
        .iter()
        .find(|item| item.pricing_model == "subscription")?;
    if let Some(existing) = state
        .subscriptions
        .iter()
        .find(|entry| entry.value().order_id == order.id)
        .map(|entry| entry.value().clone())
    {
        return Some(existing);
    }
    let billing_period = item.billing_period.clone()?;
    let price = state
        .products
        .get(&item.product_id)
        .and_then(|product| product.renewal_price)
        .unwrap_or(item.unit_price);
    let start = now();
    let trial_end = if item.trial_days > 0 {
        Some(start + chrono::Duration::days(i64::from(item.trial_days)))
    } else {
        None
    };
    let current_period_end = match trial_end {
        Some(trial_end) => period_end(trial_end, &billing_period),
        None => period_end(start, &billing_period),
    };
    let subscription = Subscription {
        id: new_id(),
        user_id: order.user_id.clone(),
        company_id: order.company_id.clone(),
        product_id: item.product_id.clone(),
        tier_id: item.tier_id.clone(),
        order_id: order.id.clone(),
        module_id: item.module_id.clone(),
        status: if trial_end.is_some() {
            "trialing".to_string()
        } else {
            "active".to_string()
        },
        billing_period,
        price: round_money(price),
        currency: order.currency.clone(),
        trial_days: item.trial_days,
        trial_end,
        current_period_start: start,
        current_period_end,
        proration_policy: item.proration_policy.clone(),
        grace_period_days: item.grace_period_days,
        dunning_max_retries: item.dunning_max_retries,
        dunning_retry_interval_days: item.dunning_retry_interval_days,
        cancel_at_period_end: false,
        cancelled_at: None,
        pending_product_id: None,
        pending_tier_id: None,
        pending_price: None,
        pending_billing_period: None,
        created_at: start,
        updated_at: start,
    };
    state
        .subscriptions
        .insert(subscription.id.clone(), subscription.clone());
    state.emit(
        "subscription.created",
        serde_json::to_value(&subscription).unwrap_or(Value::Null),
    );
    emit_subscription_entitlement_changed(state, &subscription, true, "subscription_created");
    Some(subscription)
}

/// Cancel every live subscription of a refunded order. A refund is
/// final, so the cancellation is immediate (not at period end) and
/// revokes the module entitlement when the subscription covers one.
fn cancel_subscriptions_for_order(state: &StoreState, order_id: &str, reason: &str) -> usize {
    let ids: Vec<String> = state
        .subscriptions
        .iter()
        .filter(|entry| {
            entry.value().order_id == order_id && entry.value().status != "cancelled"
        })
        .map(|entry| entry.key().clone())
        .collect();
    let mut cancelled = 0usize;
    for id in ids {
        let Some(mut subscription) = state.subscriptions.get_mut(&id) else {
            continue;
        };
        subscription.status = "cancelled".to_string();
        subscription.cancel_at_period_end = false;
        subscription.cancelled_at = Some(now());
        subscription.updated_at = now();
        let updated = subscription.clone();
        drop(subscription);
        cancelled += 1;
        state.emit(
            "subscription.cancelled",
            json!({
                "subscription": serde_json::to_value(&updated).unwrap_or(Value::Null),
                "subscription_id": updated.id,
                "reason": reason,
            }),
        );
        emit_subscription_entitlement_changed(state, &updated, false, reason);
    }
    cancelled
}

/// Apply a lifecycle decision from the billing module
/// (`subscription.lifecycle.apply`). Billing owns the lifecycle
/// engine (renewals, dunning, grace, reminders) but never writes
/// this store's records directly; it asks here, and this handler
/// applies the decision to the authoritative subscription and emits
/// the public `subscription.*` event billing mirrors from.
///
/// Actions: `renew` (advance the period to the supplied
/// `current_period_start` / `current_period_end`, applying any
/// pending plan change), `mark_past_due`, `end_trial`,
/// `cancel` (with `reason`). Unknown actions and transitions from
/// a terminal state are reported in the hook result, never applied.
fn on_subscription_lifecycle_apply(state: Arc<StoreState>, ctx: &mut HookContext) {
    let subscription_id = ctx
        .get("subscription_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let action = ctx
        .get("action")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let Some(mut subscription) = state.subscriptions.get_mut(&subscription_id) else {
        ctx.add_result(json!({
            "store": "subscription_not_found",
            "subscription_id": subscription_id,
            "action": action,
        }));
        return;
    };
    let parse_date = |value: Option<&Value>| {
        value
            .and_then(|value| value.as_str())
            .and_then(|text| text.parse::<DateTime<Utc>>().ok())
    };
    let updated: Option<Subscription> = match action.as_str() {
        "renew" => {
            if subscription.status == "cancelled" {
                ctx.add_result(json!({
                    "store": "skipped",
                    "subscription_id": subscription_id,
                    "action": action,
                    "reason": "subscription is cancelled",
                }));
                return;
            }
            if subscription.cancel_at_period_end {
                ctx.add_result(json!({
                    "store": "skipped",
                    "subscription_id": subscription_id,
                    "action": action,
                    "reason": "subscription is scheduled for cancellation at period end",
                }));
                return;
            }
            let Some(period_start) = parse_date(ctx.get("current_period_start")) else {
                ctx.add_result(json!({
                    "store": "skipped",
                    "subscription_id": subscription_id,
                    "action": action,
                    "reason": "current_period_start is required",
                }));
                return;
            };
            let Some(period_end_value) = parse_date(ctx.get("current_period_end")) else {
                ctx.add_result(json!({
                    "store": "skipped",
                    "subscription_id": subscription_id,
                    "action": action,
                    "reason": "current_period_end is required",
                }));
                return;
            };
            if period_end_value <= period_start {
                ctx.add_result(json!({
                    "store": "skipped",
                    "subscription_id": subscription_id,
                    "action": action,
                    "reason": "current_period_end must be after current_period_start",
                }));
                return;
            }
            // A plan change waiting for the next period takes effect
            // with the renewal.
            if let Some(pending_product_id) = subscription.pending_product_id.clone() {
                subscription.product_id = pending_product_id;
                if let Some(pending_price) = subscription.pending_price {
                    subscription.price = pending_price;
                }
                if let Some(pending_period) = subscription.pending_billing_period.clone() {
                    subscription.billing_period = pending_period;
                }
                subscription.tier_id = subscription.pending_tier_id.clone();
                subscription.pending_product_id = None;
                subscription.pending_tier_id = None;
                subscription.pending_price = None;
                subscription.pending_billing_period = None;
            }
            subscription.status = "active".to_string();
            subscription.current_period_start = period_start;
            subscription.current_period_end = period_end_value;
            subscription.updated_at = now();
            let updated = subscription.clone();
            drop(subscription);
            state.emit(
                "subscription.renewed",
                json!({
                    "subscription": serde_json::to_value(&updated).unwrap_or(Value::Null),
                    "subscription_id": updated.id,
                    "invoice_id": ctx.get("invoice_id").cloned().unwrap_or(Value::Null),
                }),
            );
            Some(updated)
        }
        "mark_past_due" => {
            if !matches!(subscription.status.as_str(), "active" | "trialing") {
                ctx.add_result(json!({
                    "store": "skipped",
                    "subscription_id": subscription_id,
                    "action": action,
                    "reason": format!("subscription is {}", subscription.status),
                }));
                return;
            }
            subscription.status = "past_due".to_string();
            subscription.updated_at = now();
            let updated = subscription.clone();
            drop(subscription);
            state.emit(
                "subscription.past_due",
                json!({
                    "subscription": serde_json::to_value(&updated).unwrap_or(Value::Null),
                    "subscription_id": updated.id,
                    "reason": ctx.get("reason").cloned().unwrap_or(Value::Null),
                }),
            );
            Some(updated)
        }
        "end_trial" => {
            if subscription.status != "trialing" {
                ctx.add_result(json!({
                    "store": "skipped",
                    "subscription_id": subscription_id,
                    "action": action,
                    "reason": format!("subscription is {}", subscription.status),
                }));
                return;
            }
            subscription.status = "active".to_string();
            subscription.updated_at = now();
            let updated = subscription.clone();
            drop(subscription);
            state.emit(
                "subscription.trial_ended",
                json!({
                    "subscription": serde_json::to_value(&updated).unwrap_or(Value::Null),
                    "subscription_id": updated.id,
                }),
            );
            Some(updated)
        }
        "cancel" => {
            if subscription.status == "cancelled" {
                ctx.add_result(json!({
                    "store": "already_cancelled",
                    "subscription_id": subscription_id,
                    "action": action,
                }));
                return;
            }
            let reason = ctx
                .get("reason")
                .and_then(|value| value.as_str())
                .unwrap_or("cancelled")
                .to_string();
            subscription.status = "cancelled".to_string();
            subscription.cancel_at_period_end = false;
            subscription.cancelled_at = Some(now());
            subscription.updated_at = now();
            let updated = subscription.clone();
            drop(subscription);
            state.emit(
                "subscription.cancelled",
                json!({
                    "subscription": serde_json::to_value(&updated).unwrap_or(Value::Null),
                    "subscription_id": updated.id,
                    "reason": reason,
                }),
            );
            emit_subscription_entitlement_changed(state, &updated, false, &reason);
            Some(updated)
        }
        other => {
            ctx.add_result(json!({
                "store": "skipped",
                "subscription_id": subscription_id,
                "action": action,
                "reason": format!("unknown lifecycle action \"{other}\""),
            }));
            return;
        }
    };
    if let Some(updated) = updated {
        ctx.add_result(json!({
            "store": "applied",
            "subscription_id": updated.id,
            "action": action,
            "status": updated.status,
        }));
    }
}

/// Answer `subscription.module_entitlement.check` with payload
/// `{"company_id", "module_id"}`: entitled when the company holds a
/// subscription that covers the module and currently grants access
/// (trialing/active inside the period, past-due inside grace, or
/// cancelled at period end inside the period). This is the hook the
/// licensing module's `check_module_entitlement` must call (see the
/// integration notes) so an active subscription counts as a module
/// entitlement alongside a license.
fn on_subscription_module_entitlement_check(state: Arc<StoreState>, ctx: &mut HookContext) {
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let module_id = ctx
        .get("module_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let now = now();
    let found = state
        .subscriptions
        .iter()
        .find(|entry| {
            let subscription = entry.value();
            subscription.module_id.as_deref() == Some(module_id.as_str())
                && subscription.company_id.as_deref() == Some(company_id.as_str())
                && subscription_grants_access(subscription, now)
        })
        .map(|entry| entry.value().clone());
    match found {
        Some(subscription) => ctx.add_result(json!({
            "entitled": true,
            "module_id": module_id,
            "company_id": company_id,
            "subscription_id": subscription.id,
            "source": "subscription",
        })),
        None => ctx.add_result(json!({
            "entitled": false,
            "module_id": module_id,
            "company_id": company_id,
            "source": "subscription",
            "reason": "no_active_subscription",
        })),
    }
}

// ---------------------------------------------------------------------------
// Subscription handlers
// ---------------------------------------------------------------------------

fn require_subscription_owner(req: &HttpRequest, subscription: &Subscription) -> UnifiedResult<()> {
    if caller_owns(req, &subscription.user_id) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Subscription belongs to a different user",
        ))
    }
}

async fn list_subscriptions(
    state: web::Data<Arc<StoreState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<Subscription> = state
        .subscriptions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|subscription| {
            matches_search(
                &format!(
                    "{} {} {}",
                    subscription.id, subscription.user_id, subscription.product_id
                ),
                &params,
            ) && params
                .filters
                .get("status")
                .is_none_or(|status| subscription.status == *status)
                && params
                    .filters
                    .get("user_id")
                    .is_none_or(|user_id| subscription.user_id == *user_id)
                && params
                    .filters
                    .get("product_id")
                    .is_none_or(|product_id| subscription.product_id == *product_id)
                && params.filters.get("module_id").is_none_or(|module_id| {
                    subscription.module_id.as_deref() == Some(module_id.as_str())
                })
                && params
                    .filters
                    .get("company_id")
                    .is_none_or(|company_id| {
                        subscription.company_id.as_deref() == Some(company_id.as_str())
                    })
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn get_subscription(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Subscription> = (|| {
        let subscription = state
            .subscriptions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subscription not found"))?;
        require_subscription_owner(&req, &subscription)?;
        Ok(subscription)
    })();
    json::respond(result, false)
}

/// Change the plan of a subscription mid-cycle, applying the
/// subscription's proration policy.
///
/// `prorate` applies the change immediately: the unused share of
/// the old plan is credited, the new plan is charged for the
/// remaining time, and the new period starts now. `no_prorate`
/// stores the new plan as pending and applies it at the next period
/// end with no mid-cycle charge or credit. `credit_only` credits
/// the unused time now and applies the new price from the next
/// period. The `subscription.upgraded` / `subscription.downgraded`
/// event carries the proration lines (`credit`, `charge`, `net`,
/// `currency`) so billing can invoice them.
async fn change_subscription_plan(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ChangePlanRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Subscription> = (|| {
        let input = body.into_inner();
        let existing = state
            .subscriptions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subscription not found"))?;
        require_subscription_owner(&req, &existing)?;
        if !matches!(existing.status.as_str(), "active" | "trialing") {
            return Err(UnifiedError::unprocessable(format!(
                "A {} subscription cannot change plan",
                existing.status
            )));
        }
        let product = state
            .products
            .get(&input.product_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Product {} not found", input.product_id))
            })?;
        if product.status != "published" {
            return Err(UnifiedError::unprocessable(format!(
                "Product {} is not published",
                product.name
            )));
        }
        if product.pricing_model != "subscription" {
            return Err(UnifiedError::unprocessable(
                "The new product is not a subscription product",
            ));
        }
        let Some(new_billing_period) = product.billing_period.clone() else {
            return Err(UnifiedError::unprocessable(
                "The new product has no billing period",
            ));
        };
        let (new_price, tier_id) = match &input.tier_id {
            Some(tier_id) => {
                let tier = resolve_tier(&state, &product.id, tier_id)?;
                (tier.price, Some(tier.id.clone()))
            }
            None => (
                product.renewal_price.unwrap_or(product.price),
                None,
            ),
        };
        if !new_price.is_finite() || new_price < 0.0 {
            return Err(UnifiedError::bad_request(
                "The new plan price must be a non-negative number",
            ));
        }
        let now = now();
        let remaining_days = ((existing.current_period_end - now).num_seconds() as f64 / 86_400.0)
            .max(0.0);
        let period_total_days = period_days(&existing.billing_period) as f64;
        let new_period_total_days = period_days(&new_billing_period) as f64;
        let (credit, charge, net) = proration_amounts(
            &existing.proration_policy,
            existing.price,
            new_price,
            remaining_days,
            period_total_days,
            new_period_total_days,
        );
        let direction = if new_price >= existing.price {
            "upgraded"
        } else {
            "downgraded"
        };
        let event = format!("subscription.{direction}");
        let previous_product_id = existing.product_id.clone();
        let previous_price = existing.price;
        let mut subscription = state
            .subscriptions
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Subscription not found"))?;
        let effective_at = match existing.proration_policy.as_str() {
            // Immediate change: the new period starts now.
            "prorate" => {
                subscription.product_id = product.id.clone();
                subscription.tier_id = tier_id;
                subscription.price = round_money(new_price);
                subscription.billing_period = new_billing_period.clone();
                subscription.current_period_start = now;
                subscription.current_period_end = period_end(now, &new_billing_period);
                subscription.pending_product_id = None;
                subscription.pending_tier_id = None;
                subscription.pending_price = None;
                subscription.pending_billing_period = None;
                subscription.current_period_end
            }
            // Deferred change: the new plan waits for the next
            // period end; the current period runs out unchanged.
            _ => {
                subscription.pending_product_id = Some(product.id.clone());
                subscription.pending_tier_id = tier_id;
                subscription.pending_price = Some(round_money(new_price));
                subscription.pending_billing_period = Some(new_billing_period);
                subscription.current_period_end
            }
        };
        subscription.module_id = product.module_id.clone();
        subscription.updated_at = now;
        let updated = subscription.clone();
        drop(subscription);
        state.emit(
            &event,
            json!({
                "subscription": serde_json::to_value(&updated).unwrap_or(Value::Null),
                "subscription_id": updated.id,
                "previous_product_id": previous_product_id,
                "previous_price": previous_price,
                "proration": {
                    "policy": updated.proration_policy,
                    "credit": credit,
                    "charge": charge,
                    "net": net,
                    "currency": updated.currency,
                },
                "effective_at": effective_at,
            }),
        );
        emit_subscription_entitlement_changed(
            state,
            &updated,
            true,
            &format!("subscription_{direction}"),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

/// Cancel a subscription. With `cancel_at_period_end` (default)
/// the subscription stays active until the end of the current
/// period and access continues until then; with `false` it ends
/// immediately. Emits `subscription.cancelled` and, for module
/// products, `subscription.entitlement_changed`.
async fn cancel_subscription(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CancelSubscriptionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Subscription> = (|| {
        let cancel_at_period_end = body.into_inner().cancel_at_period_end;
        let existing = state
            .subscriptions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subscription not found"))?;
        require_subscription_owner(&req, &existing)?;
        if existing.status == "cancelled" {
            return Err(UnifiedError::conflict("Subscription is already cancelled"));
        }
        if !matches!(existing.status.as_str(), "active" | "trialing" | "past_due") {
            return Err(UnifiedError::unprocessable(format!(
                "A {} subscription cannot be cancelled",
                existing.status
            )));
        }
        let mut subscription = state
            .subscriptions
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Subscription not found"))?;
        let effective = if cancel_at_period_end {
            subscription.cancel_at_period_end = true;
            subscription.updated_at = now();
            "period_end"
        } else {
            subscription.status = "cancelled".to_string();
            subscription.cancel_at_period_end = false;
            subscription.cancelled_at = Some(now());
            subscription.updated_at = now();
            "immediate"
        };
        let updated = subscription.clone();
        drop(subscription);
        state.emit(
            "subscription.cancelled",
            json!({
                "subscription": serde_json::to_value(&updated).unwrap_or(Value::Null),
                "subscription_id": updated.id,
                "effective": effective,
                "reason": "cancelled_by_customer",
            }),
        );
        if effective == "immediate" {
            emit_subscription_entitlement_changed(
                state,
                &updated,
                false,
                "cancelled_by_customer",
            );
        }
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn list_orders(
    state: web::Data<Arc<StoreState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let sort = params.allowed_sort(&["created_at", "total"], "created_at");
    let mut items: Vec<Order> = state
        .orders
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|order| {
            matches_search(&format!("{} {}", order.id, order.user_id), &params)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| order.status == *status)
                && params
                    .filters
                    .get("user_id")
                    .is_none_or(|user_id| order.user_id == *user_id)
                && params.filters.get("company_id").is_none_or(|company_id| {
                    order.company_id.as_deref() == Some(company_id.as_str())
                })
                && params.filters.get("coupon_code").is_none_or(|code| {
                    order.coupon_code.as_deref() == Some(code.as_str())
                })
        })
        .collect();
    match sort.as_str() {
        "total" => items.sort_by(|a, b| a.total.partial_cmp(&b.total).unwrap_or(std::cmp::Ordering::Equal)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn get_order(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Order> = state
        .orders
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Order not found"));
    json::respond(result, false)
}

async fn update_order(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateOrder>,
) -> HttpResponse {
    let result: UnifiedResult<Order> = (|| {
        let input = body.into_inner();
        let Some(status) = input.status else {
            return state
                .orders
                .get(path.as_str())
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found("Order not found"));
        };
        let current = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        require_order_owner(&req, &current)?;
        let updated = transition_order(&state, path.as_str(), &status)?;
        let order_value = serde_json::to_value(&updated).unwrap_or(Value::Null);
        match status.as_str() {
            "paid" => {
                state.emit("payment.completed", order_value.clone());
                state.emit("order.paid", order_paid_value(&updated));
                activate_subscription_for_paid_order(&state, &updated);
                grant_digital_purchases_for_paid_order(&state, &updated);
            }
            "completed" => state.emit("store.order.completed", order_value),
            "refunded" => state.emit(
                "store.order.refunded",
                json!({
                    "order": order_value,
                    "license_ids": state
                        .issued_licenses
                        .iter()
                        .filter(|entry| entry.value().order_id.as_deref() == Some(path.as_str()))
                        .map(|entry| entry.value().license_id.clone())
                        .collect::<Vec<String>>(),
                }),
            ),
            "cancelled" => state.emit("store.order.cancelled", order_value),
            _ => {}
        }
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_order(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let order = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        require_order_owner(&req, &order)?;
        if order.status != "pending" && order.status != "cancelled" {
            return Err(UnifiedError::unprocessable(
                "Only pending or cancelled orders can be deleted",
            ));
        }
        state.orders.remove(path.as_str());
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, false)
}

async fn pay_order(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PayRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let provider = body.into_inner().provider.trim().to_string();
        if provider.is_empty() {
            return Err(UnifiedError::bad_request("Payment provider must not be empty"));
        }
        let mut order = state
            .orders
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        require_order_owner(&req, &order)?;
        if order.status == "paid" || order.status == "completed" {
            return Err(UnifiedError::conflict("Order is already paid"));
        }
        if order.status == "cancelled" || order.status == "refunded" {
            return Err(UnifiedError::unprocessable(
                "Cancelled or refunded orders cannot be paid",
            ));
        }
        if order.status != "pending" && order.status != "payment_pending" {
            return Err(UnifiedError::unprocessable(format!(
                "Order in status {} cannot be paid",
                order.status
            )));
        }
        order.provider = Some(provider.clone());
        if provider == "manual" {
            // The manual provider completes immediately; no gateway call.
            order.status = "paid".to_string();
            order.updated_at = now();
            let order_value = serde_json::to_value(&*order).unwrap_or(Value::Null);
            drop(order);
            let paid = state
                .orders
                .get(path.as_str())
                .map(|entry| entry.value().clone())
                .expect("order still present");
            state.emit("payment.completed", order_value.clone());
            state.emit("order.paid", order_paid_value(&paid));
            let subscription = activate_subscription_for_paid_order(&state, &paid);
            grant_digital_purchases_for_paid_order(&state, &paid);
            return Ok(json!({ "order": paid, "payment_status": "completed", "provider": provider, "subscription_id": subscription.map(|subscription| subscription.id) }));
        }
        // Any other provider is handed off through hooks: a real gateway
        // module listens to `payment.initiated` and completes the payment
        // asynchronously. Only "manual" completes synchronously today.
        order.status = "payment_pending".to_string();
        order.updated_at = now();
        let order_value = serde_json::to_value(&*order).unwrap_or(Value::Null);
        drop(order);
        state.emit(
            "payment.initiated",
            json!({
                "order": order_value,
                "provider": provider,
                "amount": state.orders.get(path.as_str()).map(|o| o.total).unwrap_or(0.0),
            }),
        );
        let pending = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("order still present");
        Ok(json!({ "order": pending, "payment_status": "pending", "provider": provider }))
    })();
    json::respond(result, false)
}

async fn complete_order(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Order> = (|| {
        let current = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        require_order_owner(&req, &current)?;
        let updated = transition_order(&state, path.as_str(), "completed")?;
        state.emit("store.order.completed", order_paid_value(&updated));
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn cancel_order(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Order> = (|| {
        let current = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        require_order_owner(&req, &current)?;
        let updated = transition_order(&state, path.as_str(), "cancelled")?;
        state.emit(
            "store.order.cancelled",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn refund_order(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: Option<web::Json<RefundRequest>>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let reason = body.and_then(|b| b.into_inner().reason);
        let current = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        require_order_owner(&req, &current)?;
        let updated = transition_order(&state, path.as_str(), "refunded")?;
        let license_ids: Vec<String> = state
            .issued_licenses
            .iter()
            .filter(|entry| entry.value().order_id.as_deref() == Some(path.as_str()))
            .map(|entry| entry.value().license_id.clone())
            .collect();
        let cancelled_subscriptions =
            cancel_subscriptions_for_order(&state, path.as_str(), "order_refunded");
        state.emit(
            "store.order.refunded",
            json!({
                "order": serde_json::to_value(&updated).unwrap_or(Value::Null),
                "order_id": updated.id,
                "license_ids": license_ids,
                "cancelled_subscriptions": cancelled_subscriptions,
                "reason": reason,
            }),
        );
        Ok(json!({
            "order": updated,
            "license_ids": license_ids,
            "cancelled_subscriptions": cancelled_subscriptions,
            "reason": reason,
        }))
    })();
    json::respond(result, false)
}

async fn download_order(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let order = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        if order.status != "paid" && order.status != "completed" {
            return Err(UnifiedError::forbidden(
                "Digital delivery is only available for paid orders",
            ));
        }
        let file_id = order
            .items
            .iter()
            .find_map(|item| item.file_id.clone())
            .ok_or_else(|| {
                UnifiedError::unprocessable("This order has no downloadable file attached")
            })?;
        let job_id = new_id();
        state.emit(
            events::DOWNLOAD_STARTED,
            json!({
                "order_id": order.id,
                "user_id": order.user_id,
                "file_id": file_id,
                "job_id": job_id,
            }),
        );
        Ok(json!({
            "download": {
                "job_id": job_id,
                "order_id": order.id,
                "file_id": file_id,
                "status": "ready",
                "expires_at": (now() + chrono::Duration::hours(24)).to_rfc3339(),
            }
        }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Entitlements and signed downloads
// ---------------------------------------------------------------------------

async fn list_entitlements(
    state: web::Data<Arc<StoreState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<DownloadEntitlement> = state
        .entitlements
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entitlement| {
            matches_search(
                &format!(
                    "{} {}",
                    entitlement.license_key.clone().unwrap_or_default(),
                    entitlement.software_slug.clone().unwrap_or_default()
                ),
                &params,
            ) && params.filters.get("license_id").is_none_or(|id| {
                entitlement.license_id.as_deref() == Some(id.as_str())
            }) && params.filters.get("software_id").is_none_or(|id| {
                entitlement.software_id.as_deref() == Some(id.as_str())
            }) && params.filters.get("software_slug").is_none_or(|slug| {
                entitlement.software_slug.as_deref() == Some(slug.as_str())
            }) && params.filters.get("order_id").is_none_or(|id| {
                entitlement.order_id == *id
            }) && params.filters.get("user_id").is_none_or(|id| {
                entitlement.user_id == *id
            }) && params.filters.get("status").is_none_or(|status| {
                entitlement.status == *status
            })
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn get_entitlement(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<DownloadEntitlement> = state
        .entitlements
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Download entitlement not found"));
    json::respond(result, false)
}

async fn create_download_link(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
    body: Option<web::Json<DownloadLinkRequest>>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let entitlement = state
            .entitlements
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Download entitlement not found"))?;
        if entitlement.status == "revoked" {
            return Err(UnifiedError::forbidden(
                "Download entitlement has been revoked",
            ));
        }
        if let Some(expires_at) = entitlement.expires_at {
            if expires_at <= now() {
                return Err(UnifiedError::forbidden(
                    "Download entitlement has expired",
                ));
            }
        }
        if entitlement.downloads_used >= entitlement.download_limit {
            return Err(UnifiedError::forbidden(
                "Download limit reached for this license",
            ));
        }
        let license_id = entitlement
            .license_id
            .clone()
            .ok_or_else(|| {
                UnifiedError::unprocessable(
                    "Entitlement has no license attached; the license was not issued",
                )
            })?;
        let release_id = body
            .and_then(|b| b.into_inner().release_id)
            .or_else(|| entitlement.release_id.clone())
            .ok_or_else(|| {
                UnifiedError::unprocessable(
                    "No release is linked to this entitlement; pass release_id",
                )
            })?;
        let (download_url, expires_at) = build_signed_url(&license_id, &release_id);
        Ok(json!({
            "download_url": download_url,
            "license_id": license_id,
            "release_id": release_id,
            "expires_at": expires_at.to_rfc3339(),
            "downloads_used": entitlement.downloads_used,
            "download_limit": entitlement.download_limit,
        }))
    })();
    json::respond(result, false)
}

async fn signed_download(
    state: web::Data<Arc<StoreState>>,
    query: web::Query<SignedDownloadQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let input = query.into_inner();
        let expires_at = DateTime::from_timestamp(input.expires, 0)
            .ok_or_else(|| UnifiedError::bad_request("Invalid expiry timestamp"))?;
        if expires_at <= now() {
            return Err(UnifiedError::forbidden("Download link has expired"));
        }
        let expected = sign_download(&input.license_id, &input.release_id, input.expires);
        if !signature_matches(&expected, &input.sig) {
            return Err(UnifiedError::forbidden("Download link signature is invalid"));
        }
        let entitlement_id = state
            .entitlements
            .iter()
            .find(|entry| {
                entry.value().license_id.as_deref() == Some(input.license_id.as_str())
                    && entry
                        .value()
                        .release_id
                        .as_deref()
                        .is_none_or(|release_id| release_id == input.release_id)
            })
            .map(|entry| entry.key().clone())
            .ok_or_else(|| {
                UnifiedError::forbidden(
                    "No download entitlement exists for this license and release",
                )
            })?;
        let entitlement = record_download(&state, &entitlement_id, &input.release_id)?;
        // A digital file resolves through the digital file map (its
        // file id is the `release_id` of the entitlement); software
        // releases resolve through the catalog as before.
        let digital_file = state
            .digital_files
            .get(&input.release_id)
            .map(|entry| entry.value().clone());
        let file_id = catalog_release_file(&state, &input.release_id)
            .or_else(|| digital_file.as_ref().map(|file| file.storage_file_id.clone()));
        let mut body = json!({
            "download": {
                "license_id": input.license_id,
                "release_id": input.release_id,
                "software_slug": entitlement.software_slug,
                "file_id": file_id,
                "status": "ready",
                "downloads_used": entitlement.downloads_used,
                "download_limit": entitlement.download_limit,
                "expires_at": expires_at.to_rfc3339(),
            }
        });
        // A digital download is watermarked and recorded in the
        // per-buyer digital download history. The descriptor tells
        // the delivery layer which metadata fields to embed; the
        // byte-level embedding itself happens there, from this
        // descriptor (see `watermark_descriptor`).
        if let Some(file) = digital_file {
            let watermark = watermark_descriptor(
                &state,
                &file.format,
                &entitlement.user_id,
                &entitlement.order_id,
            );
            let watermark_applied = watermark
                .get("metadata_supported")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let record = DigitalDownload {
                id: new_id(),
                digital_product_id: file.digital_product_id.clone(),
                file_id: Some(file.id.clone()),
                preview_id: None,
                buyer_user_id: entitlement.user_id.clone(),
                order_id: Some(entitlement.order_id.clone()),
                grant_id: entitlement.license_id.clone(),
                kind: "purchase".to_string(),
                watermark_subject: Some(entitlement.user_id.clone()),
                watermark_applied,
                watermark_formats: if watermark_applied {
                    vec![file.format.clone()]
                } else {
                    Vec::new()
                },
                downloaded_at: now(),
            };
            state
                .digital_downloads
                .insert(record.id.clone(), record.clone());
            state.emit(
                "store.digital.download",
                json!({
                    "download": serde_json::to_value(&record).unwrap_or(Value::Null),
                    "watermark": watermark.clone(),
                    "file_id": file.storage_file_id,
                }),
            );
            if let Some(download) = body
                .get_mut("download")
                .and_then(|value| value.as_object_mut())
            {
                download.insert(
                    "digital_product_id".to_string(),
                    json!(file.digital_product_id),
                );
                download.insert("watermark".to_string(), watermark);
            }
        }
        Ok(body)
    })();
    json::respond(result, false)
}

async fn list_license_downloads(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let license_id = path.into_inner();
    let mut items: Vec<DownloadRecord> = state
        .downloads
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            record.license_id == license_id
                && params
                    .filters
                    .get("release_id")
                    .is_none_or(|release_id| record.release_id == *release_id)
        })
        .collect();
    items.sort_by(|a, b| a.downloaded_at.cmp(&b.downloaded_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

// ---------------------------------------------------------------------------
// Update API (WordPress-style version checks and package downloads)
// ---------------------------------------------------------------------------

async fn list_catalog(
    state: web::Data<Arc<StoreState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<CatalogEntry> = state
        .catalog
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            matches_search(&format!("{} {} {}", entry.slug, entry.name, entry.changelog), &params)
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| entry.kind == *kind)
                && params.filters.get("slug").is_none_or(|slug| entry.slug == *slug)
                && params.filters.get("product_id").is_none_or(|id| {
                    entry.product_id.as_deref() == Some(id.as_str())
                })
                && params.filters.get("license_required").is_none_or(|flag| {
                    entry.license_required == (*flag == "true" || *flag == "1")
                })
        })
        .collect();
    items.sort_by(|a, b| a.slug.cmp(&b.slug));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_catalog_entry(
    state: web::Data<Arc<StoreState>>,
    body: web::Json<CreateCatalogEntry>,
) -> HttpResponse {
    let result: UnifiedResult<CatalogEntry> = (|| {
        let input = body.into_inner();
        let slug = input.slug.trim().to_string();
        if slug.is_empty() {
            return Err(UnifiedError::bad_request("Slug must not be empty"));
        }
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Name must not be empty"));
        }
        if !valid_kind(&input.kind) {
            return Err(UnifiedError::bad_request(
                "Kind must be one of: theme, plugin, software",
            ));
        }
        if Version::parse(&input.latest_version).is_err() {
            return Err(UnifiedError::bad_request(
                "Latest version is not a valid semantic version (expected MAJOR.MINOR.PATCH)",
            ));
        }
        if let Some(product_id) = &input.product_id {
            if !state.products.contains_key(product_id) {
                return Err(UnifiedError::not_found(format!(
                    "Product {product_id} not found"
                )));
            }
        }
        if state.catalog.contains_key(&slug) {
            return Err(UnifiedError::conflict(
                "A catalog entry with this slug already exists",
            ));
        }
        let entry = CatalogEntry {
            slug: slug.clone(),
            name: input.name.trim().to_string(),
            kind: input.kind,
            product_id: input.product_id,
            product_name: input
                .product_name
                .unwrap_or_else(|| input.name.trim().to_string()),
            latest_version: input.latest_version.trim().to_string(),
            release_id: input.release_id,
            file_id: input.file_id,
            changelog: input.changelog.unwrap_or_default(),
            requires_wp: input.requires_wp,
            tested_wp: input.tested_wp,
            requires_php: input.requires_php,
            license_required: input.license_required.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state.catalog.insert(slug, entry.clone());
        Ok(entry)
    })();
    respond_created(result)
}

async fn get_catalog_entry(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<CatalogEntry> = state
        .catalog
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Catalog entry not found"));
    json::respond(result, false)
}

async fn update_catalog_entry(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
    body: web::Json<UpdateCatalogEntry>,
) -> HttpResponse {
    let result: UnifiedResult<CatalogEntry> = (|| {
        let input = body.into_inner();
        let mut entry = state
            .catalog
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Catalog entry not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Name must not be empty"));
            }
            entry.name = name.trim().to_string();
        }
        if let Some(kind) = input.kind {
            if !valid_kind(&kind) {
                return Err(UnifiedError::bad_request(
                    "Kind must be one of: theme, plugin, software",
                ));
            }
            entry.kind = kind;
        }
        if let Some(product_id) = input.product_id {
            if !state.products.contains_key(&product_id) {
                return Err(UnifiedError::not_found(format!(
                    "Product {product_id} not found"
                )));
            }
            entry.product_id = Some(product_id);
        }
        if let Some(product_name) = input.product_name {
            entry.product_name = product_name;
        }
        if let Some(latest_version) = input.latest_version {
            if Version::parse(&latest_version).is_err() {
                return Err(UnifiedError::bad_request(
                    "Latest version is not a valid semantic version (expected MAJOR.MINOR.PATCH)",
                ));
            }
            entry.latest_version = latest_version.trim().to_string();
        }
        if let Some(release_id) = input.release_id {
            entry.release_id = Some(release_id);
        }
        if let Some(file_id) = input.file_id {
            entry.file_id = Some(file_id);
        }
        if let Some(changelog) = input.changelog {
            entry.changelog = changelog;
        }
        if let Some(requires_wp) = input.requires_wp {
            entry.requires_wp = Some(requires_wp);
        }
        if let Some(tested_wp) = input.tested_wp {
            entry.tested_wp = Some(tested_wp);
        }
        if let Some(requires_php) = input.requires_php {
            entry.requires_php = Some(requires_php);
        }
        if let Some(license_required) = input.license_required {
            entry.license_required = license_required;
        }
        entry.updated_at = now();
        Ok(entry.clone())
    })();
    json::respond(result, false)
}

async fn delete_catalog_entry(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .catalog
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "slug": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Catalog entry not found"))
    })();
    json::respond(result, false)
}

/// Ask the licensing module through the hook bus whether a key is valid
/// for a product. Returns (valid, license_id). No validator registered
/// means the key cannot be trusted, so the answer is not valid.
fn validate_license_via_hook(
    state: &StoreState,
    license_key: &str,
    product_name: &str,
) -> (bool, Option<String>) {
    if !state.bus().has("license.validate_requested") {
        return (false, None);
    }
    let context = state.bus().emit(
        "license.validate_requested",
        json!({
            "license_key": license_key,
            "product": product_name,
        }),
    );
    for result in &context.results {
        if result.get("valid").and_then(|value| value.as_bool()) == Some(true) {
            let license_id = result
                .get("license_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            return (true, license_id);
        }
    }
    (false, None)
}

fn entitlement_for_license(
    state: &StoreState,
    license_id: &str,
    slug: &str,
) -> Option<DownloadEntitlement> {
    state
        .entitlements
        .iter()
        .find(|entry| {
            entry.value().license_id.as_deref() == Some(license_id)
                && entry.value().software_slug.as_deref() == Some(slug)
        })
        .map(|entry| entry.value().clone())
}

/// WordPress-style version check: given a slug and the installed
/// version, return the latest version, whether an update is available,
/// compatibility metadata, and a signed download URL only when the
/// caller is entitled (valid license plus a download entitlement with
/// downloads left, or a free package that needs no license).
async fn check_update(
    state: web::Data<Arc<StoreState>>,
    body: web::Json<UpdateCheckRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        let entry = state
            .catalog
            .get(&input.slug)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Software not found"))?;
        let update_available = match (
            Version::parse(&input.current_version),
            Version::parse(&entry.latest_version),
        ) {
            (Ok(current), Ok(latest)) => latest > current,
            _ => entry.latest_version != input.current_version,
        };
        let mut entitled = false;
        let mut download_url: Option<String> = None;
        if !entry.license_required {
            entitled = true;
            download_url = Some(format!("/api/v1/store/updates/{}/package", entry.slug));
        } else if let Some(license_key) = input.license_key.filter(|key| !key.trim().is_empty()) {
            let (valid, license_id) =
                validate_license_via_hook(&state, &license_key, &entry.product_name);
            if valid {
                if let Some(license_id) = license_id {
                    if let Some(entitlement) =
                        entitlement_for_license(&state, &license_id, &entry.slug)
                    {
                        let usable = entitlement.status == "active"
                            && entitlement.downloads_used < entitlement.download_limit
                            && entitlement
                                .expires_at
                                .is_none_or(|expires_at| expires_at > now());
                        if usable {
                            if let Some(release_id) = entitlement.release_id.clone() {
                                entitled = true;
                                let (url, _) = build_signed_url(&license_id, &release_id);
                                download_url = Some(url);
                            }
                        }
                    }
                }
            }
        }
        state.emit(
            "software.update.checked",
            json!({
                "slug": entry.slug,
                "current_version": input.current_version,
                "latest_version": entry.latest_version,
                "update_available": update_available,
                "entitled": entitled,
            }),
        );
        Ok(json!({
            "slug": entry.slug,
            "name": entry.name,
            "kind": entry.kind,
            "current_version": input.current_version,
            "latest_version": entry.latest_version,
            "update_available": update_available,
            "changelog": entry.changelog,
            "requires_wp": entry.requires_wp,
            "tested_wp": entry.tested_wp,
            "requires_php": entry.requires_php,
            "license_required": entry.license_required,
            "entitled": entitled,
            "download_url": download_url,
        }))
    })();
    json::respond(result, false)
}

/// Download the latest package for a slug. Licensed packages require a
/// valid license key and a download entitlement with downloads left;
/// each successful download counts against the entitlement limit.
async fn download_package(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
    query: web::Query<PackageQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let slug = path.into_inner();
        let entry = state
            .catalog
            .get(&slug)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Software not found"))?;
        let release_id = entry.release_id.clone().ok_or_else(|| {
            UnifiedError::unprocessable("No release is linked to this software")
        })?;
        if !entry.license_required {
            state.emit(
                events::DOWNLOAD_STARTED,
                json!({
                    "slug": slug,
                    "release_id": release_id,
                    "file_id": entry.file_id,
                }),
            );
            return Ok(json!({
                "package": {
                    "slug": entry.slug,
                    "name": entry.name,
                    "version": entry.latest_version,
                    "release_id": release_id,
                    "file_id": entry.file_id,
                    "status": "ready",
                }
            }));
        }
        let license_key = query
            .into_inner()
            .license_key
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| {
                UnifiedError::forbidden(
                    "This package requires a valid license key (license_key query parameter)",
                )
            })?;
        let (valid, license_id) =
            validate_license_via_hook(&state, &license_key, &entry.product_name);
        if !valid {
            return Err(UnifiedError::forbidden(
                "License key is not valid for this product",
            ));
        }
        let license_id = license_id.ok_or_else(|| {
            UnifiedError::forbidden("License key is not valid for this product")
        })?;
        let entitlement = entitlement_for_license(&state, &license_id, &slug).ok_or_else(|| {
            UnifiedError::forbidden(
                "No download entitlement exists for this license and product",
            )
        })?;
        let updated = record_download(&state, &entitlement.id, &release_id)?;
        let (download_url, expires_at) = build_signed_url(&license_id, &release_id);
        Ok(json!({
            "package": {
                "slug": entry.slug,
                "name": entry.name,
                "version": entry.latest_version,
                "release_id": release_id,
                "file_id": entry.file_id,
                "status": "ready",
                "download_url": download_url,
                "download_expires_at": expires_at.to_rfc3339(),
                "downloads_used": updated.downloads_used,
                "download_limit": updated.download_limit,
            }
        }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Digital products: types, products, files, previews, samples
// ---------------------------------------------------------------------------

/// Load a digital product for a read that may come from the public
/// storefront: drafts are invisible to anyone without the manage
/// permission (answered as "not found" so drafts do not leak).
fn digital_product_for_read(
    state: &StoreState,
    req: &HttpRequest,
    id: &str,
) -> UnifiedResult<DigitalProduct> {
    let digital = state
        .digital_products
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Digital product not found"))?;
    if digital.status != "published" && !digital_can_manage(state, req) {
        return Err(UnifiedError::not_found("Digital product not found"));
    }
    Ok(digital)
}

fn load_digital_product(state: &StoreState, id: &str) -> UnifiedResult<DigitalProduct> {
    state
        .digital_products
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Digital product not found"))
}

async fn list_digital_types(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<DigitalProductType>> = (|| {
        require_digital_read(&state, &req)?;
        let params = query.into_inner();
        let mut items: Vec<DigitalProductType> = state
            .digital_types
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|digital_type| {
                matches_search(
                    &format!(
                        "{} {} {}",
                        digital_type.id, digital_type.label, digital_type.description
                    ),
                    &params,
                ) && params.filters.get("enabled").is_none_or(|flag| {
                    digital_type.enabled == (*flag == "true" || *flag == "1")
                })
            })
            .collect();
        items.sort_by(|a, b| a.id.cmp(&b.id));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn create_digital_type(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<CreateDigitalProductType>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalProductType> = (|| {
        require_digital_manage(&state, &req)?;
        let input = body.into_inner();
        let id = input.id.trim().to_lowercase();
        if !valid_type_slug(&id) {
            return Err(UnifiedError::bad_request(
                "Type id must be a lowercase slug of letters, digits, and hyphens (at most 32 characters)",
            ));
        }
        if state.digital_types.contains_key(&id) {
            return Err(UnifiedError::conflict(
                "A digital product type with this id already exists",
            ));
        }
        let label = input
            .label
            .filter(|label| !label.trim().is_empty())
            .map(|label| label.trim().to_string())
            .unwrap_or_else(|| id.clone());
        if label.len() > 80 {
            return Err(UnifiedError::bad_request(
                "Type label must be at most 80 characters",
            ));
        }
        let description = input.description.unwrap_or_default();
        if description.len() > 500 {
            return Err(UnifiedError::bad_request(
                "Type description must be at most 500 characters",
            ));
        }
        let digital_type = DigitalProductType {
            id: id.clone(),
            label,
            description,
            enabled: input.enabled.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state
            .digital_types
            .insert(id, digital_type.clone());
        Ok(digital_type)
    })();
    respond_created(result)
}

async fn update_digital_type(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDigitalProductType>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalProductType> = (|| {
        require_digital_manage(&state, &req)?;
        let input = body.into_inner();
        let mut digital_type = state
            .digital_types
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Digital product type not found"))?;
        if let Some(label) = input.label {
            if label.trim().is_empty() || label.len() > 80 {
                return Err(UnifiedError::bad_request(
                    "Type label must not be empty and must be at most 80 characters",
                ));
            }
            digital_type.label = label.trim().to_string();
        }
        if let Some(description) = input.description {
            if description.len() > 500 {
                return Err(UnifiedError::bad_request(
                    "Type description must be at most 500 characters",
                ));
            }
            digital_type.description = description;
        }
        if let Some(enabled) = input.enabled {
            digital_type.enabled = enabled;
        }
        digital_type.updated_at = now();
        Ok(digital_type.clone())
    })();
    json::respond(result, false)
}

async fn delete_digital_type(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        require_digital_manage(&state, &req)?;
        let id = path.as_str().to_string();
        let in_use = state
            .digital_products
            .iter()
            .any(|entry| entry.value().type_id == id);
        if in_use {
            return Err(UnifiedError::conflict(
                "Digital product type is still used by digital products; disable it instead of deleting it",
            ));
        }
        state
            .digital_types
            .remove(&id)
            .map(|_| json!({ "deleted": true, "id": id }))
            .ok_or_else(|| UnifiedError::not_found("Digital product type not found"))
    })();
    json::respond(result, false)
}

async fn list_digital_products(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<DigitalProduct>> = (|| {
        require_digital_public(&state, &req)?;
        let params = query.into_inner();
        let show_all = digital_can_manage(&state, &req);
        let mut items: Vec<DigitalProduct> = state
            .digital_products
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|digital| {
                (show_all || digital.status == "published")
                    && params
                        .filters
                        .get("type_id")
                        .is_none_or(|type_id| digital.type_id == *type_id)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| digital.status == *status)
                    && params
                        .filters
                        .get("product_id")
                        .is_none_or(|product_id| digital.product_id == *product_id)
                    && params.filters.get("author").is_none_or(|author| {
                        digital.author.as_deref() == Some(author.as_str())
                    })
                    && matches_search(&digital_search_text(&state, digital), &params)
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn create_digital_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<CreateDigitalProduct>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalProduct> = (|| {
        require_digital_manage(&state, &req)?;
        let input = body.into_inner();
        if !state.products.contains_key(&input.product_id) {
            return Err(UnifiedError::not_found(format!(
                "Product {} not found",
                input.product_id
            )));
        }
        let type_id = input.type_id.trim().to_lowercase();
        match state.digital_types.get(&type_id) {
            Some(digital_type) if digital_type.enabled => {}
            Some(_) => {
                return Err(UnifiedError::unprocessable(format!(
                    "Digital product type \"{type_id}\" is disabled"
                )));
            }
            None => {
                return Err(UnifiedError::not_found(format!(
                    "Digital product type \"{type_id}\" not found"
                )));
            }
        }
        if state
            .digital_products
            .iter()
            .any(|entry| entry.value().product_id == input.product_id)
        {
            return Err(UnifiedError::conflict(
                "This product already has a digital profile",
            ));
        }
        validate_digital_metadata(
            input.isbn.as_deref(),
            input.language.as_deref(),
            input.pages,
            input.formats.as_deref(),
        )?;
        let royalty_kind = input
            .royalty_kind
            .unwrap_or_else(default_royalty_kind);
        let royalty_value = input.royalty_value.unwrap_or(DEFAULT_ROYALTY_VALUE);
        validate_royalty(&royalty_kind, royalty_value)?;
        if let Some(status) = &input.status {
            if !valid_product_status(status) {
                return Err(UnifiedError::bad_request(
                    "Digital product status must be one of: draft, published",
                ));
            }
        }
        if let Some(download_limit) = input.download_limit {
            if download_limit > 10_000 {
                return Err(UnifiedError::bad_request(
                    "Download limit must be at most 10000 (0 means the configured default)",
                ));
            }
        }
        let formats: Vec<String> = input
            .formats
            .unwrap_or_default()
            .into_iter()
            .map(|format| format.trim().to_lowercase())
            .collect();
        let digital = DigitalProduct {
            id: new_id(),
            product_id: input.product_id,
            type_id,
            author: input.author.filter(|value| !value.trim().is_empty()),
            publisher: input.publisher.filter(|value| !value.trim().is_empty()),
            edition: input.edition.filter(|value| !value.trim().is_empty()),
            isbn: input.isbn.filter(|value| !value.trim().is_empty()),
            language: input.language.filter(|value| !value.trim().is_empty()),
            pages: input.pages,
            formats,
            cover_storage_id: input
                .cover_storage_id
                .filter(|value| !value.trim().is_empty()),
            sample_allowed: input.sample_allowed.unwrap_or(false),
            download_limit: input.download_limit.unwrap_or(0),
            royalty_kind,
            royalty_value,
            royalty_payee: input.royalty_payee.filter(|value| !value.trim().is_empty()),
            royalty_payee_company_id: input
                .royalty_payee_company_id
                .filter(|value| !value.trim().is_empty()),
            status: input.status.unwrap_or_else(default_product_status),
            created_at: now(),
            updated_at: now(),
        };
        state
            .digital_products
            .insert(digital.id.clone(), digital.clone());
        state.emit(
            "store.digital.created",
            serde_json::to_value(&digital).unwrap_or(Value::Null),
        );
        Ok(digital)
    })();
    respond_created(result)
}

async fn get_digital_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalProduct> = (|| {
        require_digital_public(&state, &req)?;
        digital_product_for_read(&state, &req, path.as_str())
    })();
    json::respond(result, false)
}

async fn update_digital_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDigitalProduct>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalProduct> = (|| {
        require_digital_manage(&state, &req)?;
        let input = body.into_inner();
        // Validate the new type and royalty pair before touching the
        // stored record, so a rejected update never leaves a
        // half-mutated record behind.
        if let Some(type_id) = &input.type_id {
            let type_id = type_id.trim().to_lowercase();
            match state.digital_types.get(&type_id) {
                Some(digital_type) if digital_type.enabled => {}
                Some(_) => {
                    return Err(UnifiedError::unprocessable(format!(
                        "Digital product type \"{type_id}\" is disabled"
                    )));
                }
                None => {
                    return Err(UnifiedError::not_found(format!(
                        "Digital product type \"{type_id}\" not found"
                    )));
                }
            }
        }
        validate_digital_metadata(
            input.isbn.as_deref(),
            input.language.as_deref(),
            input.pages,
            input.formats.as_deref(),
        )?;
        let existing = load_digital_product(&state, path.as_str())?;
        let royalty_kind = input
            .royalty_kind
            .clone()
            .unwrap_or_else(|| existing.royalty_kind.clone());
        let royalty_value = input.royalty_value.unwrap_or(existing.royalty_value);
        validate_royalty(&royalty_kind, royalty_value)?;
        if let Some(status) = &input.status {
            if !valid_product_status(status) {
                return Err(UnifiedError::bad_request(
                    "Digital product status must be one of: draft, published",
                ));
            }
        }
        let mut digital = state
            .digital_products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Digital product not found"))?;
        if let Some(type_id) = input.type_id {
            digital.type_id = type_id.trim().to_lowercase();
        }
        if let Some(author) = input.author {
            digital.author = if author.trim().is_empty() {
                None
            } else {
                Some(author.trim().to_string())
            };
        }
        if let Some(publisher) = input.publisher {
            digital.publisher = if publisher.trim().is_empty() {
                None
            } else {
                Some(publisher.trim().to_string())
            };
        }
        if let Some(edition) = input.edition {
            digital.edition = if edition.trim().is_empty() {
                None
            } else {
                Some(edition.trim().to_string())
            };
        }
        if let Some(isbn) = input.isbn {
            digital.isbn = if isbn.trim().is_empty() {
                None
            } else {
                Some(isbn.trim().to_string())
            };
        }
        if let Some(language) = input.language {
            digital.language = if language.trim().is_empty() {
                None
            } else {
                Some(language.trim().to_string())
            };
        }
        if let Some(pages) = input.pages {
            digital.pages = Some(pages);
        }
        if let Some(formats) = input.formats {
            digital.formats = formats
                .into_iter()
                .map(|format| format.trim().to_lowercase())
                .collect();
        }
        if let Some(cover_storage_id) = input.cover_storage_id {
            digital.cover_storage_id = if cover_storage_id.trim().is_empty() {
                None
            } else {
                Some(cover_storage_id.trim().to_string())
            };
        }
        if let Some(sample_allowed) = input.sample_allowed {
            digital.sample_allowed = sample_allowed;
        }
        if let Some(download_limit) = input.download_limit {
            if download_limit > 10_000 {
                return Err(UnifiedError::bad_request(
                    "Download limit must be at most 10000 (0 means the configured default)",
                ));
            }
            digital.download_limit = download_limit;
        }
        digital.royalty_kind = royalty_kind;
        digital.royalty_value = royalty_value;
        if let Some(royalty_payee) = input.royalty_payee {
            digital.royalty_payee = if royalty_payee.trim().is_empty() {
                None
            } else {
                Some(royalty_payee.trim().to_string())
            };
        }
        if let Some(royalty_payee_company_id) = input.royalty_payee_company_id {
            digital.royalty_payee_company_id = if royalty_payee_company_id.trim().is_empty()
            {
                None
            } else {
                Some(royalty_payee_company_id.trim().to_string())
            };
        }
        if let Some(status) = input.status {
            digital.status = status;
        }
        digital.updated_at = now();
        Ok(digital.clone())
    })();
    json::respond(result, false)
}

async fn delete_digital_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        require_digital_manage(&state, &req)?;
        let id = path.as_str().to_string();
        let _digital = load_digital_product(&state, &id)?;
        if state
            .digital_files
            .iter()
            .any(|entry| entry.value().digital_product_id == id)
        {
            return Err(UnifiedError::conflict(
                "Digital product still has files; delete the files first",
            ));
        }
        if state
            .digital_previews
            .iter()
            .any(|entry| entry.value().digital_product_id == id)
        {
            return Err(UnifiedError::conflict(
                "Digital product still has previews; delete the previews first",
            ));
        }
        if state
            .digital_royalties
            .iter()
            .any(|entry| entry.value().digital_product_id == id)
        {
            return Err(UnifiedError::conflict(
                "Digital product has royalty history; it cannot be deleted",
            ));
        }
        if state
            .digital_downloads
            .iter()
            .any(|entry| entry.value().digital_product_id == id)
        {
            return Err(UnifiedError::conflict(
                "Digital product has download history; it cannot be deleted",
            ));
        }
        state
            .digital_products
            .remove(&id)
            .map(|_| json!({ "deleted": true, "id": id }))
            .ok_or_else(|| UnifiedError::not_found("Digital product not found"))
    })();
    json::respond(result, false)
}

fn set_digital_product_status(
    state: &StoreState,
    req: &HttpRequest,
    id: &str,
    status: &str,
    event: &str,
) -> UnifiedResult<DigitalProduct> {
    require_digital_manage(state, req)?;
    let mut digital = state
        .digital_products
        .get_mut(id)
        .ok_or_else(|| UnifiedError::not_found("Digital product not found"))?;
    if digital.status == status {
        return Err(UnifiedError::conflict(format!(
            "Digital product is already {status}"
        )));
    }
    digital.status = status.to_string();
    digital.updated_at = now();
    let updated = digital.clone();
    drop(digital);
    state.emit(event, serde_json::to_value(&updated).unwrap_or(Value::Null));
    Ok(updated)
}

async fn publish_digital_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result = set_digital_product_status(
        &state,
        &req,
        path.as_str(),
        "published",
        "store.digital.published",
    );
    json::respond(result, false)
}

async fn unpublish_digital_product(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result = set_digital_product_status(
        &state,
        &req,
        path.as_str(),
        "draft",
        "store.digital.unpublished",
    );
    json::respond(result, false)
}

async fn list_digital_files(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<DigitalProductFile>> = (|| {
        require_digital_read(&state, &req)?;
        let digital = digital_product_for_read(&state, &req, path.as_str())?;
        let params = query.into_inner();
        let mut items: Vec<DigitalProductFile> = state
            .digital_files
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|file| {
                file.digital_product_id == digital.id
                    && matches_search(
                        &format!("{} {}", file.format, file.label.clone().unwrap_or_default()),
                        &params,
                    )
                    && params
                        .filters
                        .get("format")
                        .is_none_or(|format| file.format == *format)
                    && params.filters.get("is_primary").is_none_or(|flag| {
                        file.is_primary == (*flag == "true" || *flag == "1")
                    })
            })
            .collect();
        items.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(a.created_at.cmp(&b.created_at))
        });
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn create_digital_file(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateDigitalFile>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalProductFile> = (|| {
        require_digital_manage(&state, &req)?;
        let digital = load_digital_product(&state, path.as_str())?;
        let input = body.into_inner();
        let storage_file_id = input.storage_file_id.trim().to_string();
        if storage_file_id.is_empty() || storage_file_id.len() > 512 {
            return Err(UnifiedError::bad_request(
                "Storage file id must not be empty and must be at most 512 characters",
            ));
        }
        let format = input.format.trim().to_lowercase();
        if !valid_format(&format) {
            return Err(UnifiedError::bad_request(
                "Format must be a lowercase alphanumeric token such as pdf or epub",
            ));
        }
        if let Some(checksum) = &input.checksum {
            let checksum = checksum.trim();
            if !checksum.is_empty()
                && (checksum.len() < 32
                    || checksum.len() > 128
                    || !checksum.chars().all(|c| c.is_ascii_hexdigit()))
            {
                return Err(UnifiedError::bad_request(
                    "Checksum must be a hex digest of 32 to 128 characters",
                ));
            }
        }
        if let Some(file_size_bytes) = input.file_size_bytes {
            if file_size_bytes == 0 {
                return Err(UnifiedError::bad_request(
                    "File size must be greater than zero when set",
                ));
            }
        }
        let is_primary = input.is_primary.unwrap_or(false);
        if is_primary {
            for mut entry in state.digital_files.iter_mut() {
                if entry.value().digital_product_id == digital.id {
                    entry.value().is_primary = false;
                    entry.value().updated_at = now();
                }
            }
        }
        let file = DigitalProductFile {
            id: new_id(),
            digital_product_id: digital.id.clone(),
            storage_file_id,
            format,
            label: input.label.filter(|value| !value.trim().is_empty()),
            file_size_bytes: input.file_size_bytes,
            checksum: input.checksum.filter(|value| !value.trim().is_empty()),
            is_primary,
            sort_order: input.sort_order.unwrap_or(0),
            created_at: now(),
            updated_at: now(),
        };
        state.digital_files.insert(file.id.clone(), file.clone());
        Ok(file)
    })();
    respond_created(result)
}

async fn delete_digital_file(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        require_digital_manage(&state, &req)?;
        let id = path.as_str().to_string();
        let file = state
            .digital_files
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Digital file not found"))?;
        if state
            .entitlements
            .iter()
            .any(|entry| entry.value().release_id.as_deref() == Some(id.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Digital file has download entitlements; it cannot be deleted",
            ));
        }
        if state
            .digital_downloads
            .iter()
            .any(|entry| entry.value().file_id.as_deref() == Some(id.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Digital file has download history; it cannot be deleted",
            ));
        }
        state
            .digital_files
            .remove(&id)
            .map(|_| json!({ "deleted": true, "id": file.id }))
            .ok_or_else(|| UnifiedError::not_found("Digital file not found"))
    })();
    json::respond(result, false)
}

async fn list_digital_previews(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<DigitalProductPreview>> = (|| {
        require_digital_public(&state, &req)?;
        let digital = digital_product_for_read(&state, &req, path.as_str())?;
        let params = query.into_inner();
        let show_all = digital_can_manage(&state, &req);
        let mut items: Vec<DigitalProductPreview> = state
            .digital_previews
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|preview| {
                preview.digital_product_id == digital.id
                    && (show_all || preview.allowed)
                    && matches_search(
                        &format!(
                            "{} {}",
                            preview.format,
                            preview.label.clone().unwrap_or_default()
                        ),
                        &params,
                    )
                    && params.filters.get("format").is_none_or(|format| {
                        preview.format == *format
                    })
                    && params.filters.get("allowed").is_none_or(|flag| {
                        preview.allowed == (*flag == "true" || *flag == "1")
                    })
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn create_digital_preview(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateDigitalPreview>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalProductPreview> = (|| {
        require_digital_manage(&state, &req)?;
        let digital = load_digital_product(&state, path.as_str())?;
        let input = body.into_inner();
        let storage_file_id = input.storage_file_id.trim().to_string();
        if storage_file_id.is_empty() || storage_file_id.len() > 512 {
            return Err(UnifiedError::bad_request(
                "Storage file id must not be empty and must be at most 512 characters",
            ));
        }
        let format = input.format.trim().to_lowercase();
        if !valid_format(&format) {
            return Err(UnifiedError::bad_request(
                "Format must be a lowercase alphanumeric token such as pdf or epub",
            ));
        }
        let preview = DigitalProductPreview {
            id: new_id(),
            digital_product_id: digital.id.clone(),
            storage_file_id,
            format,
            label: input.label.filter(|value| !value.trim().is_empty()),
            allowed: input.allowed.unwrap_or(false),
            created_at: now(),
            updated_at: now(),
        };
        state
            .digital_previews
            .insert(preview.id.clone(), preview.clone());
        Ok(preview)
    })();
    respond_created(result)
}

async fn delete_digital_preview(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        require_digital_manage(&state, &req)?;
        let id = path.as_str().to_string();
        if state
            .digital_downloads
            .iter()
            .any(|entry| entry.value().preview_id.as_deref() == Some(id.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Preview has download history; it cannot be deleted",
            ));
        }
        state
            .digital_previews
            .remove(&id)
            .map(|_| json!({ "deleted": true, "id": id }))
            .ok_or_else(|| UnifiedError::not_found("Digital preview not found"))
    })();
    json::respond(result, false)
}

#[derive(Debug, Clone, Deserialize)]
pub struct SampleQuery {
    pub preview_id: Option<String>,
}

/// Download the free sample of a digital product. Public by design
/// when the seller allows samples, and rate-limited per identity and
/// preview plus logged in the download history, so a free excerpt
/// cannot be harvested without a trace. Samples are excerpts, not
/// the purchased file, so they carry no buyer watermark.
async fn download_sample(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<SampleQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        require_digital_public(&state, &req)?;
        let digital = digital_product_for_read(&state, &req, path.as_str())?;
        if !digital.sample_allowed {
            return Err(UnifiedError::forbidden(
                "This digital product offers no free sample",
            ));
        }
        let preview = match &query.into_inner().preview_id {
            Some(preview_id) => {
                let preview = state
                    .digital_previews
                    .get(preview_id)
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| UnifiedError::not_found("Digital preview not found"))?;
                if preview.digital_product_id != digital.id {
                    return Err(UnifiedError::not_found("Digital preview not found"));
                }
                if !preview.allowed {
                    return Err(UnifiedError::forbidden(
                        "This preview is not available for public download",
                    ));
                }
                preview
            }
            None => {
                let mut allowed: Vec<DigitalProductPreview> = state
                    .digital_previews
                    .iter()
                    .filter(|entry| {
                        entry.value().digital_product_id == digital.id
                            && entry.value().allowed
                    })
                    .map(|entry| entry.value().clone())
                    .collect();
                allowed.sort_by(|a, b| a.created_at.cmp(&b.created_at));
                match allowed.len() {
                    0 => {
                        return Err(UnifiedError::not_found(
                            "This digital product has no public sample",
                        ));
                    }
                    1 => allowed.into_iter().next().expect("one allowed preview"),
                    _ => {
                        return Err(UnifiedError::bad_request(
                            "This product has several samples; pass preview_id to choose one",
                        ));
                    }
                }
            }
        };
        let identity = owner_of(&req);
        check_sample_rate_limit(&state, &identity, &preview.id)?;
        let record = DigitalDownload {
            id: new_id(),
            digital_product_id: digital.id.clone(),
            file_id: None,
            preview_id: Some(preview.id.clone()),
            buyer_user_id: identity.clone(),
            order_id: None,
            grant_id: None,
            kind: "sample".to_string(),
            watermark_subject: None,
            watermark_applied: false,
            watermark_formats: Vec::new(),
            downloaded_at: now(),
        };
        state
            .digital_downloads
            .insert(record.id.clone(), record.clone());
        state.emit(
            "store.digital.sample.downloaded",
            serde_json::to_value(&record).unwrap_or(Value::Null),
        );
        Ok(json!({
            "sample": {
                "digital_product_id": digital.id,
                "preview_id": preview.id,
                "storage_file_id": preview.storage_file_id,
                "format": preview.format,
                "status": "ready",
                "download_id": record.id,
            }
        }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Digital products: downloads, royalties, payouts, settings
// ---------------------------------------------------------------------------

/// Create a signed expiring download link for one file of a digital
/// product the caller bought. This reuses the existing signed-link
/// flow end to end: the caller must hold a paid or completed order
/// containing the product, the entitlement (minted at payment, or
/// lazily here when the order was paid before the digital profile
/// existed) enforces the per-file download limit, and the actual
/// download is counted by `signed_download`, exactly like a software
/// release. The response also carries the watermark descriptor for
/// the file, so the buyer sees what will be embedded.
async fn create_digital_download_link(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: Option<web::Json<DigitalDownloadLinkRequest>>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = require_digital_read(&state, &req)?;
        let digital = load_digital_product(&state, path.as_str())?;
        let order = state
            .orders
            .iter()
            .filter(|entry| {
                let order = entry.value();
                order.user_id == caller.actor
                    && matches!(order.status.as_str(), "paid" | "completed")
                    && order
                        .items
                        .iter()
                        .any(|item| item.product_id == digital.product_id)
            })
            .map(|entry| entry.value().clone())
            .max_by(|a, b| a.created_at.cmp(&b.created_at))
            .ok_or_else(|| {
                UnifiedError::forbidden(
                    "No paid order for this digital product belongs to the caller",
                )
            })?;
        let requested_file_id = body.and_then(|body| body.into_inner().file_id);
        let files: Vec<DigitalProductFile> = state
            .digital_files
            .iter()
            .filter(|entry| entry.value().digital_product_id == digital.id)
            .map(|entry| entry.value().clone())
            .collect();
        let file = match &requested_file_id {
            Some(file_id) => files
                .iter()
                .find(|file| file.id == *file_id)
                .ok_or_else(|| UnifiedError::not_found("Digital file not found"))?,
            None => files
                .iter()
                .find(|file| file.is_primary)
                .or_else(|| {
                    files.iter().min_by(|a, b| {
                        a.sort_order
                            .cmp(&b.sort_order)
                            .then(a.created_at.cmp(&b.created_at))
                    })
                })
                .ok_or_else(|| {
                    UnifiedError::unprocessable(
                        "This digital product has no deliverable file",
                    )
                })?,
        };
        let (entitlement, _) = ensure_digital_entitlement(&state, &order, &digital, file);
        if entitlement.status == "revoked" {
            return Err(UnifiedError::forbidden(
                "Download entitlement has been revoked",
            ));
        }
        if let Some(expires_at) = entitlement.expires_at {
            if expires_at <= now() {
                return Err(UnifiedError::forbidden(
                    "Download entitlement has expired",
                ));
            }
        }
        if entitlement.downloads_used >= entitlement.download_limit {
            return Err(UnifiedError::forbidden(
                "Download limit reached for this purchase",
            ));
        }
        let license_id = entitlement.license_id.clone().ok_or_else(|| {
            UnifiedError::unprocessable("Entitlement has no purchase grant attached")
        })?;
        let (download_url, expires_at) = build_signed_url(&license_id, &file.id);
        let watermark =
            watermark_descriptor(&state, &file.format, &caller.actor, &order.id);
        Ok(json!({
            "download_url": download_url,
            "digital_product_id": digital.id,
            "file_id": file.id,
            "storage_file_id": file.storage_file_id,
            "format": file.format,
            "grant_id": license_id,
            "expires_at": expires_at.to_rfc3339(),
            "downloads_used": entitlement.downloads_used,
            "download_limit": entitlement.download_limit,
            "watermark": watermark,
        }))
    })();
    json::respond(result, false)
}

async fn list_digital_downloads(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<DigitalDownload>> = (|| {
        let caller = require_digital_read(&state, &req)?;
        let params = query.into_inner();
        let user_id = params
            .filters
            .get("user_id")
            .cloned()
            .unwrap_or_else(|| caller.actor.clone());
        if user_id != caller.actor && !digital_can_manage(&state, &req) {
            return Err(UnifiedError::forbidden(
                "Download history of another user requires the manage permission",
            ));
        }
        let mut items: Vec<DigitalDownload> = state
            .digital_downloads
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|download| {
                download.buyer_user_id == user_id
                    && params.filters.get("product_id").is_none_or(|product_id| {
                        download.digital_product_id == *product_id
                    })
                    && params
                        .filters
                        .get("kind")
                        .is_none_or(|kind| download.kind == *kind)
                    && params.filters.get("file_id").is_none_or(|file_id| {
                        download.file_id.as_deref() == Some(file_id.as_str())
                    })
            })
            .collect();
        items.sort_by(|a, b| a.downloaded_at.cmp(&b.downloaded_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn list_digital_royalties(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<DigitalRoyalty>> = (|| {
        let caller = require_digital_read(&state, &req)?;
        let params = query.into_inner();
        let payee = params
            .filters
            .get("payee")
            .cloned()
            .unwrap_or_else(|| caller.actor.clone());
        if payee != caller.actor && !digital_can_manage(&state, &req) {
            return Err(UnifiedError::forbidden(
                "Royalty history of another payee requires the manage permission",
            ));
        }
        let mut items: Vec<DigitalRoyalty> = state
            .digital_royalties
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|royalty| {
                royalty.payee.as_deref() == Some(payee.as_str())
                    && params.filters.get("status").is_none_or(|status| {
                        royalty.status == *status
                    })
                    && params.filters.get("product_id").is_none_or(|product_id| {
                        royalty.product_id == *product_id
                    })
                    && params.filters.get("order_id").is_none_or(|order_id| {
                        royalty.order_id == *order_id
                    })
                    && params.filters.get("currency").is_none_or(|currency| {
                        royalty.currency.eq_ignore_ascii_case(currency)
                    })
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn get_digital_royalty(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalRoyalty> = (|| {
        let caller = require_digital_read(&state, &req)?;
        let royalty = state
            .digital_royalties
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Royalty not found"))?;
        if royalty.payee.as_deref() != Some(caller.actor.as_str())
            && !digital_can_manage(&state, &req)
        {
            return Err(UnifiedError::forbidden(
                "Royalty belongs to a different payee",
            ));
        }
        Ok(royalty)
    })();
    json::respond(result, false)
}

async fn create_digital_payout(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<CreateDigitalPayout>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalPayoutRequest> = (|| {
        let caller = require_digital_read(&state, &req)?;
        let input = body.into_inner();
        if input.royalty_ids.is_empty() {
            return Err(UnifiedError::bad_request(
                "At least one royalty id is required",
            ));
        }
        if input.royalty_ids.len() > 100 {
            return Err(UnifiedError::bad_request(
                "At most 100 royalties can be grouped into one payout request",
            ));
        }
        let mut royalties: Vec<DigitalRoyalty> = Vec::new();
        for royalty_id in &input.royalty_ids {
            let royalty = state
                .digital_royalties
                .get(royalty_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!("Royalty {royalty_id} not found"))
                })?;
            if royalty.status != "accrued" {
                return Err(UnifiedError::unprocessable(format!(
                    "Royalty {} is {} and cannot be paid out",
                    royalty.id, royalty.status
                )));
            }
            royalties.push(royalty);
        }
        let first = royalties.first().expect("royalties is not empty");
        let payee = first.payee.clone();
        let payee_company_id = first.payee_company_id.clone();
        let currency = first.currency.clone();
        if payee.is_none() && payee_company_id.is_none() {
            return Err(UnifiedError::unprocessable(
                "Royalty has no payee, so no payout can be requested for it",
            ));
        }
        for royalty in &royalties {
            if royalty.payee != payee
                || royalty.payee_company_id != payee_company_id
                || royalty.currency != currency
            {
                return Err(UnifiedError::unprocessable(
                    "All royalties in one payout request must share the same payee and currency",
                ));
            }
        }
        // A payee requests their own payout; anyone else needs the
        // manage permission (an administrator requesting on a
        // payee's behalf).
        if payee.as_deref() != Some(caller.actor.as_str())
            && !digital_can_manage(&state, &req)
        {
            return Err(UnifiedError::forbidden(
                "Payout can only be requested by its payee",
            ));
        }
        let amount = round_money(royalties.iter().map(|royalty| royalty.amount).sum());
        let payout = DigitalPayoutRequest {
            id: new_id(),
            payee: payee.clone(),
            payee_company_id: payee_company_id.clone(),
            currency,
            amount,
            royalty_ids: input.royalty_ids.clone(),
            status: "requested".to_string(),
            requested_by: caller.actor.clone(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .digital_payouts
            .insert(payout.id.clone(), payout.clone());
        for royalty_id in &input.royalty_ids {
            if let Some(mut royalty) = state.digital_royalties.get_mut(royalty_id) {
                royalty.status = "payout_requested".to_string();
                royalty.payout_id = Some(payout.id.clone());
                royalty.updated_at = now();
            }
        }
        // Events only: billing owns the payout itself. The payout
        // request carries everything billing needs to pay it.
        state.emit(
            "store.digital.payout.requested",
            serde_json::to_value(&payout).unwrap_or(Value::Null),
        );
        Ok(payout)
    })();
    respond_created(result)
}

async fn list_digital_payouts(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<DigitalPayoutRequest>> = (|| {
        let caller = require_digital_read(&state, &req)?;
        let params = query.into_inner();
        let payee = params
            .filters
            .get("payee")
            .cloned()
            .unwrap_or_else(|| caller.actor.clone());
        if payee != caller.actor && !digital_can_manage(&state, &req) {
            return Err(UnifiedError::forbidden(
                "Payout history of another payee requires the manage permission",
            ));
        }
        let mut items: Vec<DigitalPayoutRequest> = state
            .digital_payouts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|payout| {
                payout.payee.as_deref() == Some(payee.as_str())
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| payout.status == *status)
                    && params.filters.get("currency").is_none_or(|currency| {
                        payout.currency.eq_ignore_ascii_case(currency)
                    })
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn get_digital_payout(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalPayoutRequest> = (|| {
        let caller = require_digital_read(&state, &req)?;
        let payout = state
            .digital_payouts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Payout request not found"))?;
        if payout.payee.as_deref() != Some(caller.actor.as_str())
            && !digital_can_manage(&state, &req)
        {
            return Err(UnifiedError::forbidden(
                "Payout request belongs to a different payee",
            ));
        }
        Ok(payout)
    })();
    json::respond(result, false)
}

/// Shared payout transition (approve / mark-paid / reject). The
/// payout moves only from the listed states; the grouped royalties
/// move with it, and a rejection releases them back to `accrued`
/// with the payout link cleared so they can be requested again.
fn move_digital_payout(
    state: &StoreState,
    payout_id: &str,
    allowed_from: &[&str],
    to_status: &str,
    royalty_status: &str,
    event: &str,
) -> UnifiedResult<DigitalPayoutRequest> {
    let mut payout = state
        .digital_payouts
        .get_mut(payout_id)
        .ok_or_else(|| UnifiedError::not_found("Payout request not found"))?;
    if !allowed_from.contains(&payout.status.as_str()) {
        return Err(UnifiedError::unprocessable(format!(
            "A {} payout request cannot move to {}",
            payout.status, to_status
        )));
    }
    payout.status = to_status.to_string();
    payout.updated_at = now();
    let updated = payout.clone();
    drop(payout);
    for royalty_id in &updated.royalty_ids {
        if let Some(mut royalty) = state.digital_royalties.get_mut(royalty_id) {
            royalty.status = royalty_status.to_string();
            if royalty_status == "accrued" {
                royalty.payout_id = None;
            }
            royalty.updated_at = now();
        }
    }
    state.emit(event, serde_json::to_value(&updated).unwrap_or(Value::Null));
    Ok(updated)
}

async fn approve_digital_payout(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalPayoutRequest> = (|| {
        require_digital_manage(&state, &req)?;
        move_digital_payout(
            &state,
            path.as_str(),
            &["requested"],
            "approved",
            "approved",
            "store.digital.payout.approved",
        )
    })();
    json::respond(result, false)
}

async fn mark_digital_payout_paid(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalPayoutRequest> = (|| {
        require_digital_manage(&state, &req)?;
        // Records that billing moved the money; it never moves money
        // itself. Only an approved payout can be marked paid.
        move_digital_payout(
            &state,
            path.as_str(),
            &["approved"],
            "paid",
            "paid",
            "store.digital.payout.paid",
        )
    })();
    json::respond(result, false)
}

async fn reject_digital_payout(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalPayoutRequest> = (|| {
        require_digital_manage(&state, &req)?;
        move_digital_payout(
            &state,
            path.as_str(),
            &["requested", "approved"],
            "rejected",
            "accrued",
            "store.digital.payout.rejected",
        )
    })();
    json::respond(result, false)
}

async fn get_digital_settings(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        require_digital_read(&state, &req)?;
        let defaults = state
            .digital_defaults
            .read()
            .map(|defaults| defaults.clone())
            .unwrap_or_default();
        let formats: Vec<WatermarkFormat> = state
            .watermark_formats
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        Ok(json!({
            "defaults": defaults,
            "type_count": state.digital_types.len(),
            "watermark_formats": formats,
        }))
    })();
    json::respond(result, false)
}

async fn update_digital_settings(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<UpdateDigitalSettings>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalDefaults> = (|| {
        require_digital_manage(&state, &req)?;
        let input = body.into_inner();
        if let Some(kind) = &input.default_royalty_kind {
            let value = input.default_royalty_value.unwrap_or_else(|| {
                state
                    .digital_defaults
                    .read()
                    .map(|defaults| defaults.default_royalty_value)
                    .unwrap_or(DEFAULT_ROYALTY_VALUE)
            });
            validate_royalty(kind, value)?;
        } else if let Some(value) = input.default_royalty_value {
            let kind = state
                .digital_defaults
                .read()
                .map(|defaults| defaults.default_royalty_kind.clone())
                .unwrap_or_else(|| DEFAULT_ROYALTY_KIND.to_string());
            validate_royalty(&kind, value)?;
        }
        let mut defaults = state
            .digital_defaults
            .write()
            .expect("digital defaults lock poisoned");
        if let Some(limit) = input.default_download_limit {
            if limit == 0 || limit > 10_000 {
                return Err(UnifiedError::bad_request(
                    "Default download limit must be between 1 and 10000",
                ));
            }
            defaults.default_download_limit = limit;
        }
        if let Some(limit) = input.sample_rate_limit_per_hour {
            if limit == 0 || limit > 1_000 {
                return Err(UnifiedError::bad_request(
                    "Sample rate limit must be between 1 and 1000 per hour",
                ));
            }
            defaults.sample_rate_limit_per_hour = limit;
        }
        if let Some(window) = input.sample_rate_limit_window_minutes {
            if window == 0 || window > 1_440 {
                return Err(UnifiedError::bad_request(
                    "Sample rate limit window must be between 1 and 1440 minutes",
                ));
            }
            defaults.sample_rate_limit_window_minutes = window;
        }
        if let Some(kind) = input.default_royalty_kind {
            defaults.default_royalty_kind = kind;
        }
        if let Some(value) = input.default_royalty_value {
            defaults.default_royalty_value = value;
        }
        if let Some(enabled) = input.watermark_enabled {
            defaults.watermark_enabled = enabled;
        }
        Ok(defaults.clone())
    })();
    json::respond(result, false)
}

async fn list_watermark_formats(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<WatermarkFormat>> = (|| {
        require_digital_read(&state, &req)?;
        let params = query.into_inner();
        let mut items: Vec<WatermarkFormat> = state
            .watermark_formats
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|format| {
                matches_search(&format!("{} {}", format.format, format.notes), &params)
                    && params.filters.get("metadata_supported").is_none_or(|flag| {
                        format.metadata_supported == (*flag == "true" || *flag == "1")
                    })
            })
            .collect();
        items.sort_by(|a, b| a.format.cmp(&b.format));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn update_watermark_format(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateWatermarkFormat>,
) -> HttpResponse {
    let result: UnifiedResult<WatermarkFormat> = (|| {
        require_digital_manage(&state, &req)?;
        let input = body.into_inner();
        if let Some(fields) = &input.metadata_fields {
            if fields.len() > 16 {
                return Err(UnifiedError::bad_request(
                    "At most 16 metadata fields can be listed for a format",
                ));
            }
            for field in fields {
                if field.trim().is_empty() || field.len() > 64 {
                    return Err(UnifiedError::bad_request(
                        "Metadata field names must not be empty and must be at most 64 characters",
                    ));
                }
            }
        }
        if let Some(notes) = &input.notes {
            if notes.len() > 500 {
                return Err(UnifiedError::bad_request(
                    "Format notes must be at most 500 characters",
                ));
            }
        }
        let mut format = state
            .watermark_formats
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Watermark format not found"))?;
        if let Some(metadata_supported) = input.metadata_supported {
            format.metadata_supported = metadata_supported;
        }
        if let Some(metadata_fields) = input.metadata_fields {
            format.metadata_fields = metadata_fields;
        }
        if let Some(notes) = input.notes {
            format.notes = notes;
        }
        format.updated_at = now();
        Ok(format.clone())
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Vendor / company payout linkage
// ---------------------------------------------------------------------------

async fn vendor_payout_summary(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let company_id = path.into_inner();
    let mut by_currency: HashMap<String, Value> = HashMap::new();
    let mut order_count: u64 = 0;
    for entry in state.orders.iter() {
        let order = entry.value();
        if order.company_id.as_deref() != Some(company_id.as_str()) {
            continue;
        }
        order_count += 1;
        let totals = by_currency
            .entry(order.currency.clone())
            .or_insert_with(|| {
                json!({
                    "currency": order.currency,
                    "orders": 0,
                    "gross": 0.0,
                    "paid": 0.0,
                    "refunded": 0.0,
                })
            });
        let totals = totals.as_object_mut().expect("totals object");
        totals["orders"] = json!(totals["orders"].as_u64().unwrap_or(0) + 1);
        let gross = totals["gross"].as_f64().unwrap_or(0.0) + order.total;
        totals["gross"] = json!(round_money(gross));
        match order.status.as_str() {
            "paid" | "completed" => {
                let paid = totals["paid"].as_f64().unwrap_or(0.0) + order.total;
                totals["paid"] = json!(round_money(paid));
            }
            "refunded" => {
                let refunded = totals["refunded"].as_f64().unwrap_or(0.0) + order.total;
                totals["refunded"] = json!(round_money(refunded));
            }
            _ => {}
        }
    }
    let mut totals: Vec<Value> = by_currency.into_values().collect();
    totals.sort_by(|a, b| {
        a.get("currency")
            .and_then(|value| value.as_str())
            .cmp(&b.get("currency").and_then(|value| value.as_str()))
    });
    json::success_response(
        json!({
            "company_id": company_id,
            "orders": order_count,
            "totals": totals,
        }),
        200,
    )
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct StoreModule {
    state: Arc<StoreState>,
}

impl StoreModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(StoreState::new()),
        }
    }
}

impl Default for StoreModule {
    fn default() -> Self {
        Self::new()
    }
}

/// Digital-products selling is declared sellable for the product
/// registry through `DIGITAL_PRODUCTS_AVAILABLE_AS_PRODUCT` (category
/// `commerce`, see the constants near the digital types): the
/// integrator flags that capability, not this module. This module
/// deliberately does NOT override `available_as_product()` /
/// `product_category()`: the store is the marketplace itself, not a
/// sellable module, so listing "Store" as a buyable product
/// offering would be wrong.
impl Module for StoreModule {
    fn id(&self) -> &'static str {
        "store"
    }

    fn name(&self) -> &'static str {
        "Store"
    }

    fn description(&self) -> &'static str {
        "Store for selling software: products with pricing tiers, coupons, cart, checkout, orders, refunds, signed downloads, entitlements, update checks, and subscription checkout with plan changes and cancellation. Digital products (ebooks, audiobooks, templates, course packs, digital assets) sell through the same checkout: paid orders mint per-file download entitlements served by the signed download flow, accrue per-sale royalties for authors and vendors (payout requests are events for billing), offer rate-limited public samples, and watermark delivered files at the metadata level (never claimed as unbreakable DRM)."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["licensing", "billing"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let _ = STORE_CONFIG.set(ctx.config.clone());
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
            "software.release.published",
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_software_release_published(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            PAYMENT_CAPTURED,
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_payment_captured(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            PAYMENT_REFUNDED,
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_payment_refunded(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            BANK_TRANSFER_VERIFICATION_CREATED,
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_bank_transfer_verification_created(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            BANK_TRANSFER_REJECTED,
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_bank_transfer_rejected(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            PRODUCT_LISTED,
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_product_listed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            PRODUCT_DELISTED,
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_product_delisted(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STORE_ENTITLEMENT_CHECK,
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_entitlement_check(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "subscription.lifecycle.apply",
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_subscription_lifecycle_apply(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "subscription.module_entitlement.check",
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_subscription_module_entitlement_check(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "properties.block.purchase_requested",
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_properties_purchase_requested(state.clone(), ctx)
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/store/products", owner: "store", description: "List products" },
            RouteInfo { method: "POST", path: "/api/v1/store/products", owner: "store", description: "Create product" },
            RouteInfo { method: "GET", path: "/api/v1/store/products/{id}", owner: "store", description: "Get product" },
            RouteInfo { method: "PUT", path: "/api/v1/store/products/{id}", owner: "store", description: "Update product" },
            RouteInfo { method: "DELETE", path: "/api/v1/store/products/{id}", owner: "store", description: "Delete product" },
            RouteInfo { method: "POST", path: "/api/v1/store/products/{id}/publish", owner: "store", description: "Publish product" },
            RouteInfo { method: "POST", path: "/api/v1/store/products/{id}/unpublish", owner: "store", description: "Unpublish product" },
            RouteInfo { method: "GET", path: "/api/v1/store/products/{id}/tiers", owner: "store", description: "List tiers" },
            RouteInfo { method: "POST", path: "/api/v1/store/products/{id}/tiers", owner: "store", description: "Create tier" },
            RouteInfo { method: "GET", path: "/api/v1/store/tiers/{id}", owner: "store", description: "Get tier" },
            RouteInfo { method: "PUT", path: "/api/v1/store/tiers/{id}", owner: "store", description: "Update tier" },
            RouteInfo { method: "DELETE", path: "/api/v1/store/tiers/{id}", owner: "store", description: "Delete tier" },
            RouteInfo { method: "GET", path: "/api/v1/store/coupons", owner: "store", description: "List coupons" },
            RouteInfo { method: "POST", path: "/api/v1/store/coupons", owner: "store", description: "Create coupon" },
            RouteInfo { method: "GET", path: "/api/v1/store/coupons/{id}", owner: "store", description: "Get coupon" },
            RouteInfo { method: "PUT", path: "/api/v1/store/coupons/{id}", owner: "store", description: "Update coupon" },
            RouteInfo { method: "DELETE", path: "/api/v1/store/coupons/{id}", owner: "store", description: "Delete coupon" },
            RouteInfo { method: "POST", path: "/api/v1/store/price-calculate", owner: "store", description: "Calculate the price of a product (optionally a tier and a coupon) without creating an order." },
            RouteInfo { method: "GET", path: "/api/v1/store/cart", owner: "store", description: "Get cart" },
            RouteInfo { method: "POST", path: "/api/v1/store/cart", owner: "store", description: "Add to cart" },
            RouteInfo { method: "DELETE", path: "/api/v1/store/cart", owner: "store", description: "Remove from cart" },
            RouteInfo { method: "POST", path: "/api/v1/store/checkout", owner: "store", description: "Checkout" },
            RouteInfo { method: "GET", path: "/api/v1/store/orders", owner: "store", description: "List orders" },
            RouteInfo { method: "GET", path: "/api/v1/store/orders/{id}", owner: "store", description: "Get order" },
            RouteInfo { method: "PUT", path: "/api/v1/store/orders/{id}", owner: "store", description: "Update order" },
            RouteInfo { method: "DELETE", path: "/api/v1/store/orders/{id}", owner: "store", description: "Delete order" },
            RouteInfo { method: "POST", path: "/api/v1/store/orders/{id}/pay", owner: "store", description: "Pay order" },
            RouteInfo { method: "GET", path: "/api/v1/store/reference-settings", owner: "store", description: "Read payment reference settings (default prefix, per-product prefixes)." },
            RouteInfo { method: "PUT", path: "/api/v1/store/reference-settings", owner: "store", description: "Update payment reference settings (capability store.reference:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/store/orders/{id}/complete", owner: "store", description: "Complete order" },
            RouteInfo { method: "POST", path: "/api/v1/store/orders/{id}/cancel", owner: "store", description: "Cancel order" },
            RouteInfo { method: "POST", path: "/api/v1/store/orders/{id}/refund", owner: "store", description: "Refund order" },
            RouteInfo { method: "GET", path: "/api/v1/store/orders/{id}/download", owner: "store", description: "Download order" },
            RouteInfo { method: "GET", path: "/api/v1/store/entitlements", owner: "store", description: "List entitlements" },
            RouteInfo { method: "GET", path: "/api/v1/store/entitlements/{id}", owner: "store", description: "Get entitlement" },
            RouteInfo { method: "GET", path: "/api/v1/store/subscriptions", owner: "store", description: "List subscriptions with pagination and filters." },
            RouteInfo { method: "GET", path: "/api/v1/store/subscriptions/{id}", owner: "store", description: "Get one subscription." },
            RouteInfo { method: "POST", path: "/api/v1/store/subscriptions/{id}/change-plan", owner: "store", description: "Change a subscription plan mid-cycle, applying the product proration policy." },
            RouteInfo { method: "POST", path: "/api/v1/store/subscriptions/{id}/cancel", owner: "store", description: "Cancel a subscription immediately or at the end of the current period." },
            RouteInfo { method: "POST", path: "/api/v1/store/entitlements/{id}/download-link", owner: "store", description: "Create download link" },
            RouteInfo { method: "GET", path: "/api/v1/store/downloads/signed", owner: "store", description: "Signed download" },
            RouteInfo { method: "GET", path: "/api/v1/store/licenses/{license_id}/downloads", owner: "store", description: "List license downloads" },
            RouteInfo { method: "GET", path: "/api/v1/store/updates/catalog", owner: "store", description: "List catalog" },
            RouteInfo { method: "POST", path: "/api/v1/store/updates/catalog", owner: "store", description: "Create catalog entry" },
            RouteInfo { method: "GET", path: "/api/v1/store/updates/catalog/{slug}", owner: "store", description: "Get catalog entry" },
            RouteInfo { method: "PUT", path: "/api/v1/store/updates/catalog/{slug}", owner: "store", description: "Update catalog entry" },
            RouteInfo { method: "DELETE", path: "/api/v1/store/updates/catalog/{slug}", owner: "store", description: "Delete catalog entry" },
            RouteInfo { method: "POST", path: "/api/v1/store/updates/check", owner: "store", description: "WordPress-style version check: given a slug and the installed version, return the latest version, whether an update is available, compatibility metadata,..." },
            RouteInfo { method: "GET", path: "/api/v1/store/updates/{slug}/package", owner: "store", description: "Download the latest package for a slug." },
            RouteInfo { method: "GET", path: "/api/v1/store/digital/types", owner: "store", description: "List digital product types (read; admin-extensible registry)." },
            RouteInfo { method: "POST", path: "/api/v1/store/digital/types", owner: "store", description: "Create digital product type (manage)." },
            RouteInfo { method: "PUT", path: "/api/v1/store/digital/types/{type_id}", owner: "store", description: "Update digital product type (manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/store/digital/types/{type_id}", owner: "store", description: "Delete digital product type (manage; refused while in use)." },
            RouteInfo { method: "GET", path: "/api/v1/store/digital/downloads", owner: "store", description: "List per-buyer digital download history (read; own history, or any with manage)." },
            RouteInfo { method: "GET", path: "/api/v1/store/digital/royalties", owner: "store", description: "List digital royalty ledger (read; own royalties, or all with manage)." },
            RouteInfo { method: "GET", path: "/api/v1/store/digital/royalties/{id}", owner: "store", description: "Get one digital royalty (read; own royalty, or any with manage)." },
            RouteInfo { method: "GET", path: "/api/v1/store/digital/payouts", owner: "store", description: "List digital payout requests (read; own requests, or all with manage)." },
            RouteInfo { method: "POST", path: "/api/v1/store/digital/payouts", owner: "store", description: "Request a royalty payout (read; emits store.digital.payout.requested for billing)." },
            RouteInfo { method: "GET", path: "/api/v1/store/digital/payouts/{id}", owner: "store", description: "Get one digital payout request (read; own request, or any with manage)." },
            RouteInfo { method: "POST", path: "/api/v1/store/digital/payouts/{id}/approve", owner: "store", description: "Approve a digital payout request (manage)." },
            RouteInfo { method: "POST", path: "/api/v1/store/digital/payouts/{id}/mark-paid", owner: "store", description: "Mark a digital payout paid after billing moved the money (manage)." },
            RouteInfo { method: "POST", path: "/api/v1/store/digital/payouts/{id}/reject", owner: "store", description: "Reject a digital payout request and release its royalties (manage)." },
            RouteInfo { method: "GET", path: "/api/v1/store/digital/settings", owner: "store", description: "Get digital defaults and watermark formats (read)." },
            RouteInfo { method: "PUT", path: "/api/v1/store/digital/settings", owner: "store", description: "Update digital defaults (manage)." },
            RouteInfo { method: "GET", path: "/api/v1/store/digital/watermark-formats", owner: "store", description: "List watermark formats (read)." },
            RouteInfo { method: "PUT", path: "/api/v1/store/digital/watermark-formats/{format}", owner: "store", description: "Update a watermark format (manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/store/digital/files/{file_id}", owner: "store", description: "Delete a digital file (manage; refused with entitlements or history)." },
            RouteInfo { method: "DELETE", path: "/api/v1/store/digital/previews/{preview_id}", owner: "store", description: "Delete a digital preview (manage; refused with download history)." },
            RouteInfo { method: "GET", path: "/api/v1/store/digital", owner: "store", description: "List digital products (public storefront; published only)." },
            RouteInfo { method: "POST", path: "/api/v1/store/digital", owner: "store", description: "Create digital product (manage)." },
            RouteInfo { method: "GET", path: "/api/v1/store/digital/{id}", owner: "store", description: "Get digital product (public storefront; published only)." },
            RouteInfo { method: "PUT", path: "/api/v1/store/digital/{id}", owner: "store", description: "Update digital product (manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/store/digital/{id}", owner: "store", description: "Delete digital product (manage; refused with files, previews, or history)." },
            RouteInfo { method: "POST", path: "/api/v1/store/digital/{id}/publish", owner: "store", description: "Publish digital product (manage)." },
            RouteInfo { method: "POST", path: "/api/v1/store/digital/{id}/unpublish", owner: "store", description: "Unpublish digital product (manage)." },
            RouteInfo { method: "GET", path: "/api/v1/store/digital/{id}/files", owner: "store", description: "List files of a digital product (read)." },
            RouteInfo { method: "POST", path: "/api/v1/store/digital/{id}/files", owner: "store", description: "Add a file to a digital product (manage)." },
            RouteInfo { method: "GET", path: "/api/v1/store/digital/{id}/previews", owner: "store", description: "List previews of a digital product (public storefront; allowed only)." },
            RouteInfo { method: "POST", path: "/api/v1/store/digital/{id}/previews", owner: "store", description: "Add a preview to a digital product (manage)." },
            RouteInfo { method: "GET", path: "/api/v1/store/digital/{id}/sample", owner: "store", description: "Download the free sample of a digital product (public when allowed; rate-limited and logged)." },
            RouteInfo { method: "POST", path: "/api/v1/store/digital/{id}/download-link", owner: "store", description: "Create a signed expiring download link for a purchased digital file (read; paid order required)." },
            RouteInfo { method: "GET", path: "/api/v1/store/vendors/{company_id}/payout-summary", owner: "store", description: "Vendor payout summary" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/store")
                .route("/products", web::get().to(list_products))
                .route("/products", web::post().to(create_product))
                .route("/products/{id}", web::get().to(get_product))
                .route("/products/{id}", web::put().to(update_product))
                .route("/products/{id}", web::delete().to(delete_product))
                .route("/products/{id}/publish", web::post().to(publish_product))
                .route("/products/{id}/unpublish", web::post().to(unpublish_product))
                .route("/products/{id}/tiers", web::get().to(list_tiers))
                .route("/products/{id}/tiers", web::post().to(create_tier))
                .route("/tiers/{id}", web::get().to(get_tier))
                .route("/tiers/{id}", web::put().to(update_tier))
                .route("/tiers/{id}", web::delete().to(delete_tier))
                .route("/coupons", web::get().to(list_coupons))
                .route("/coupons", web::post().to(create_coupon))
                .route("/coupons/{id}", web::get().to(get_coupon))
                .route("/coupons/{id}", web::put().to(update_coupon))
                .route("/coupons/{id}", web::delete().to(delete_coupon))
                .route("/price-calculate", web::post().to(calculate_price))
                .route("/cart", web::get().to(get_cart))
                .route("/cart", web::post().to(add_to_cart))
                .route("/cart", web::delete().to(remove_from_cart))
                .route("/checkout", web::post().to(checkout))
                .route("/orders", web::get().to(list_orders))
                .route("/orders/{id}", web::get().to(get_order))
                .route("/orders/{id}", web::put().to(update_order))
                .route("/orders/{id}", web::delete().to(delete_order))
                .route("/orders/{id}/pay", web::post().to(pay_order))
                .route("/reference-settings", web::get().to(get_reference_settings))
                .route("/reference-settings", web::put().to(update_reference_settings))
                .route("/orders/{id}/complete", web::post().to(complete_order))
                .route("/orders/{id}/cancel", web::post().to(cancel_order))
                .route("/orders/{id}/refund", web::post().to(refund_order))
                .route("/orders/{id}/download", web::get().to(download_order))
                .route("/entitlements", web::get().to(list_entitlements))
                .route("/entitlements/{id}", web::get().to(get_entitlement))
                .route("/subscriptions", web::get().to(list_subscriptions))
                .route("/subscriptions/{id}", web::get().to(get_subscription))
                .route(
                    "/subscriptions/{id}/change-plan",
                    web::post().to(change_subscription_plan),
                )
                .route(
                    "/subscriptions/{id}/cancel",
                    web::post().to(cancel_subscription),
                )
                .route(
                    "/entitlements/{id}/download-link",
                    web::post().to(create_download_link),
                )
                .route("/downloads/signed", web::get().to(signed_download))
                .route(
                    "/licenses/{license_id}/downloads",
                    web::get().to(list_license_downloads),
                )
                .route("/updates/catalog", web::get().to(list_catalog))
                .route("/updates/catalog", web::post().to(create_catalog_entry))
                .route("/updates/catalog/{slug}", web::get().to(get_catalog_entry))
                .route(
                    "/updates/catalog/{slug}",
                    web::put().to(update_catalog_entry),
                )
                .route(
                    "/updates/catalog/{slug}",
                    web::delete().to(delete_catalog_entry),
                )
                .route("/updates/check", web::post().to(check_update))
                .route("/updates/{slug}/package", web::get().to(download_package))
                // Digital products: static paths first so they win
                // over `/digital/{id}` in every router.
                .route("/digital/types", web::get().to(list_digital_types))
                .route("/digital/types", web::post().to(create_digital_type))
                .route("/digital/types/{type_id}", web::put().to(update_digital_type))
                .route(
                    "/digital/types/{type_id}",
                    web::delete().to(delete_digital_type),
                )
                .route("/digital/downloads", web::get().to(list_digital_downloads))
                .route("/digital/royalties", web::get().to(list_digital_royalties))
                .route(
                    "/digital/royalties/{id}",
                    web::get().to(get_digital_royalty),
                )
                .route("/digital/payouts", web::get().to(list_digital_payouts))
                .route("/digital/payouts", web::post().to(create_digital_payout))
                .route("/digital/payouts/{id}", web::get().to(get_digital_payout))
                .route(
                    "/digital/payouts/{id}/approve",
                    web::post().to(approve_digital_payout),
                )
                .route(
                    "/digital/payouts/{id}/mark-paid",
                    web::post().to(mark_digital_payout_paid),
                )
                .route(
                    "/digital/payouts/{id}/reject",
                    web::post().to(reject_digital_payout),
                )
                .route("/digital/settings", web::get().to(get_digital_settings))
                .route(
                    "/digital/settings",
                    web::put().to(update_digital_settings),
                )
                .route(
                    "/digital/watermark-formats",
                    web::get().to(list_watermark_formats),
                )
                .route(
                    "/digital/watermark-formats/{format}",
                    web::put().to(update_watermark_format),
                )
                .route(
                    "/digital/files/{file_id}",
                    web::delete().to(delete_digital_file),
                )
                .route(
                    "/digital/previews/{preview_id}",
                    web::delete().to(delete_digital_preview),
                )
                .route("/digital", web::get().to(list_digital_products))
                .route("/digital", web::post().to(create_digital_product))
                .route("/digital/{id}", web::get().to(get_digital_product))
                .route("/digital/{id}", web::put().to(update_digital_product))
                .route("/digital/{id}", web::delete().to(delete_digital_product))
                .route(
                    "/digital/{id}/publish",
                    web::post().to(publish_digital_product),
                )
                .route(
                    "/digital/{id}/unpublish",
                    web::post().to(unpublish_digital_product),
                )
                .route("/digital/{id}/files", web::get().to(list_digital_files))
                .route("/digital/{id}/files", web::post().to(create_digital_file))
                .route(
                    "/digital/{id}/previews",
                    web::get().to(list_digital_previews),
                )
                .route(
                    "/digital/{id}/previews",
                    web::post().to(create_digital_preview),
                )
                .route("/digital/{id}/sample", web::get().to(download_sample))
                .route(
                    "/digital/{id}/download-link",
                    web::post().to(create_digital_download_link),
                )
                .route(
                    "/vendors/{company_id}/payout-summary",
                    web::get().to(vendor_payout_summary),
                ),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(StoreModule::new()));
}
