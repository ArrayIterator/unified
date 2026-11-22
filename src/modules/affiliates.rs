// Affiliate / Referral module — partner affiliates, referral links
// and codes, tracked conversions (click -> signup -> purchase),
// commissions (percent or fixed, per product or global default),
// payouts (requested through billing events; this module never
// moves money), affiliate dashboards, and fraud flags that are
// review-only and never auto-ban or auto-void.
//
// Routes live under `/api/v1/affiliates`:
//
//   Affiliates:
//     GET/POST / — list affiliates / apply as an affiliate.
//     GET/PATCH/DELETE /{id}.
//     POST /{id}/approve, /{id}/reject, /{id}/suspend,
//          /{id}/reactivate.
//   Links:
//     GET/POST /{id}/links — links of one affiliate.
//     GET/DELETE /links/{link_id}.
//   Clicks (public tracking endpoint, see the public-access note):
//     POST /track — record a click on a referral code.
//     GET  /{id}/clicks — clicks of one affiliate (filter flagged).
//   Conversions:
//     GET  /{id}/conversions — conversions of one affiliate.
//     POST /conversions — record a conversion manually (manage).
//   Commissions:
//     GET  /{id}/commissions — commissions of one affiliate.
//     POST /commissions/{commission_id}/approve.
//     POST /commissions/{commission_id}/void.
//   Payouts:
//     GET/POST /{id}/payouts — payout history / request a payout.
//     POST /payouts/{payout_id}/approve.
//     POST /payouts/{payout_id}/mark-paid.
//     POST /payouts/{payout_id}/reject.
//   Dashboard:
//     GET  /{id}/dashboard — clicks, conversions, earnings, payouts.
//     GET  /me/dashboard — the caller's own dashboard.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory affiliate, link, click, conversion, commission, and
// payout management; the approval state machine (pending ->
// approved / rejected / suspended, with reactivation back to
// approved); unique referral codes (auto-generated when the caller
// does not supply one); click recording with one-way SHA-256 IP
// hashes (raw IPs are never stored); last-click attribution inside
// a configurable window (default 30 days); commission computation
// from the affiliate default or a per-product rule; the commission
// state machine (pending -> approved -> paid, or void by a human
// with a reason); payout requests that only emit an event for the
// billing side (no money moves here); fraud flags (self-referral,
// duplicate IPs, impossible click velocity) that mark records for
// human review only; and hook listeners that turn
// `user.created` into signup conversions and
// `store.order.completed` into purchase conversions. What is NOT
// built here: actual payment of affiliates (the billing module
// owns that — this module only emits
// `affiliates.payout.requested`); automatic approval or payment of
// anything (every commission approval and every payout step is a
// manual, capability-checked action); automatic banning or
// voiding on fraud (flags are advisory only, except that a
// refunded order voids its still-pending commissions — a refund
// is a business fact, not a fraud judgement, and it never touches
// approved or paid commissions); and attribution without a
// referral signal — a purchase is only attributed when the order
// payload carries a referral code, click id, or affiliate id (the
// store / checkout must forward the code the visitor arrived
// with). Without that signal no conversion is guessed.
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`, and denied outright when a rule exists
// with `allowed: false`). There is no reusable per-request
// "is this endpoint public?" helper to call from a handler — the
// check lives in the API Manager middleware / `resolve_public`
// path — so the public route below relies on that gate: an
// administrator must create the matching `ApiAccessRule` row for
// `POST /api/v1/affiliates/track`. The route is registered in
// `route_info()` with `route_version_support()` defaulting to
// `All`, so the API Manager's per-version registry picks it up. Do
// NOT treat the absence of a capability check on the tracking
// route as an open door: without the rule the API Manager denies
// the call. Every other route checks a capability.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
// An affiliate may only ever see their own records: every
// affiliate-scoped read checks the manage capability OR that the
// affiliate's `user_id` equals the caller, server-side, before any
// data is returned.
//
// Events emitted on the hook bus:
//   - `affiliates.created`, `affiliates.approved`,
//     `affiliates.rejected`, `affiliates.suspended`,
//     `affiliates.reactivated`
//   - `affiliates.conversion.tracked`
//   - `affiliates.commission.created`, `affiliates.commission.approved`,
//     `affiliates.commission.voided`
//   - `affiliates.payout.requested` (payload carries `payout_id`,
//     `affiliate_id`, `amount`, `currency`, `method`, and
//     `commission_ids` for the billing module — no money moves here)
//   - `affiliates.payout.approved`, `affiliates.payout.paid`,
//     `affiliates.payout.rejected`
//   - `affiliates.click.flagged` (advisory only; see fraud note)
// Events this module listens to:
//   - `user.created` — signup conversion when the payload carries
//     a referral code (`referral_code`), a click id (`click_id`),
//     or an affiliate id (`affiliate_id`); otherwise skipped.
//   - `store.order.completed` — purchase conversion when the order
//     payload carries the same referral signal; the order total
//     and first line-item product id drive the commission. The
//     `payment.captured` event is deliberately NOT consumed as
//     well: the store emits both for one purchase and consuming
//     both would attribute (and pay) the same order twice.
//   - `store.order.refunded` — voids still-pending commissions of
//     the refunded order (approved / paid commissions are never
//     touched by this listener).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_EMAIL_LEN: usize = 254;
const MAX_CODE_LEN: usize = 64;
const MAX_CAMPAIGN_LEN: usize = 128;
const MAX_REASON_LEN: usize = 1000;
const MAX_METHOD_LEN: usize = 64;
const MAX_PRODUCT_ID_LEN: usize = 128;
const MAX_USER_AGENT_LEN: usize = 512;
const MAX_REFERRER_LEN: usize = 2048;
const MAX_USER_ID_LEN: usize = 128;
const MAX_COMPANY_ID_LEN: usize = 128;
/// Default attribution window in days (last-click wins inside it).
const DEFAULT_ATTRIBUTION_WINDOW_DAYS: i64 = 30;
/// Fraud thresholds. Crossing one only flags the click for review.
const DUPLICATE_IP_WINDOW_MINUTES: i64 = 60;
const DUPLICATE_IP_THRESHOLD: usize = 10;
const VELOCITY_WINDOW_MINUTES: i64 = 10;
const VELOCITY_THRESHOLD: usize = 20;
/// Random suffix length for auto-generated referral codes.
const CODE_RANDOM_LEN: usize = 8;

/// Capability names checked through the `role.check` hook.
const PERM_AFFILIATES_VIEW: &str = "affiliates.view";
const PERM_AFFILIATES_MANAGE: &str = "affiliates.manage";

/// Hook event names this module listens to (literals repeated here
/// so the module stays decoupled from the users / store modules).
const USER_CREATED: &str = "user.created";
const STORE_ORDER_COMPLETED: &str = "store.order.completed";
const STORE_ORDER_REFUNDED: &str = "store.order.refunded";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Approval lifecycle of an affiliate: `pending` -> `approved` /
/// `rejected`; `approved` -> `suspended` -> `approved` (reactivate).
/// A rejected affiliate may re-apply by creating a new record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AffiliateStatus {
    Pending,
    Approved,
    Rejected,
    Suspended,
}

impl AffiliateStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Suspended => "suspended",
        }
    }
}

/// How a commission is computed: a percentage of the order total,
/// or a fixed amount in the affiliate currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommissionType {
    Percent,
    Fixed,
}

impl CommissionType {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Percent => "percent",
            Self::Fixed => "fixed",
        }
    }
}

/// Lifecycle of a commission: `pending` -> `approved` -> `paid`,
/// or `void` (manual, with a reason) from pending / approved.
/// Approval and voiding are always manual, capability-checked
/// actions; nothing here auto-approves or auto-pays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommissionStatus {
    Pending,
    Approved,
    Paid,
    Void,
}

impl CommissionStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Paid => "paid",
            Self::Void => "void",
        }
    }
}

/// Lifecycle of a payout request: `requested` -> `approved` ->
/// `paid`, or `rejected` from requested / approved. The payout
/// itself (moving money) happens in the billing module, which
/// listens to `affiliates.payout.requested`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayoutStatus {
    Requested,
    Approved,
    Paid,
    Rejected,
}

impl PayoutStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Approved => "approved",
            Self::Paid => "paid",
            Self::Rejected => "rejected",
        }
    }
}

/// One affiliate (partner). `user_id` is the affiliate's own user
/// id (from the `X-User-Id` header); `commission_type` /
/// `commission_value` are the global default (percent 0..=100, or
/// a fixed amount); per-product overrides live in
/// `AffiliateCommissionRule`. `attribution_window_days` bounds
/// last-click attribution for this affiliate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Affiliate {
    pub id: Uuid,
    pub user_id: String,
    pub display_name: String,
    pub email: String,
    pub status: AffiliateStatus,
    pub commission_type: CommissionType,
    /// Percent (0..=100) when `commission_type` is percent, fixed
    /// amount otherwise. Money: NUMERIC(19,4) in the migration.
    pub commission_value: f64,
    pub currency: String,
    pub attribution_window_days: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payout_method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payout_reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Per-product commission override. `product_id` is the store
/// product id; the affiliate default applies when no rule matches
/// a product. At most one rule per (affiliate, product).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffiliateCommissionRule {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub product_id: String,
    pub commission_type: CommissionType,
    pub commission_value: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One referral link / code. `code` is globally unique (the value
/// visitors arrive with); `campaign` groups links; `product_id`
/// narrows a link to one product when set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffiliateLink {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One recorded click on a referral link. The visitor IP is stored
/// only as a one-way SHA-256 hex digest (`ip_hash`); the raw IP is
/// never stored or logged. `flagged` with `flag_reasons` is
/// advisory: a flagged click is still counted, and flagging never
/// blocks, bans, or voids anything on its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffiliateClick {
    pub id: Uuid,
    pub link_id: Uuid,
    pub affiliate_id: Uuid,
    /// SHA-256 hex of the visitor IP (plus nothing else); raw IPs
    /// are never stored.
    pub ip_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer: Option<String>,
    pub flagged: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flag_reasons: Vec<String>,
    pub converted: bool,
    pub clicked_at: DateTime<Utc>,
}

/// One tracked conversion: `signup` (a referred user registered)
/// or `purchase` (a referred order completed). `flagged` marks
/// conversions that need human review (for example self-referral:
/// the customer is the affiliate themselves).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffiliateConversion {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click_id: Option<Uuid>,
    pub customer_user_id: String,
    /// `signup` or `purchase`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// Order total for purchases, 0 for signups. Money:
    /// NUMERIC(19,4) in the migration.
    pub order_total: f64,
    pub currency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    pub flagged: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flag_reasons: Vec<String>,
    pub attributed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// One commission earned from a purchase conversion. Created in
/// `pending` and moved only by manual, capability-checked actions.
/// `fraud_flags` mirrors the conversion flags so a reviewer sees
/// why this commission needs a look before approving it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffiliateCommission {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub conversion_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    pub commission_type: CommissionType,
    /// Percent applied, or the fixed amount, at creation time.
    pub rate_or_amount: f64,
    /// Amount payable. Money: NUMERIC(19,4) in the migration.
    pub amount: f64,
    pub currency: String,
    pub status: CommissionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payout_id: Option<Uuid>,
    pub flagged: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fraud_flags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One payout request: a set of approved commissions paid out
/// together. This module only requests; the billing module moves
/// the money and the payout is marked paid here afterwards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffiliatePayout {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commission_ids: Vec<Uuid>,
    /// Sum of the commissions. Money: NUMERIC(19,4).
    pub total_amount: f64,
    pub currency: String,
    pub status: PayoutStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub requested_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Affiliate application / creation input. `user_id` may only be
/// set by a manager creating an affiliate for someone else; a
/// plain caller always applies as themselves.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateAffiliateRequest {
    #[serde(default)]
    pub user_id: Option<String>,
    pub display_name: String,
    pub email: String,
    #[serde(default)]
    pub commission_type: Option<CommissionType>,
    #[serde(default)]
    pub commission_value: Option<f64>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub attribution_window_days: Option<i64>,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub payout_method: Option<String>,
    #[serde(default)]
    pub payout_reference: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateAffiliateRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub commission_type: Option<CommissionType>,
    #[serde(default)]
    pub commission_value: Option<f64>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub attribution_window_days: Option<i64>,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub payout_method: Option<String>,
    #[serde(default)]
    pub payout_reference: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReviewRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateLinkRequest {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub campaign: Option<String>,
    #[serde(default)]
    pub product_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCommissionRuleRequest {
    pub product_id: String,
    pub commission_type: CommissionType,
    pub commission_value: f64,
}

/// Public click-tracking input. `ip` is accepted only as a
/// fallback when no `X-Forwarded-For` header is present (for
/// example behind a proxy that strips it); the value is hashed
/// immediately and never stored.
#[derive(Debug, Clone, Deserialize)]
pub struct TrackClickRequest {
    pub code: String,
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub referrer: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateConversionRequest {
    pub affiliate_id: String,
    #[serde(default)]
    pub link_id: Option<String>,
    #[serde(default)]
    pub click_id: Option<String>,
    pub customer_user_id: String,
    pub kind: String,
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub order_total: Option<f64>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub product_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RequestPayoutRequest {
    #[serde(default)]
    pub commission_ids: Option<Vec<String>>,
    #[serde(default)]
    pub method: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarkPaidRequest {
    #[serde(default)]
    pub reference: Option<String>,
}

/// In-memory stores for affiliates. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000044_affiliates`; until that migration is
/// executed, affiliates do not survive a restart.
#[derive(Clone)]
pub struct AffiliatesState {
    pub affiliates: Arc<DashMap<Uuid, Affiliate>>,
    pub commission_rules: Arc<DashMap<Uuid, AffiliateCommissionRule>>,
    pub links: Arc<DashMap<Uuid, AffiliateLink>>,
    pub clicks: Arc<DashMap<Uuid, AffiliateClick>>,
    pub conversions: Arc<DashMap<Uuid, AffiliateConversion>>,
    pub commissions: Arc<DashMap<Uuid, AffiliateCommission>>,
    pub payouts: Arc<DashMap<Uuid, AffiliatePayout>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for AffiliatesState {
    fn default() -> Self {
        Self {
            affiliates: Arc::new(DashMap::new()),
            commission_rules: Arc::new(DashMap::new()),
            links: Arc::new(DashMap::new()),
            clicks: Arc::new(DashMap::new()),
            conversions: Arc::new(DashMap::new()),
            commissions: Arc::new(DashMap::new()),
            payouts: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl AffiliatesState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the ai module.
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
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
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

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
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

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
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

/// Money: finite, non-negative, rounded to 4 decimal places.
fn valid_price(price: f64) -> UnifiedResult<f64> {
    if !price.is_finite() || price < 0.0 {
        return Err(UnifiedError::bad_request("price must be a finite number >= 0"));
    }
    Ok((price * 10_000.0).round() / 10_000.0)
}

fn round_money(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Currency: 3-letter ISO code, uppercased.
fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let upper = currency.trim().to_uppercase();
    if upper.len() != 3 || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code such as \"USD\"",
        ));
    }
    Ok(upper)
}

fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || email.contains(' ') {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// A referral code is lowercase letters, digits, and single hyphens
/// only, never leading or trailing with a hyphen, at most 64 chars.
fn valid_code(code: &str) -> bool {
    if code.is_empty() || code.len() > MAX_CODE_LEN {
        return false;
    }
    if code.starts_with('-') || code.ends_with('-') || code.contains("--") {
        return false;
    }
    code.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn slugify(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in value.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("ref");
    }
    out.truncate(MAX_CODE_LEN - CODE_RANDOM_LEN - 1);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Auto-generate a referral code from a display name plus a random
/// suffix. Retried by the caller while the code is already taken.
fn generate_code(base: &str) -> String {
    let slug = slugify(base);
    let random: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(CODE_RANDOM_LEN)
        .map(char::from)
        .collect::<String>()
        .to_lowercase();
    format!("{slug}-{random}")
}

/// One-way SHA-256 hex digest of a visitor IP. The raw IP is never
/// stored or logged anywhere in this module.
fn hash_ip(ip: &str) -> String {
    let digest = Sha256::digest(ip.trim().as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Best-effort visitor IP: first entry of `X-Forwarded-For`, else
/// the body-supplied fallback, else "unknown". The value is hashed
/// immediately by the caller; it is never stored raw.
fn visitor_ip(req: &HttpRequest, fallback: Option<&str>) -> String {
    if let Some(forwarded) = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
    {
        if let Some(first) = forwarded.split(',').next().map(str::trim) {
            if !first.is_empty() {
                return first.to_string();
            }
        }
    }
    match fallback {
        Some(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => "unknown".to_string(),
    }
}

fn validate_commission(
    commission_type: CommissionType,
    commission_value: f64,
) -> UnifiedResult<f64> {
    if !commission_value.is_finite() || commission_value < 0.0 {
        return Err(UnifiedError::bad_request(
            "commission_value must be a finite number >= 0",
        ));
    }
    match commission_type {
        CommissionType::Percent => {
            if commission_value > 100.0 {
                return Err(UnifiedError::bad_request(
                    "commission_value for a percent commission must be between 0 and 100",
                ));
            }
        }
        CommissionType::Fixed => {
            if commission_value > 1_000_000.0 {
                return Err(UnifiedError::bad_request(
                    "commission_value for a fixed commission must be at most 1000000",
                ));
            }
        }
    }
    Ok(round_money(commission_value))
}

fn load_affiliate(state: &AffiliatesState, raw_id: &str) -> UnifiedResult<Affiliate> {
    let id = parse_uuid(raw_id, "affiliate")?;
    state
        .affiliates
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Affiliate not found: {id}")))
}

fn load_affiliate_by_user(state: &AffiliatesState, user_id: &str) -> UnifiedResult<Affiliate> {
    state
        .affiliates
        .iter()
        .map(|entry| entry.value().clone())
        .find(|affiliate| affiliate.user_id == user_id)
        .ok_or_else(|| UnifiedError::not_found("No affiliate record for the caller"))
}

fn load_link(state: &AffiliatesState, raw_id: &str) -> UnifiedResult<AffiliateLink> {
    let id = parse_uuid(raw_id, "affiliate link")?;
    state
        .links
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Affiliate link not found: {id}")))
}

fn load_link_by_code(state: &AffiliatesState, code: &str) -> UnifiedResult<AffiliateLink> {
    state
        .links
        .iter()
        .map(|entry| entry.value().clone())
        .find(|link| link.code == code)
        .ok_or_else(|| UnifiedError::not_found(format!("Referral code not found: {code}")))
}

fn load_commission(state: &AffiliatesState, raw_id: &str) -> UnifiedResult<AffiliateCommission> {
    let id = parse_uuid(raw_id, "commission")?;
    state
        .commissions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Commission not found: {id}")))
}

fn load_payout(state: &AffiliatesState, raw_id: &str) -> UnifiedResult<AffiliatePayout> {
    let id = parse_uuid(raw_id, "payout")?;
    state
        .payouts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Payout not found: {id}")))
}

fn code_in_use(state: &AffiliatesState, code: &str, except: Option<Uuid>) -> bool {
    state
        .links
        .iter()
        .any(|entry| entry.value().code == code && Some(entry.value().id) != except)
}

fn is_manager(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_AFFILIATES_MANAGE)
}

/// Affiliate-scoped access: a manager may see any affiliate; any
/// other caller may only see the affiliate whose `user_id` is
/// their own. Enforced server-side on every affiliate-scoped read.
fn require_affiliate_access(
    ctx: &AppContext,
    caller: &Caller,
    affiliate: &Affiliate,
) -> UnifiedResult<()> {
    if is_manager(ctx, &caller.role) || affiliate.user_id == caller.actor {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "An affiliate may only access their own records",
        ))
    }
}

/// Resolve the commission rule for a purchase: the per-product rule
/// wins when one exists, otherwise the affiliate global default.
fn commission_for(
    state: &AffiliatesState,
    affiliate: &Affiliate,
    product_id: Option<&str>,
    order_total: f64,
) -> (CommissionType, f64, f64) {
    if let Some(product_id) = product_id {
        if let Some(rule) = state
            .commission_rules
            .iter()
            .map(|entry| entry.value().clone())
            .find(|rule| rule.affiliate_id == affiliate.id && rule.product_id == product_id)
        {
            let amount = match rule.commission_type {
                CommissionType::Percent => round_money(order_total * rule.commission_value / 100.0),
                CommissionType::Fixed => round_money(rule.commission_value),
            };
            return (rule.commission_type, rule.commission_value, amount);
        }
    }
    let amount = match affiliate.commission_type {
        CommissionType::Percent => round_money(order_total * affiliate.commission_value / 100.0),
        CommissionType::Fixed => round_money(affiliate.commission_value),
    };
    (affiliate.commission_type, affiliate.commission_value, amount)
}

/// Fraud checks applied to a click. They only add flag reasons;
/// they never block, ban, or void anything. Duplicate IPs: many
/// clicks from one IP hash on the same link inside an hour.
/// Velocity: many clicks from one IP hash across all links inside
/// ten minutes.
fn fraud_flags_for_click(state: &AffiliatesState, link_id: Uuid, ip_hash: &str, now: DateTime<Utc>) -> Vec<String> {
    let mut flags = Vec::new();
    let duplicate_window = now - Duration::minutes(DUPLICATE_IP_WINDOW_MINUTES);
    let same_link_same_ip = state
        .clicks
        .iter()
        .filter(|entry| {
            entry.value().link_id == link_id
                && entry.value().ip_hash == ip_hash
                && entry.value().clicked_at >= duplicate_window
        })
        .count();
    if same_link_same_ip >= DUPLICATE_IP_THRESHOLD {
        flags.push(format!(
            "duplicate_ip: {same_link_same_ip} clicks from the same address on this link in the last {DUPLICATE_IP_WINDOW_MINUTES} minutes"
        ));
    }
    let velocity_window = now - Duration::minutes(VELOCITY_WINDOW_MINUTES);
    let same_ip_any_link = state
        .clicks
        .iter()
        .filter(|entry| entry.value().ip_hash == ip_hash && entry.value().clicked_at >= velocity_window)
        .count();
    if same_ip_any_link >= VELOCITY_THRESHOLD {
        flags.push(format!(
            "click_velocity: {same_ip_any_link} clicks from the same address in the last {VELOCITY_WINDOW_MINUTES} minutes"
        ));
    }
    flags
}

/// Record a conversion and, for purchases, its pending commission.
/// Shared by the manual endpoint and the hook listeners so both
/// paths attribute identically. Fraud flags are recorded on the
/// conversion (and copied to the commission) for human review; they
/// never prevent the conversion from being recorded.
#[allow(clippy::too_many_arguments)]
fn record_conversion(
    state: &AffiliatesState,
    affiliate: &Affiliate,
    link_id: Option<Uuid>,
    click_id: Option<Uuid>,
    customer_user_id: &str,
    kind: &str,
    order_id: Option<String>,
    order_total: f64,
    currency: &str,
    product_id: Option<String>,
    now: DateTime<Utc>,
) -> UnifiedResult<(AffiliateConversion, Option<AffiliateCommission>)> {
    let mut flag_reasons: Vec<String> = Vec::new();
    // Self-referral: the affiliate buying (or signing up) through
    // their own link. Flagged for review only — the conversion is
    // still recorded and the commission stays pending for a human.
    if customer_user_id == affiliate.user_id {
        flag_reasons.push("self_referral: the customer is the affiliate themselves".to_string());
    }
    // Attribution window: the click that produced this conversion
    // must fall inside the affiliate's window (last-click wins).
    if let Some(click_id) = click_id {
        if let Some(click) = state.clicks.get(&click_id).map(|entry| entry.value().clone()) {
            let window_start = now - Duration::days(affiliate.attribution_window_days);
            if click.clicked_at < window_start {
                return Err(UnifiedError::bad_request(format!(
                    "Click is outside the {}-day attribution window",
                    affiliate.attribution_window_days
                )));
            }
            if let Some(mut entry) = state.clicks.get_mut(&click_id) {
                entry.converted = true;
            }
        }
    }
    let flagged = !flag_reasons.is_empty();
    let conversion = AffiliateConversion {
        id: Uuid::new_v4(),
        affiliate_id: affiliate.id,
        link_id,
        click_id,
        customer_user_id: customer_user_id.to_string(),
        kind: kind.to_string(),
        order_id: order_id.clone(),
        order_total: round_money(order_total),
        currency: currency.to_string(),
        product_id: product_id.clone(),
        flagged,
        flag_reasons: flag_reasons.clone(),
        attributed_at: now,
        created_at: now,
    };
    state.conversions.insert(conversion.id, conversion.clone());
    let mut commission = None;
    if kind == "purchase" {
        let (commission_type, rate_or_amount, amount) =
            commission_for(state, affiliate, product_id.as_deref(), order_total);
        let record = AffiliateCommission {
            id: Uuid::new_v4(),
            affiliate_id: affiliate.id,
            conversion_id: conversion.id,
            order_id: order_id.clone(),
            product_id: product_id.clone(),
            commission_type,
            rate_or_amount,
            amount,
            currency: currency.to_string(),
            status: CommissionStatus::Pending,
            payout_id: None,
            flagged,
            fraud_flags: flag_reasons.clone(),
            reviewed_by: None,
            review_reason: None,
            created_at: now,
            updated_at: now,
        };
        state.commissions.insert(record.id, record.clone());
        commission = Some(record);
    }
    state.emit(
        "affiliates.conversion.tracked",
        json!({
            "conversion_id": conversion.id,
            "affiliate_id": affiliate.id,
            "kind": kind,
            "customer_user_id": customer_user_id,
            "order_id": order_id,
            "order_total": conversion.order_total,
            "currency": currency,
            "flagged": flagged,
            "fraud_flags": flag_reasons,
        }),
    );
    if let Some(record) = &commission {
        state.emit(
            "affiliates.commission.created",
            json!({
                "commission_id": record.id,
                "affiliate_id": affiliate.id,
                "conversion_id": conversion.id,
                "amount": record.amount,
                "currency": record.currency,
                "status": record.status.as_str(),
                "flagged": record.flagged,
            }),
        );
    }
    Ok((conversion, commission))
}

/// Resolve the referral signal from a hook payload: a referral
/// code, a click id, or an affiliate id. Returns the affiliate and
/// the link / click it resolves to. Nothing is guessed: without a
/// signal the caller gets `None` and skips the event.
fn resolve_referral(
    state: &AffiliatesState,
    ctx: &HookContext,
) -> Option<(Affiliate, Option<Uuid>, Option<Uuid>)> {
    if let Some(code) = ctx.get("referral_code").and_then(|value| value.as_str()) {
        if let Ok(link) = load_link_by_code(state, code) {
            if let Some(affiliate) = state.affiliates.get(&link.affiliate_id).map(|e| e.value().clone()) {
                let click_id = ctx
                    .get("click_id")
                    .and_then(|value| value.as_str())
                    .and_then(|raw| Uuid::parse_str(raw).ok());
                return Some((affiliate, Some(link.id), click_id));
            }
        }
        return None;
    }
    if let Some(code) = ctx.get("affiliate_code").and_then(|value| value.as_str()) {
        if let Ok(link) = load_link_by_code(state, code) {
            if let Some(affiliate) = state.affiliates.get(&link.affiliate_id).map(|e| e.value().clone()) {
                return Some((affiliate, Some(link.id), None));
            }
        }
        return None;
    }
    if let Some(raw) = ctx.get("click_id").and_then(|value| value.as_str()) {
        if let Ok(click_id) = Uuid::parse_str(raw) {
            if let Some(click) = state.clicks.get(&click_id).map(|e| e.value().clone()) {
                if let Some(affiliate) = state.affiliates.get(&click.affiliate_id).map(|e| e.value().clone()) {
                    return Some((affiliate, Some(click.link_id), Some(click.id)));
                }
            }
        }
        return None;
    }
    if let Some(raw) = ctx.get("affiliate_id").and_then(|value| value.as_str()) {
        if let Ok(affiliate_id) = Uuid::parse_str(raw) {
            if let Some(affiliate) = state.affiliates.get(&affiliate_id).map(|e| e.value().clone()) {
                return Some((affiliate, None, None));
            }
        }
    }
    None
}

/// Dashboard figures for one affiliate, computed live from the
/// stores (no cached totals that could drift).
fn dashboard_for(state: &AffiliatesState, affiliate_id: Uuid) -> Value {
    let clicks = state
        .clicks
        .iter()
        .filter(|entry| entry.value().affiliate_id == affiliate_id)
        .count() as u64;
    let flagged_clicks = state
        .clicks
        .iter()
        .filter(|entry| entry.value().affiliate_id == affiliate_id && entry.value().flagged)
        .count() as u64;
    let conversions: Vec<AffiliateConversion> = state
        .conversions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|conversion| conversion.affiliate_id == affiliate_id)
        .collect();
    let signups = conversions
        .iter()
        .filter(|conversion| conversion.kind == "signup")
        .count() as u64;
    let purchases = conversions
        .iter()
        .filter(|conversion| conversion.kind == "purchase")
        .count() as u64;
    let flagged_conversions = conversions.iter().filter(|c| c.flagged).count() as u64;
    let commissions: Vec<AffiliateCommission> = state
        .commissions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|commission| commission.affiliate_id == affiliate_id)
        .collect();
    let sum_by = |status: CommissionStatus| -> f64 {
        round_money(
            commissions
                .iter()
                .filter(|commission| commission.status == status)
                .map(|commission| commission.amount)
                .sum(),
        )
    };
    let payouts: Vec<AffiliatePayout> = state
        .payouts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|payout| payout.affiliate_id == affiliate_id)
        .collect();
    let mut payouts_sorted = payouts.clone();
    payouts_sorted.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    json!({
        "affiliate_id": affiliate_id,
        "clicks": clicks,
        "flagged_clicks": flagged_clicks,
        "conversions": conversions.len(),
        "signups": signups,
        "purchases": purchases,
        "flagged_conversions": flagged_conversions,
        "earnings": {
            "pending": sum_by(CommissionStatus::Pending),
            "approved": sum_by(CommissionStatus::Approved),
            "paid": sum_by(CommissionStatus::Paid),
            "void": sum_by(CommissionStatus::Void),
        },
        "commissions": commissions.len(),
        "payouts": payouts_sorted,
    })
}

