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
// Interconnection happens only through the hook bus: this module never
// calls another module directly. License issuance is done by the
// licensing module through the `order.created` hook; this module reads
// the hook results back and records the download entitlement for each
// license that was issued.
//
// All data lives in memory in this module (no new tables are touched
// here); persistence of products, tiers, coupons, orders, entitlements
// and download records is a wiring step for the database layer.

use crate::core::config::AppConfig;
use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
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
    pub user_id: String,
    pub items: Vec<OrderItem>,
    pub subtotal: f64,
    pub discount_total: f64,
    pub total: f64,
    pub currency: String,
    #[serde(default)]
    pub coupon_code: Option<String>,
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

#[derive(Debug, Clone, Deserialize)]
pub struct CheckoutItem {
    pub product_id: String,
    pub quantity: Option<u32>,
    pub tier_id: Option<String>,
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
// State
// ---------------------------------------------------------------------------

pub struct StoreState {
    hooks: RwLock<Arc<HookBus>>,
    products: DashMap<String, Product>,
    tiers: DashMap<String, PricingTier>,
    coupons: DashMap<String, Coupon>,
    carts: DashMap<String, Cart>,
    orders: DashMap<String, Order>,
    entitlements: DashMap<String, DownloadEntitlement>,
    downloads: DashMap<String, DownloadRecord>,
    catalog: DashMap<String, CatalogEntry>,
    issued_licenses: DashMap<String, IssuedLicense>,
}

impl StoreState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            products: DashMap::new(),
            tiers: DashMap::new(),
            coupons: DashMap::new(),
            carts: DashMap::new(),
            orders: DashMap::new(),
            entitlements: DashMap::new(),
            downloads: DashMap::new(),
            catalog: DashMap::new(),
            issued_licenses: DashMap::new(),
        }
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
    matches!(period, "monthly" | "yearly")
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
            Some("monthly") => Some(current + chrono::Duration::days(30)),
            Some("yearly") => Some(current + chrono::Duration::days(365)),
            _ => Some(current + chrono::Duration::days(30)),
        },
        _ => Some(current + chrono::Duration::days(365 * updates_years.unwrap_or(1) as i64)),
    }
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
            state.emit("order.paid", order_value);
            ctx.add_result(json!({
                "store": "paid",
                "order_id": order_id,
                "payment_reference": reference,
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
            state.emit(
                "store.order.refunded",
                json!({
                    "order": serde_json::to_value(&updated).unwrap_or(Value::Null),
                    "order_id": updated.id,
                    "license_ids": license_ids,
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
            });
        }
        subtotal = round_money(subtotal);
        // A coupon discounts only the line items it covers; a coupon
        // that covers nothing in the order is rejected below.
        let mut discount_total = 0.0;
        let mut coupon_code: Option<String> = None;
        let mut applied_coupon: Option<Coupon> = None;
        if let Some(code) = input.coupon_code.filter(|code| !code.trim().is_empty()) {
            let coupon = validate_coupon_usable(&state, &code)?;
            let mut order_discount = 0.0;
            for item in &items {
                order_discount += coupon_discount_for(&coupon, &item.product_id, item.line_total);
            }
            if order_discount <= 0.0 {
                return Err(UnifiedError::unprocessable(
                    "Coupon does not apply to any item in this order",
                ));
            }
            discount_total = round_money(order_discount.min(subtotal));
            coupon_code = Some(code.trim().to_uppercase());
            applied_coupon = Some(coupon);
        }
        let order = Order {
            id: new_id(),
            user_id: owner.clone(),
            items,
            subtotal,
            discount_total,
            total: round_money(subtotal - discount_total),
            currency: currency.unwrap_or_else(|| "USD".to_string()),
            coupon_code: coupon_code.clone(),
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
                state.emit("order.paid", order_value);
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
            state.emit("payment.completed", order_value.clone());
            state.emit("order.paid", order_value.clone());
            let paid = state
                .orders
                .get(path.as_str())
                .map(|entry| entry.value().clone())
                .expect("order still present");
            return Ok(json!({ "order": paid, "payment_status": "completed", "provider": provider }));
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
        state.emit(
            "store.order.completed",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
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
        state.emit(
            "store.order.refunded",
            json!({
                "order": serde_json::to_value(&updated).unwrap_or(Value::Null),
                "order_id": updated.id,
                "license_ids": license_ids,
                "reason": reason,
            }),
        );
        Ok(json!({
            "order": updated,
            "license_ids": license_ids,
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
        let file_id = catalog_release_file(&state, &input.release_id);
        Ok(json!({
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
        }))
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

impl Module for StoreModule {
    fn id(&self) -> &'static str {
        "store"
    }

    fn name(&self) -> &'static str {
        "Store"
    }

    fn description(&self) -> &'static str {
        "Store for selling software: products with pricing tiers, coupons, cart, checkout, orders, refunds, signed downloads, entitlements, and update checks."
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
            RouteInfo { method: "POST", path: "/api/v1/store/orders/{id}/complete", owner: "store", description: "Complete order" },
            RouteInfo { method: "POST", path: "/api/v1/store/orders/{id}/cancel", owner: "store", description: "Cancel order" },
            RouteInfo { method: "POST", path: "/api/v1/store/orders/{id}/refund", owner: "store", description: "Refund order" },
            RouteInfo { method: "GET", path: "/api/v1/store/orders/{id}/download", owner: "store", description: "Download order" },
            RouteInfo { method: "GET", path: "/api/v1/store/entitlements", owner: "store", description: "List entitlements" },
            RouteInfo { method: "GET", path: "/api/v1/store/entitlements/{id}", owner: "store", description: "Get entitlement" },
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
                .route("/orders/{id}/complete", web::post().to(complete_order))
                .route("/orders/{id}/cancel", web::post().to(cancel_order))
                .route("/orders/{id}/refund", web::post().to(refund_order))
                .route("/orders/{id}/download", web::get().to(download_order))
                .route("/entitlements", web::get().to(list_entitlements))
                .route("/entitlements/{id}", web::get().to(get_entitlement))
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
