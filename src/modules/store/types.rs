// Store module - shared types and request/response shapes.
//
// Split from `src/modules/store.rs`; the module root re-exports the public API of this module.

use chrono::{DateTime, Utc};
use hmac::Hmac;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;

pub(crate) type HmacSha256 = Hmac<Sha256>;

// Types

pub(crate) fn default_product_status() -> String {
    "published".to_string()
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Product {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub price: i64,
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
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub renewal_price: Option<i64>,
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
    /// Integer minor units; accepted as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub price: i64,
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
    /// Integer minor units when present; accepted as major units in JSON.
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub renewal_price: Option<i64>,
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
    /// Integer minor units when present; accepted as major units in JSON.
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub price: Option<i64>,
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
    /// Integer minor units when present; accepted as major units in JSON.
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub renewal_price: Option<i64>,
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
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub price: i64,
    /// Integer minor units when present. Serialized as major units in JSON.
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub renewal_price: Option<i64>,
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
    /// Integer minor units; accepted as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub price: i64,
    /// Integer minor units when present; accepted as major units in JSON.
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub renewal_price: Option<i64>,
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
    /// Integer minor units when present; accepted as major units in JSON.
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub price: Option<i64>,
    /// Integer minor units when present; accepted as major units in JSON.
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub renewal_price: Option<i64>,
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
    /// Percent discount in basis points (1 bp = 0.01%, so 1000 is
    /// 10%). Only meaningful when `discount_type` is `percent`;
    /// always 0 for a fixed-amount coupon. Integer by design: the
    /// old dual-meaning `discount_value: f64` (percent or major-unit
    /// fixed amount in one float) is gone, so the percent branch
    /// computes in exact integer math and the fixed branch is
    /// already minor units.
    pub discount_percent_bp: i64,
    /// Fixed discount in integer minor units in the order currency.
    /// Only meaningful when `discount_type` is `fixed`; always 0
    /// for a percent coupon.
    pub discount_fixed_minor: i64,
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
    /// Percent discount in basis points; required for `percent`
    /// coupons, ignored (must be 0) for `fixed` coupons.
    #[serde(default)]
    pub discount_percent_bp: i64,
    /// Fixed discount in integer minor units; required for `fixed`
    /// coupons, ignored (must be 0) for `percent` coupons.
    #[serde(default)]
    pub discount_fixed_minor: i64,
    pub expires_at: Option<DateTime<Utc>>,
    pub usage_limit: Option<u32>,
    pub product_ids: Option<Vec<String>>,
    pub active: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateCoupon {
    pub discount_type: Option<String>,
    pub discount_percent_bp: Option<i64>,
    pub discount_fixed_minor: Option<i64>,
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
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub unit_price: i64,
    pub quantity: u32,
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub line_total: i64,
    #[serde(default)]
    pub tier_id: Option<String>,
    #[serde(default)]
    pub tier_name: Option<String>,
    /// Paid certification exam this line buys, when the cart line
    /// is an exam purchase. Carried onto the order item at
    /// checkout (see `CheckoutItem::exam_id`).
    #[serde(default)]
    pub exam_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cart {
    pub owner: String,
    pub items: Vec<CartItem>,
    pub currency: String,
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub total: i64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddCartItem {
    pub product_id: String,
    pub quantity: Option<u32>,
    pub tier_id: Option<String>,
    /// Paid certification exam this line buys (see
    /// `CheckoutItem::exam_id`).
    #[serde(default)]
    pub exam_id: Option<String>,
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
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub unit_price: i64,
    pub quantity: u32,
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub line_total: i64,

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
    /// Paid certification exam this line buys (see
    /// `CheckoutItem::exam_id`). Serialized on the order, so the
    /// `order.paid` / `store.order.completed` payloads carry it
    /// for the exams module's activation listener, which activates
    /// the exam for the buyer when the order is paid. Like
    /// `course_id`, this is an in-memory checkout stamp only: the
    /// database `store_order_items` table carries neither field
    /// yet, and the in-memory maps stay authoritative until the
    /// database layer is wired.
    #[serde(default)]
    pub exam_id: Option<String>,
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

pub(crate) fn default_seats() -> i32 {
    1
}

pub(crate) fn default_download_limit() -> u32 {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    /// Public payment reference for this order, in the single shared
    /// reference format (`PREFIX-12hex-12hex`, 30 characters; see
    /// `unified_core::reference_id`). Issued once at checkout and
    /// never changed afterwards: gateways reconcile against this
    /// value, while `id` stays the internal UUID primary key.
    /// `#[serde(default)]` keeps orders serialized before this field
    /// existed readable; such orders carry an empty reference until
    /// the integrator backfills them (see the batch report).
    #[serde(default)]
    pub reference: String,
    pub user_id: String,
    pub items: Vec<OrderItem>,
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub subtotal: i64,
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub discount_total: i64,
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub total: i64,

    pub currency: String,
    #[serde(default)]
    pub coupon_code: Option<String>,
    /// The discount evaluation that priced this order: the pricing
    /// module's answer (full trace, `evaluator: "pricing"`) when
    /// it answered, or the store-local fallback trace
    /// (`evaluator: "store-local"`) with the reason the shared
    /// evaluator could not price it. Exactly one evaluator's math
    /// is ever applied to an order; this field records which one.
    /// Store money is integer minor units; the pricing answer
    /// arrives as integer scaled units and is converted to minor
    /// units with half-up rounding at this boundary.
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
    /// `trialing`, `active`, `past_due`, `cancelled`, or `expired`
    /// (set by the expiry sweep once the access window closes).
    pub status: String,
    /// `weekly`, `monthly`, or `yearly`.
    pub billing_period: String,
    /// Price charged per period, in integer minor units.
    /// Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub price: i64,
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
    /// Integer minor units when present. Serialized as major units in JSON.
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub pending_price: Option<i64>,
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

pub(crate) fn default_true() -> bool {
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
    /// Paid certification exam this line buys, when the buyer is
    /// completing a paid exam purchase. The exam id is carried onto
    /// the order item and then onto the `order.paid` /
    /// `store.order.completed` payloads, which is what activates
    /// the exam for the buyer in the exams module. The store never
    /// invents this id: it comes from the exam purchase flow, and
    /// it must be a UUID.
    #[serde(default)]
    pub exam_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckoutRequest {
    pub items: Option<Vec<CheckoutItem>>,
    pub coupon_code: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PayRequest {
    pub provider: String,
    /// Admin-recorded payment reference for a manual (offline)
    /// payment: the bank receipt, cash receipt, or transfer id
    /// proving the money arrived. Required when `provider` is
    /// `manual`; ignored for gateway providers, whose own
    /// `payment.captured` event carries the reference.
    #[serde(default)]
    pub payment_reference: Option<String>,
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
    /// `pending` (created at checkout, before the order is paid),
    /// `active`, `expired`, or `revoked`. Only `active`
    /// entitlements download: a `pending` entitlement becomes
    /// `active` when its order is paid.
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
    /// `pending` (recorded at checkout, before the order is paid),
    /// `active`, or `suspended`.
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

// Digital products (ebooks and other downloadable digital goods)

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
pub(crate) const PERM_DIGITAL_READ: &str = "store.digital:read";
pub(crate) const PERM_DIGITAL_MANAGE: &str = "store.digital:manage";
/// Capability checked through the `role.check` hook for store
/// administration: recording a manual (offline) payment and moving
/// an order to `paid` directly. Held by store administrators only;
/// ordinary buyers never hold it, so they cannot mark their own
/// orders paid.
pub(crate) const PERM_STORE_MANAGE: &str = "store.manage";
/// Capability checked through the `role.check` hook for the store
/// catalog write routes (create, update, delete). The public
/// catalog reads and the WordPress update-check / download
/// endpoints stay unauthenticated by design: update clients call
/// them without an operator identity.
pub(crate) const PERM_STORE_CATALOG_MANAGE: &str = "store.catalog:manage";

/// Configurable defaults for digital products. These are seed values
/// only: an administrator can change every one of them through
/// `PUT /api/v1/store/digital/settings`, and the live values live in
/// `StoreState::digital_defaults`.
pub(crate) const DEFAULT_DIGITAL_DOWNLOAD_LIMIT: u32 = 10;
pub(crate) const DEFAULT_SAMPLE_RATE_LIMIT_PER_HOUR: u32 = 5;
pub(crate) const DEFAULT_SAMPLE_RATE_LIMIT_WINDOW_MINUTES: u32 = 60;
pub(crate) const DEFAULT_ROYALTY_KIND: &str = "percent";
/// No royalty accrues unless the seller configures one: the default
/// royalty is zero basis points on purpose, so money is never
/// promised to an author or vendor by accident.
pub(crate) const DEFAULT_ROYALTY_PERCENT_BP: i64 = 0;

pub(crate) fn default_royalty_kind() -> String {
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
    /// Reference to the cover image in the shared documents store
    /// - a documents document id, or a `/files/public/{key}` (or
    /// `/files/private/{key}`) URL from the media picker. Never
    /// file bytes, never a foreign URL.
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
    /// Percent royalty in basis points (1 bp = 0.01%, so 1000 is
    /// 10%). Only meaningful when `royalty_kind` is `percent`;
    /// always 0 for a fixed royalty. Integer by design: the old
    /// dual-meaning `royalty_value: f64` is gone, so the percent
    /// branch computes in exact integer math and the fixed branch
    /// is already minor units.
    #[serde(default)]
    pub royalty_percent_bp: i64,
    /// Fixed royalty per unit sold, in integer minor units in the
    /// order currency. Only meaningful when `royalty_kind` is
    /// `fixed`; always 0 for a percent royalty.
    #[serde(default)]
    pub royalty_fixed_minor: i64,
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
    pub royalty_percent_bp: Option<i64>,
    pub royalty_fixed_minor: Option<i64>,
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
    pub royalty_percent_bp: Option<i64>,
    pub royalty_fixed_minor: Option<i64>,
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
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
    pub currency: String,
    /// Royalty rule snapshot: `percent` or `fixed`.
    pub royalty_kind: String,
    /// Percent royalty in basis points at the time of the sale.
    pub royalty_percent_bp: i64,
    /// Fixed royalty per unit in integer minor units at the time
    /// of the sale.
    pub royalty_fixed_minor: i64,
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
    /// Integer minor units. Serialized as major units in JSON.
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
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
    /// Default percent royalty in basis points.
    pub default_royalty_percent_bp: i64,
    /// Default fixed royalty per unit in integer minor units.
    pub default_royalty_fixed_minor: i64,
    pub watermark_enabled: bool,
}

impl Default for DigitalDefaults {
    fn default() -> Self {
        Self {
            default_download_limit: DEFAULT_DIGITAL_DOWNLOAD_LIMIT,
            sample_rate_limit_per_hour: DEFAULT_SAMPLE_RATE_LIMIT_PER_HOUR,
            sample_rate_limit_window_minutes: DEFAULT_SAMPLE_RATE_LIMIT_WINDOW_MINUTES,
            default_royalty_kind: DEFAULT_ROYALTY_KIND.to_string(),
            default_royalty_percent_bp: DEFAULT_ROYALTY_PERCENT_BP,
            default_royalty_fixed_minor: 0,
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
    pub default_royalty_percent_bp: Option<i64>,
    pub default_royalty_fixed_minor: Option<i64>,
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