// ---------------------------------------------------------------------------
// Hook listeners
// ---------------------------------------------------------------------------

/// `user.created` -> signup conversion. Only when the payload
/// carries a referral signal (see `resolve_referral`); a plain
/// signup without one is not a referral and is skipped. The
/// customer id is read from `id` / `user_id` / `user.id`.
fn on_user_created(state: Arc<AffiliatesState>, ctx: &mut HookContext) {
    let customer_id = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("user_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            ctx.get("user")
                .and_then(|user| user.get("id"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(customer_id) = customer_id else {
        ctx.add_result(json!({
            "affiliates": "skipped",
            "reason": "user.created carries no customer id",
        }));
        return;
    };
    let Some((affiliate, link_id, click_id)) = resolve_referral(&state, ctx) else {
        ctx.add_result(json!({
            "affiliates": "skipped",
            "reason": "user.created carries no referral code, click id, or affiliate id",
        }));
        return;
    };
    if affiliate.status != AffiliateStatus::Approved {
        ctx.add_result(json!({
            "affiliates": "skipped",
            "affiliate_id": affiliate.id,
            "reason": format!("affiliate is {} and cannot earn conversions", affiliate.status.as_str()),
        }));
        return;
    }
    let now = Utc::now();
    match record_conversion(
        &state,
        &affiliate,
        link_id,
        click_id,
        &customer_id,
        "signup",
        None,
        0.0,
        &affiliate.currency,
        None,
        now,
    ) {
        Ok((conversion, _)) => {
            ctx.add_result(json!({
                "affiliates": "conversion_tracked",
                "conversion_id": conversion.id,
                "kind": "signup",
                "flagged": conversion.flagged,
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "affiliates": "skipped",
                "reason": error.message,
            }));
        }
    }
}

/// `store.order.completed` -> purchase conversion. The order
/// payload is the store `Order` JSON (`id`, `user_id`, `total`,
/// `currency`, `items[].product_id`); the referral signal must be
/// present on the payload (`referral_code`, `affiliate_code`,
/// `click_id`, or `affiliate_id`). `payment.captured` is not
/// consumed as well, so one purchase is never attributed twice.
fn on_store_order_completed(state: Arc<AffiliatesState>, ctx: &mut HookContext) {
    let order = ctx.get("order").cloned().unwrap_or_else(|| ctx.data.clone());
    let order_id = order
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let customer_id = order
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let (Some(order_id), Some(customer_id)) = (order_id, customer_id) else {
        ctx.add_result(json!({
            "affiliates": "skipped",
            "reason": "store.order.completed carries no order id or user id",
        }));
        return;
    };
    // A completed order that was already attributed (for example a
    // duplicate emit) must not earn a second commission.
    if state
        .conversions
        .iter()
        .any(|entry| entry.value().order_id.as_deref() == Some(order_id.as_str()) && entry.value().kind == "purchase")
    {
        ctx.add_result(json!({
            "affiliates": "already_tracked",
            "order_id": order_id,
        }));
        return;
    }
    let Some((affiliate, link_id, click_id)) = resolve_referral(&state, ctx) else {
        ctx.add_result(json!({
            "affiliates": "skipped",
            "order_id": order_id,
            "reason": "store.order.completed carries no referral signal",
        }));
        return;
    };
    if affiliate.status != AffiliateStatus::Approved {
        ctx.add_result(json!({
            "affiliates": "skipped",
            "affiliate_id": affiliate.id,
            "order_id": order_id,
            "reason": format!("affiliate is {} and cannot earn conversions", affiliate.status.as_str()),
        }));
        return;
    }
    let total = order.get("total").and_then(|value| value.as_f64()).unwrap_or(0.0);
    let currency = order
        .get("currency")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| affiliate.currency.clone());
    let product_id = order
        .get("items")
        .and_then(|items| items.as_array())
        .and_then(|items| items.first())
        .and_then(|item| item.get("product_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let now = Utc::now();
    match record_conversion(
        &state,
        &affiliate,
        link_id,
        click_id,
        &customer_id,
        "purchase",
        Some(order_id.clone()),
        total,
        &currency,
        product_id,
        now,
    ) {
        Ok((conversion, commission)) => {
            ctx.add_result(json!({
                "affiliates": "conversion_tracked",
                "conversion_id": conversion.id,
                "kind": "purchase",
                "order_id": order_id,
                "commission_id": commission.map(|record| record.id),
                "flagged": conversion.flagged,
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "affiliates": "skipped",
                "order_id": order_id,
                "reason": error.message,
            }));
        }
    }
}

/// `store.order.refunded` -> void still-pending commissions of the
/// refunded order. This is a refund-driven void (a business fact),
/// not a fraud judgement: approved and paid commissions are never
/// touched, and flagged commissions are voided here only because
/// the underlying order no longer stands.
fn on_store_order_refunded(state: Arc<AffiliatesState>, ctx: &mut HookContext) {
    let order = ctx.get("order").cloned().unwrap_or_else(|| ctx.data.clone());
    let Some(order_id) = order
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        ctx.add_result(json!({
            "affiliates": "skipped",
            "reason": "store.order.refunded carries no order id",
        }));
        return;
    };
    let now = Utc::now();
    let mut voided: Vec<Uuid> = Vec::new();
    for mut entry in state.commissions.iter_mut() {
        let record = entry.value_mut();
        if record.order_id.as_deref() == Some(order_id.as_str())
            && record.status == CommissionStatus::Pending
        {
            record.status = CommissionStatus::Void;
            record.review_reason = Some("order_refunded".to_string());
            record.updated_at = now;
            voided.push(record.id);
        }
    }
    for id in &voided {
        if let Some(record) = state.commissions.get(id).map(|entry| entry.value().clone()) {
            state.emit(
                "affiliates.commission.voided",
                json!({
                    "commission_id": record.id,
                    "affiliate_id": record.affiliate_id,
                    "order_id": order_id,
                    "reason": "order_refunded",
                }),
            );
        }
    }
    ctx.add_result(json!({
        "affiliates": "refund_processed",
        "order_id": order_id,
        "commissions_voided": voided.len(),
    }));
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct AffiliatesModule {
    state: AffiliatesState,
}

impl AffiliatesModule {
    pub fn new() -> Self {
        Self {
            state: AffiliatesState::default(),
        }
    }
}

impl Default for AffiliatesModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for AffiliatesModule {
    fn id(&self) -> &'static str {
        "affiliates"
    }

    fn name(&self) -> &'static str {
        "Affiliate / Referral"
    }

    fn description(&self) -> &'static str {
        "Affiliate and referral program: affiliate applications with an approval workflow, unique referral links and codes, click tracking with fraud flags for review only, signup and purchase conversions with last-click attribution, percent or fixed commissions per product or global default, and payout requests emitted to billing (no money moves here)."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("commerce")
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "store", "billing"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["dashmap", "serde_json", "sha2", "rand"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = Arc::new(self.state.clone());
        bus.register(
            USER_CREATED,
            "affiliates",
            Arc::new(move |ctx: &mut HookContext| {
                on_user_created(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            STORE_ORDER_COMPLETED,
            "affiliates",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_completed(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            STORE_ORDER_REFUNDED,
            "affiliates",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_refunded(Arc::clone(&state), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/affiliates")
                .app_data(state)
                .route("", web::get().to(list_affiliates))
                .route("", web::post().to(create_affiliate))
                .route("/track", web::post().to(track_click))
                .route("/conversions", web::post().to(create_conversion))
                .route("/me/dashboard", web::get().to(my_dashboard))
                .route("/links/{link_id}", web::get().to(get_link))
                .route("/links/{link_id}", web::delete().to(delete_link))
                .route("/commissions/{commission_id}/approve", web::post().to(approve_commission))
                .route("/commissions/{commission_id}/void", web::post().to(void_commission))
                .route("/payouts/{payout_id}/approve", web::post().to(approve_payout))
                .route("/payouts/{payout_id}/mark-paid", web::post().to(mark_payout_paid))
                .route("/payouts/{payout_id}/reject", web::post().to(reject_payout))
                .route("/{id}", web::get().to(get_affiliate))
                .route("/{id}", web::patch().to(update_affiliate))
                .route("/{id}", web::delete().to(delete_affiliate))
                .route("/{id}/approve", web::post().to(approve_affiliate))
                .route("/{id}/reject", web::post().to(reject_affiliate))
                .route("/{id}/suspend", web::post().to(suspend_affiliate))
                .route("/{id}/reactivate", web::post().to(reactivate_affiliate))
                .route("/{id}/links", web::get().to(list_links))
                .route("/{id}/links", web::post().to(create_link))
                .route("/{id}/commission-rules", web::get().to(list_commission_rules))
                .route("/{id}/commission-rules", web::post().to(create_commission_rule))
                .route(
                    "/{id}/commission-rules/{rule_id}",
                    web::delete().to(delete_commission_rule),
                )
                .route("/{id}/clicks", web::get().to(list_clicks))
                .route("/{id}/conversions", web::get().to(list_conversions))
                .route("/{id}/commissions", web::get().to(list_commissions))
                .route("/{id}/payouts", web::get().to(list_payouts))
                .route("/{id}/payouts", web::post().to(request_payout))
                .route("/{id}/dashboard", web::get().to(get_dashboard)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/affiliates", owner: "affiliates", description: "List affiliates (paginated, filter by status, company_id, user_id, search)." },
            RouteInfo { method: "POST", path: "/api/v1/affiliates", owner: "affiliates", description: "Apply as an affiliate (own user id; a manager may apply for someone else)." },
            RouteInfo { method: "GET", path: "/api/v1/affiliates/{id}", owner: "affiliates", description: "Get one affiliate (manager or the affiliate themselves)." },
            RouteInfo { method: "PATCH", path: "/api/v1/affiliates/{id}", owner: "affiliates", description: "Update an affiliate (manage; commission defaults, payout method)." },
            RouteInfo { method: "DELETE", path: "/api/v1/affiliates/{id}", owner: "affiliates", description: "Delete an affiliate with no conversions, commissions, or payouts." },
            RouteInfo { method: "POST", path: "/api/v1/affiliates/{id}/approve", owner: "affiliates", description: "Approve a pending affiliate." },
            RouteInfo { method: "POST", path: "/api/v1/affiliates/{id}/reject", owner: "affiliates", description: "Reject a pending affiliate (reason optional)." },
            RouteInfo { method: "POST", path: "/api/v1/affiliates/{id}/suspend", owner: "affiliates", description: "Suspend an approved affiliate." },
            RouteInfo { method: "POST", path: "/api/v1/affiliates/{id}/reactivate", owner: "affiliates", description: "Reactivate a suspended affiliate back to approved." },
            RouteInfo { method: "GET", path: "/api/v1/affiliates/{id}/links", owner: "affiliates", description: "List referral links of an affiliate (manager or owner)." },
            RouteInfo { method: "POST", path: "/api/v1/affiliates/{id}/links", owner: "affiliates", description: "Create a referral link (code auto-generated when omitted; manager or owner)." },
            RouteInfo { method: "GET", path: "/api/v1/affiliates/links/{link_id}", owner: "affiliates", description: "Get one referral link (manager or owner)." },
            RouteInfo { method: "DELETE", path: "/api/v1/affiliates/links/{link_id}", owner: "affiliates", description: "Delete a referral link with no clicks (manager or owner)." },
            RouteInfo { method: "GET", path: "/api/v1/affiliates/{id}/commission-rules", owner: "affiliates", description: "List per-product commission rules of an affiliate." },
            RouteInfo { method: "POST", path: "/api/v1/affiliates/{id}/commission-rules", owner: "affiliates", description: "Create a per-product commission rule (manage; one rule per product)." },
            RouteInfo { method: "DELETE", path: "/api/v1/affiliates/{id}/commission-rules/{rule_id}", owner: "affiliates", description: "Delete a per-product commission rule (manage)." },
            RouteInfo { method: "POST", path: "/api/v1/affiliates/track", owner: "affiliates", description: "PUBLIC: record a referral click (no auth; gated by the API Manager ApiAccessRule for this path)." },
            RouteInfo { method: "GET", path: "/api/v1/affiliates/{id}/clicks", owner: "affiliates", description: "List clicks of an affiliate (paginated, filter flagged; manager or owner)." },
            RouteInfo { method: "GET", path: "/api/v1/affiliates/{id}/conversions", owner: "affiliates", description: "List conversions of an affiliate (paginated, filter kind, flagged; manager or owner)." },
            RouteInfo { method: "POST", path: "/api/v1/affiliates/conversions", owner: "affiliates", description: "Record a conversion manually (manage)." },
            RouteInfo { method: "GET", path: "/api/v1/affiliates/{id}/commissions", owner: "affiliates", description: "List commissions of an affiliate (paginated, filter status, flagged; manager or owner)." },
            RouteInfo { method: "POST", path: "/api/v1/affiliates/commissions/{commission_id}/approve", owner: "affiliates", description: "Approve a pending commission (manage)." },
            RouteInfo { method: "POST", path: "/api/v1/affiliates/commissions/{commission_id}/void", owner: "affiliates", description: "Void a pending or approved commission with a reason (manage)." },
            RouteInfo { method: "GET", path: "/api/v1/affiliates/{id}/payouts", owner: "affiliates", description: "List payouts of an affiliate (paginated, filter status; manager or owner)." },
            RouteInfo { method: "POST", path: "/api/v1/affiliates/{id}/payouts", owner: "affiliates", description: "Request a payout for approved commissions (manager or owner; emits affiliates.payout.requested for billing)." },
            RouteInfo { method: "POST", path: "/api/v1/affiliates/payouts/{payout_id}/approve", owner: "affiliates", description: "Approve a requested payout (manage)." },
            RouteInfo { method: "POST", path: "/api/v1/affiliates/payouts/{payout_id}/mark-paid", owner: "affiliates", description: "Mark an approved payout paid and its commissions paid (manage; after billing moved the money)." },
            RouteInfo { method: "POST", path: "/api/v1/affiliates/payouts/{payout_id}/reject", owner: "affiliates", description: "Reject a requested or approved payout and release its commissions (manage)." },
            RouteInfo { method: "GET", path: "/api/v1/affiliates/{id}/dashboard", owner: "affiliates", description: "Affiliate dashboard: clicks, conversions, earnings by commission status, payout history (manager or owner)." },
            RouteInfo { method: "GET", path: "/api/v1/affiliates/me/dashboard", owner: "affiliates", description: "The caller's own affiliate dashboard." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AffiliatesModule::new()));
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_affiliates(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Affiliate>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let user_filter = params.filters.get("user_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let manager = is_manager(&ctx, &caller.role);
        let mut items: Vec<Affiliate> = state
            .affiliates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|affiliate| {
                // Non-managers only ever see their own record.
                if !manager && affiliate.user_id != caller.actor {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if affiliate.status.as_str() != status {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if affiliate.company_id.as_deref() != Some(company_id.as_str()) {
                        return false;
                    }
                }
                if let Some(user_id) = &user_filter {
                    if affiliate.user_id != *user_id {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !affiliate.display_name.to_lowercase().contains(&search)
                    && !affiliate.email.to_lowercase().contains(&search)
                    && !affiliate.user_id.to_lowercase().contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn create_affiliate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    body: web::Json<CreateAffiliateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Affiliate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let input = body.into_inner();
        // Only a manager may apply for someone else; everyone else
        // applies as themselves.
        let user_id = match &input.user_id {
            Some(other) if other.trim() != caller.actor => {
                require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
                require_text(other, "user_id", MAX_USER_ID_LEN)?
            }
            _ => caller.actor.clone(),
        };
        if state
            .affiliates
            .iter()
            .any(|entry| entry.value().user_id == user_id)
        {
            return Err(UnifiedError::conflict(format!(
                "User \"{user_id}\" already has an affiliate record"
            )));
        }
        let display_name = require_text(&input.display_name, "display_name", MAX_NAME_LEN)?;
        let email = require_text(&input.email, "email", MAX_EMAIL_LEN)?;
        if !valid_email(&email) {
            return Err(UnifiedError::bad_request("email must be a valid email address"));
        }
        let commission_type = input.commission_type.unwrap_or(CommissionType::Percent);
        let commission_value = validate_commission(
            commission_type,
            input.commission_value.unwrap_or(10.0),
        )?;
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => "USD".to_string(),
        };
        let attribution_window_days = input
            .attribution_window_days
            .unwrap_or(DEFAULT_ATTRIBUTION_WINDOW_DAYS);
        if !(1..=365).contains(&attribution_window_days) {
            return Err(UnifiedError::bad_request(
                "attribution_window_days must be between 1 and 365",
            ));
        }
        let company_id = match &input.company_id {
            Some(raw) => Some(require_text(raw, "company_id", MAX_COMPANY_ID_LEN)?),
            None => None,
        };
        let payout_method = match &input.payout_method {
            Some(raw) => Some(require_text(raw, "payout_method", MAX_METHOD_LEN)?),
            None => None,
        };
        let payout_reference = match &input.payout_reference {
            Some(raw) => Some(require_text(raw, "payout_reference", MAX_NAME_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let affiliate = Affiliate {
            id: Uuid::new_v4(),
            user_id,
            display_name,
            email,
            status: AffiliateStatus::Pending,
            commission_type,
            commission_value,
            currency,
            attribution_window_days,
            company_id,
            payout_method,
            payout_reference,
            reviewed_by: None,
            review_reason: None,
            created_at: now,
            updated_at: now,
        };
        state.affiliates.insert(affiliate.id, affiliate.clone());
        state.emit(
            "affiliates.created",
            json!({
                "affiliate_id": affiliate.id,
                "user_id": affiliate.user_id,
                "status": affiliate.status.as_str(),
            }),
        );
        Ok(affiliate)
    })();
    match result {
        Ok(affiliate) => created(affiliate),
        Err(error) => err(error, debug),
    }
}

async fn get_affiliate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Affiliate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = load_affiliate(&state, &path)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        Ok(affiliate)
    })();
    match result {
        Ok(affiliate) => ok(affiliate),
        Err(error) => err(error, debug),
    }
}

async fn update_affiliate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateAffiliateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Affiliate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let input = body.into_inner();
        let mut affiliate = load_affiliate(&state, &path)?;
        if let Some(raw) = &input.display_name {
            affiliate.display_name = require_text(raw, "display_name", MAX_NAME_LEN)?;
        }
        if let Some(raw) = &input.email {
            let email = require_text(raw, "email", MAX_EMAIL_LEN)?;
            if !valid_email(&email) {
                return Err(UnifiedError::bad_request("email must be a valid email address"));
            }
            affiliate.email = email;
        }
        if let Some(commission_type) = input.commission_type {
            let value = input
                .commission_value
                .unwrap_or(affiliate.commission_value);
            affiliate.commission_value = validate_commission(commission_type, value)?;
            affiliate.commission_type = commission_type;
        } else if let Some(value) = input.commission_value {
            affiliate.commission_value =
                validate_commission(affiliate.commission_type, value)?;
        }
        if let Some(raw) = &input.currency {
            affiliate.currency = valid_currency(raw)?;
        }
        if let Some(window) = input.attribution_window_days {
            if !(1..=365).contains(&window) {
                return Err(UnifiedError::bad_request(
                    "attribution_window_days must be between 1 and 365",
                ));
            }
            affiliate.attribution_window_days = window;
        }
        if let Some(raw) = &input.company_id {
            affiliate.company_id = Some(require_text(raw, "company_id", MAX_COMPANY_ID_LEN)?);
        }
        if let Some(raw) = &input.payout_method {
            affiliate.payout_method = Some(require_text(raw, "payout_method", MAX_METHOD_LEN)?);
        }
        if let Some(raw) = &input.payout_reference {
            affiliate.payout_reference =
                Some(require_text(raw, "payout_reference", MAX_NAME_LEN)?);
        }
        affiliate.updated_at = Utc::now();
        state.affiliates.insert(affiliate.id, affiliate.clone());
        Ok(affiliate)
    })();
    match result {
        Ok(affiliate) => ok(affiliate),
        Err(error) => err(error, debug),
    }
}

async fn delete_affiliate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let affiliate = load_affiliate(&state, &path)?;
        if state
            .conversions
            .iter()
            .any(|entry| entry.value().affiliate_id == affiliate.id)
            || state
                .commissions
                .iter()
                .any(|entry| entry.value().affiliate_id == affiliate.id)
            || state
                .payouts
                .iter()
                .any(|entry| entry.value().affiliate_id == affiliate.id)
        {
            return Err(UnifiedError::conflict(
                "Affiliate has conversions, commissions, or payouts and cannot be deleted; suspend it instead",
            ));
        }
        state.links.retain(|_, link| link.affiliate_id != affiliate.id);
        state
            .commission_rules
            .retain(|_, rule| rule.affiliate_id != affiliate.id);
        state.clicks.retain(|_, click| click.affiliate_id != affiliate.id);
        state.affiliates.remove(&affiliate.id);
        Ok(json!({ "deleted": affiliate.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Shared approval-transition helper: only the listed source
/// statuses may move to `target`.
fn transition_affiliate(
    state: &AffiliatesState,
    caller: &Caller,
    raw_id: &str,
    from: &[AffiliateStatus],
    target: AffiliateStatus,
    event: &str,
    reason: Option<String>,
) -> UnifiedResult<Affiliate> {
    let mut affiliate = load_affiliate(state, raw_id)?;
    if !from.contains(&affiliate.status) {
        return Err(UnifiedError::conflict(format!(
            "Affiliate is {} and cannot move to {}",
            affiliate.status.as_str(),
            target.as_str()
        )));
    }
    affiliate.status = target;
    affiliate.reviewed_by = Some(caller.actor.clone());
    affiliate.review_reason = reason;
    affiliate.updated_at = Utc::now();
    state.affiliates.insert(affiliate.id, affiliate.clone());
    state.emit(
        event,
        json!({
            "affiliate_id": affiliate.id,
            "user_id": affiliate.user_id,
            "status": affiliate.status.as_str(),
            "reviewed_by": caller.actor,
        }),
    );
    Ok(affiliate)
}

async fn approve_affiliate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Affiliate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?;
        transition_affiliate(
            &state,
            &caller,
            &path,
            &[AffiliateStatus::Pending],
            AffiliateStatus::Approved,
            "affiliates.approved",
            reason,
        )
    })();
    match result {
        Ok(affiliate) => ok(affiliate),
        Err(error) => err(error, debug),
    }
}

async fn reject_affiliate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Affiliate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?;
        transition_affiliate(
            &state,
            &caller,
            &path,
            &[AffiliateStatus::Pending],
            AffiliateStatus::Rejected,
            "affiliates.rejected",
            reason,
        )
    })();
    match result {
        Ok(affiliate) => ok(affiliate),
        Err(error) => err(error, debug),
    }
}

async fn suspend_affiliate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Affiliate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?;
        transition_affiliate(
            &state,
            &caller,
            &path,
            &[AffiliateStatus::Approved],
            AffiliateStatus::Suspended,
            "affiliates.suspended",
            reason,
        )
    })();
    match result {
        Ok(affiliate) => ok(affiliate),
        Err(error) => err(error, debug),
    }
}

async fn reactivate_affiliate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Affiliate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?;
        transition_affiliate(
            &state,
            &caller,
            &path,
            &[AffiliateStatus::Suspended],
            AffiliateStatus::Approved,
            "affiliates.reactivated",
            reason,
        )
    })();
    match result {
        Ok(affiliate) => ok(affiliate),
        Err(error) => err(error, debug),
    }
}

async fn list_links(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AffiliateLink>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = load_affiliate(&state, &path)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        let params = query.into_inner();
        let active_filter = params.filters.get("active").cloned();
        let campaign_filter = params.filters.get("campaign").cloned();
        let mut items: Vec<AffiliateLink> = state
            .links
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|link| {
                link.affiliate_id == affiliate.id
                    && active_filter
                        .as_deref()
                        .is_none_or(|raw| link.active.to_string() == raw)
                    && campaign_filter
                        .as_deref()
                        .is_none_or(|raw| link.campaign.as_deref() == Some(raw))
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn create_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateLinkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliateLink> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = load_affiliate(&state, &path)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        let input = body.into_inner();
        let code = match &input.code {
            Some(raw) => {
                let code = raw.trim().to_lowercase();
                if !valid_code(&code) {
                    return Err(UnifiedError::bad_request(
                        "code must be lowercase letters, digits, and single hyphens only (no leading/trailing hyphen, max 64 characters)",
                    ));
                }
                if code_in_use(&state, &code, None) {
                    return Err(UnifiedError::conflict(format!(
                        "Referral code is already in use: {code}"
                    )));
                }
                code
            }
            None => {
                let mut code = generate_code(&affiliate.display_name);
                let mut attempts = 0;
                while code_in_use(&state, &code, None) && attempts < 10 {
                    code = generate_code(&affiliate.display_name);
                    attempts += 1;
                }
                if code_in_use(&state, &code, None) {
                    return Err(UnifiedError::conflict(
                        "Could not generate a unique referral code; try again with an explicit code",
                    ));
                }
                code
            }
        };
        let campaign = match &input.campaign {
            Some(raw) => Some(require_text(raw, "campaign", MAX_CAMPAIGN_LEN)?),
            None => None,
        };
        let product_id = match &input.product_id {
            Some(raw) => Some(require_text(raw, "product_id", MAX_PRODUCT_ID_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let link = AffiliateLink {
            id: Uuid::new_v4(),
            affiliate_id: affiliate.id,
            code,
            campaign,
            product_id,
            active: true,
            created_at: now,
            updated_at: now,
        };
        state.links.insert(link.id, link.clone());
        Ok(link)
    })();
    match result {
        Ok(link) => created(link),
        Err(error) => err(error, debug),
    }
}

async fn get_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliateLink> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let link = load_link(&state, &path)?;
        let affiliate = load_affiliate(&state, &link.affiliate_id.to_string())?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        Ok(link)
    })();
    match result {
        Ok(link) => ok(link),
        Err(error) => err(error, debug),
    }
}

async fn delete_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let link = load_link(&state, &path)?;
        let affiliate = load_affiliate(&state, &link.affiliate_id.to_string())?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        if state
            .clicks
            .iter()
            .any(|entry| entry.value().link_id == link.id)
        {
            return Err(UnifiedError::conflict(
                "Referral link has recorded clicks and cannot be deleted; keep the link for audit instead",
            ));
        }
        state.links.remove(&link.id);
        Ok(json!({ "deleted": link.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_commission_rules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AffiliateCommissionRule>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = load_affiliate(&state, &path)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        let params = query.into_inner();
        let mut items: Vec<AffiliateCommissionRule> = state
            .commission_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.affiliate_id == affiliate.id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn create_commission_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateCommissionRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliateCommissionRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let affiliate = load_affiliate(&state, &path)?;
        let input = body.into_inner();
        let product_id = require_text(&input.product_id, "product_id", MAX_PRODUCT_ID_LEN)?;
        let value = validate_commission(input.commission_type, input.commission_value)?;
        if state.commission_rules.iter().any(|entry| {
            entry.value().affiliate_id == affiliate.id && entry.value().product_id == product_id
        }) {
            return Err(UnifiedError::conflict(format!(
                "A commission rule already exists for product \"{product_id}\" on this affiliate"
            )));
        }
        let now = Utc::now();
        let rule = AffiliateCommissionRule {
            id: Uuid::new_v4(),
            affiliate_id: affiliate.id,
            product_id,
            commission_type: input.commission_type,
            commission_value: value,
            created_at: now,
            updated_at: now,
        };
        state.commission_rules.insert(rule.id, rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(rule) => created(rule),
        Err(error) => err(error, debug),
    }
}

async fn delete_commission_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let (affiliate_id, rule_id) = path.into_inner();
        let affiliate = load_affiliate(&state, &affiliate_id)?;
        let rule_uuid = parse_uuid(&rule_id, "commission rule")?;
        let rule = state
            .commission_rules
            .get(&rule_uuid)
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.affiliate_id == affiliate.id)
            .ok_or_else(|| UnifiedError::not_found(format!("Commission rule not found: {rule_id}")))?;
        state.commission_rules.remove(&rule.id);
        Ok(json!({ "deleted": rule.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Public click tracking. No caller identity and no capability
/// check here by design: clicks come from anonymous visitors. The
/// route is gated by the API Manager `ApiAccessRule` for
/// `POST /api/v1/affiliates/track` (see the header doc). The raw
/// visitor IP is hashed immediately and never stored.
async fn track_click(
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    body: web::Json<TrackClickRequest>,
) -> HttpResponse {
    let result: UnifiedResult<AffiliateClick> = (|| {
        let input = body.into_inner();
        let code = input.code.trim().to_lowercase();
        if !valid_code(&code) {
            return Err(UnifiedError::bad_request("code is not a valid referral code"));
        }
        let link = load_link_by_code(&state, &code)?;
        if !link.active {
            return Err(UnifiedError::not_found(format!(
                "Referral code not found: {code}"
            )));
        }
        let ip = visitor_ip(&req, input.ip.as_deref());
        let ip_hash = hash_ip(&ip);
        let now = Utc::now();
        // Fraud flags are advisory only: the click is recorded
        // either way and flagging never blocks anything.
        let flag_reasons = fraud_flags_for_click(&state, link.id, &ip_hash, now);
        let flagged = !flag_reasons.is_empty();
        let user_agent = match &input.user_agent {
            Some(raw) if !raw.trim().is_empty() => {
                Some(raw.trim().chars().take(MAX_USER_AGENT_LEN).collect())
            }
            _ => None,
        };
        let referrer = match &input.referrer {
            Some(raw) if !raw.trim().is_empty() => {
                Some(raw.trim().chars().take(MAX_REFERRER_LEN).collect())
            }
            _ => None,
        };
        let click = AffiliateClick {
            id: Uuid::new_v4(),
            link_id: link.id,
            affiliate_id: link.affiliate_id,
            ip_hash,
            user_agent,
            referrer,
            flagged,
            flag_reasons: flag_reasons.clone(),
            converted: false,
            clicked_at: now,
        };
        state.clicks.insert(click.id, click.clone());
        if flagged {
            state.emit(
                "affiliates.click.flagged",
                json!({
                    "click_id": click.id,
                    "link_id": link.id,
                    "affiliate_id": link.affiliate_id,
                    "flag_reasons": flag_reasons,
                }),
            );
        }
        Ok(click)
    })();
    match result {
        Ok(click) => created(click),
        Err(error) => err(error, false),
    }
}

async fn list_clicks(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AffiliateClick>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = load_affiliate(&state, &path)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        let params = query.into_inner();
        let flagged_filter = params.filters.get("flagged").cloned();
        let link_filter = params.filters.get("link_id").cloned();
        let mut items: Vec<AffiliateClick> = state
            .clicks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|click| {
                click.affiliate_id == affiliate.id
                    && flagged_filter
                        .as_deref()
                        .is_none_or(|raw| click.flagged.to_string() == raw)
                    && link_filter
                        .as_deref()
                        .is_none_or(|raw| click.link_id.to_string() == raw)
            })
            .collect();
        items.sort_by(|a, b| b.clicked_at.cmp(&a.clicked_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn list_conversions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AffiliateConversion>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = load_affiliate(&state, &path)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let flagged_filter = params.filters.get("flagged").cloned();
        let mut items: Vec<AffiliateConversion> = state
            .conversions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|conversion| {
                conversion.affiliate_id == affiliate.id
                    && kind_filter
                        .as_deref()
                        .is_none_or(|raw| conversion.kind == raw)
                    && flagged_filter
                        .as_deref()
                        .is_none_or(|raw| conversion.flagged.to_string() == raw)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn create_conversion(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    body: web::Json<CreateConversionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliateConversion> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let input = body.into_inner();
        let affiliate = load_affiliate(&state, &input.affiliate_id)?;
        if affiliate.status != AffiliateStatus::Approved {
            return Err(UnifiedError::conflict(format!(
                "Affiliate is {} and cannot earn conversions",
                affiliate.status.as_str()
            )));
        }
        let kind = input.kind.trim().to_lowercase();
        if kind != "signup" && kind != "purchase" {
            return Err(UnifiedError::bad_request(
                "kind must be \"signup\" or \"purchase\"",
            ));
        }
        let customer_user_id =
            require_text(&input.customer_user_id, "customer_user_id", MAX_USER_ID_LEN)?;
        let link_id = match &input.link_id {
            Some(raw) => {
                let link = load_link(&state, raw)?;
                if link.affiliate_id != affiliate.id {
                    return Err(UnifiedError::bad_request(
                        "link_id does not belong to this affiliate",
                    ));
                }
                Some(link.id)
            }
            None => None,
        };
        let click_id = match &input.click_id {
            Some(raw) => {
                let id = parse_uuid(raw, "click")?;
                let click = state
                    .clicks
                    .get(&id)
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| UnifiedError::not_found(format!("Click not found: {id}")))?;
                if click.affiliate_id != affiliate.id {
                    return Err(UnifiedError::bad_request(
                        "click_id does not belong to this affiliate",
                    ));
                }
                Some(click.id)
            }
            None => None,
        };
        let order_id = match &input.order_id {
            Some(raw) => Some(require_text(raw, "order_id", MAX_NAME_LEN)?),
            None => None,
        };
        if kind == "purchase" {
            if let Some(order_id) = &order_id {
                if state.conversions.iter().any(|entry| {
                    entry.value().order_id.as_deref() == Some(order_id.as_str())
                        && entry.value().kind == "purchase"
                }) {
                    return Err(UnifiedError::conflict(format!(
                        "Order \"{order_id}\" already has a purchase conversion"
                    )));
                }
            }
        }
        let order_total = match input.order_total {
            Some(raw) => valid_price(raw)?,
            None => 0.0,
        };
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => affiliate.currency.clone(),
        };
        let product_id = match &input.product_id {
            Some(raw) => Some(require_text(raw, "product_id", MAX_PRODUCT_ID_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let (conversion, _) = record_conversion(
            &state,
            &affiliate,
            link_id,
            click_id,
            &customer_user_id,
            &kind,
            order_id,
            order_total,
            &currency,
            product_id,
            now,
        )?;
        Ok(conversion)
    })();
    match result {
        Ok(conversion) => created(conversion),
        Err(error) => err(error, debug),
    }
}

async fn list_commissions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AffiliateCommission>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = load_affiliate(&state, &path)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let flagged_filter = params.filters.get("flagged").cloned();
        let mut items: Vec<AffiliateCommission> = state
            .commissions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|commission| {
                commission.affiliate_id == affiliate.id
                    && status_filter
                        .as_deref()
                        .is_none_or(|raw| commission.status.as_str() == raw)
                    && flagged_filter
                        .as_deref()
                        .is_none_or(|raw| commission.flagged.to_string() == raw)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn approve_commission(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliateCommission> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let mut commission = load_commission(&state, &path)?;
        if commission.status != CommissionStatus::Pending {
            return Err(UnifiedError::conflict(format!(
                "Commission is {} and cannot be approved",
                commission.status.as_str()
            )));
        }
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?;
        commission.status = CommissionStatus::Approved;
        commission.reviewed_by = Some(caller.actor.clone());
        commission.review_reason = reason;
        commission.updated_at = Utc::now();
        state.commissions.insert(commission.id, commission.clone());
        state.emit(
            "affiliates.commission.approved",
            json!({
                "commission_id": commission.id,
                "affiliate_id": commission.affiliate_id,
                "amount": commission.amount,
                "currency": commission.currency,
                "reviewed_by": caller.actor,
            }),
        );
        Ok(commission)
    })();
    match result {
        Ok(commission) => ok(commission),
        Err(error) => err(error, debug),
    }
}

async fn void_commission(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliateCommission> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let mut commission = load_commission(&state, &path)?;
        if commission.status != CommissionStatus::Pending
            && commission.status != CommissionStatus::Approved
        {
            return Err(UnifiedError::conflict(format!(
                "Commission is {} and cannot be voided",
                commission.status.as_str()
            )));
        }
        if commission.payout_id.is_some() {
            return Err(UnifiedError::conflict(
                "Commission is attached to a payout request; reject the payout first to release it",
            ));
        }
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?;
        commission.status = CommissionStatus::Void;
        commission.reviewed_by = Some(caller.actor.clone());
        commission.review_reason = reason;
        commission.updated_at = Utc::now();
        state.commissions.insert(commission.id, commission.clone());
        state.emit(
            "affiliates.commission.voided",
            json!({
                "commission_id": commission.id,
                "affiliate_id": commission.affiliate_id,
                "amount": commission.amount,
                "currency": commission.currency,
                "reviewed_by": caller.actor,
            }),
        );
        Ok(commission)
    })();
    match result {
        Ok(commission) => ok(commission),
        Err(error) => err(error, debug),
    }
}

async fn list_payouts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AffiliatePayout>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = load_affiliate(&state, &path)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<AffiliatePayout> = state
            .payouts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|payout| {
                payout.affiliate_id == affiliate.id
                    && status_filter
                        .as_deref()
                        .is_none_or(|raw| payout.status.as_str() == raw)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn request_payout(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RequestPayoutRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliatePayout> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = load_affiliate(&state, &path)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        let input = body.into_inner();
        let mut commissions: Vec<AffiliateCommission> = match &input.commission_ids {
            Some(raw_ids) => {
                let mut found = Vec::new();
                for raw in raw_ids {
                    let commission = load_commission(&state, raw)?;
                    if commission.affiliate_id != affiliate.id {
                        return Err(UnifiedError::bad_request(
                            "One of the commission ids does not belong to this affiliate",
                        ));
                    }
                    found.push(commission);
                }
                found
            }
            None => state
                .commissions
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|commission| {
                    commission.affiliate_id == affiliate.id
                        && commission.status == CommissionStatus::Approved
                        && commission.payout_id.is_none()
                })
                .collect(),
        };
        commissions.retain(|commission| {
            commission.status == CommissionStatus::Approved && commission.payout_id.is_none()
        });
        if commissions.is_empty() {
            return Err(UnifiedError::bad_request(
                "No approved, unattached commissions are available for payout",
            ));
        }
        // All commissions in one payout must share a currency.
        let currency = commissions[0].currency.clone();
        if commissions.iter().any(|commission| commission.currency != currency) {
            return Err(UnifiedError::bad_request(
                "All commissions in one payout must share the same currency",
            ));
        }
        let total = round_money(commissions.iter().map(|commission| commission.amount).sum());
        let method = match &input.method {
            Some(raw) => Some(require_text(raw, "method", MAX_METHOD_LEN)?),
            None => affiliate.payout_method.clone(),
        };
        let now = Utc::now();
        let payout = AffiliatePayout {
            id: Uuid::new_v4(),
            affiliate_id: affiliate.id,
            commission_ids: commissions.iter().map(|commission| commission.id).collect(),
            total_amount: total,
            currency: currency.clone(),
            status: PayoutStatus::Requested,
            method,
            reference: None,
            requested_by: caller.actor.clone(),
            reviewed_by: None,
            review_reason: None,
            created_at: now,
            updated_at: now,
        };
        state.payouts.insert(payout.id, payout.clone());
        for commission in &commissions {
            if let Some(mut entry) = state.commissions.get_mut(&commission.id) {
                entry.payout_id = Some(payout.id);
                entry.updated_at = now;
            }
        }
        // Request only: the billing module owns the actual payment.
        state.emit(
            "affiliates.payout.requested",
            json!({
                "payout_id": payout.id,
                "affiliate_id": affiliate.id,
                "amount": payout.total_amount,
                "currency": currency,
                "method": payout.method,
                "commission_ids": payout.commission_ids,
                "requested_by": caller.actor,
            }),
        );
        Ok(payout)
    })();
    match result {
        Ok(payout) => created(payout),
        Err(error) => err(error, debug),
    }
}

/// Shared payout transition helper (approve / mark-paid / reject).
fn transition_payout(
    state: &AffiliatesState,
    caller: &Caller,
    raw_id: &str,
    from: &[PayoutStatus],
    target: PayoutStatus,
    event: &str,
    reason: Option<String>,
    reference: Option<String>,
) -> UnifiedResult<AffiliatePayout> {
    let mut payout = load_payout(state, raw_id)?;
    if !from.contains(&payout.status) {
        return Err(UnifiedError::conflict(format!(
            "Payout is {} and cannot move to {}",
            payout.status.as_str(),
            target.as_str()
        )));
    }
    let now = Utc::now();
    payout.status = target;
    payout.reviewed_by = Some(caller.actor.clone());
    payout.review_reason = reason;
    if let Some(reference) = reference {
        payout.reference = Some(reference);
    }
    payout.updated_at = now;
    match target {
        PayoutStatus::Paid => {
            for commission_id in &payout.commission_ids {
                if let Some(mut entry) = state.commissions.get_mut(commission_id) {
                    entry.status = CommissionStatus::Paid;
                    entry.updated_at = now;
                }
            }
        }
        PayoutStatus::Rejected => {
            // Release the commissions back to approved, unattached.
            for commission_id in &payout.commission_ids {
                if let Some(mut entry) = state.commissions.get_mut(commission_id) {
                    entry.payout_id = None;
                    entry.updated_at = now;
                }
            }
        }
        _ => {}
    }
    state.payouts.insert(payout.id, payout.clone());
    state.emit(
        event,
        json!({
            "payout_id": payout.id,
            "affiliate_id": payout.affiliate_id,
            "amount": payout.total_amount,
            "currency": payout.currency,
            "status": payout.status.as_str(),
            "reviewed_by": caller.actor,
        }),
    );
    Ok(payout)
}

async fn approve_payout(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliatePayout> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?;
        transition_payout(
            &state,
            &caller,
            &path,
            &[PayoutStatus::Requested],
            PayoutStatus::Approved,
            "affiliates.payout.approved",
            reason,
            None,
        )
    })();
    match result {
        Ok(payout) => ok(payout),
        Err(error) => err(error, debug),
    }
}

async fn mark_payout_paid(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MarkPaidRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliatePayout> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let reference = body
            .reference
            .as_deref()
            .map(|raw| require_text(raw, "reference", MAX_NAME_LEN))
            .transpose()?;
        transition_payout(
            &state,
            &caller,
            &path,
            &[PayoutStatus::Approved],
            PayoutStatus::Paid,
            "affiliates.payout.paid",
            None,
            reference,
        )
    })();
    match result {
        Ok(payout) => ok(payout),
        Err(error) => err(error, debug),
    }
}

async fn reject_payout(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliatePayout> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?;
        transition_payout(
            &state,
            &caller,
            &path,
            &[PayoutStatus::Requested, PayoutStatus::Approved],
            PayoutStatus::Rejected,
            "affiliates.payout.rejected",
            reason,
            None,
        )
    })();
    match result {
        Ok(payout) => ok(payout),
        Err(error) => err(error, debug),
    }
}

async fn get_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = load_affiliate(&state, &path)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        Ok(dashboard_for(&state, affiliate.id))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn my_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = load_affiliate_by_user(&state, &caller.actor)?;
        Ok(dashboard_for(&state, affiliate.id))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
