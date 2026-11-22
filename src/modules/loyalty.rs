// Loyalty / Rewards module — customer points programs, earn rules,
// tiers, a ledger-backed points balance, and redemptions (points
// to a discount on an order, or points to a product).
//
// Routes live under `/api/v1/loyalty`:
//
//   Programs:
//     GET/POST /programs — list programs / create a program.
//     GET/PATCH/DELETE /programs/{id}.
//     POST /programs/{id}/activate, /programs/{id}/deactivate.
//   Earn rules:
//     GET/POST /programs/{id}/earn-rules.
//     GET/PATCH/DELETE /programs/{id}/earn-rules/{rule_id}.
//   Tiers:
//     GET/POST /programs/{id}/tiers.
//     GET/PATCH/DELETE /programs/{id}/tiers/{tier_id}.
//   Points:
//     POST /programs/{id}/earn — award points (manage).
//     POST /programs/{id}/adjust — signed adjustment (manage).
//     POST /programs/{id}/expire — run the expiry sweep (manage).
//     GET  /programs/{id}/balance/{customer_id} — balance + tier.
//     GET  /programs/{id}/balance/me — the caller's own balance.
//     GET  /programs/{id}/tier/{customer_id} — tier evaluation.
//     GET  /programs/{id}/ledger — ledger rows (paginated).
//   Redemptions:
//     POST /programs/{id}/redeem — spend points (owner or manage).
//     POST /redemptions/{redemption_id}/cancel — refund (manage).
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory points programs (one active at a time per company),
// configurable earn rules (purchase amount -> points, referral,
// review, signup, each with optional daily / lifetime caps and a
// minimum order amount), tiers by lifetime points or spend with
// benefits, a points ledger in which EVERY movement is a row
// (earn / spend / expire / adjust, with reason, actor, and the
// balance after the movement), balances always computed from the
// ledger (never stored as a bare number), expiry as a ledger
// movement (never a silent delete), tier evaluation on read and
// after every earn, and hook listeners that turn
// `store.order.completed` into purchase points and `user.created`
// into signup / referral points. What is NOT built here: the
// store-side honouring of a redeemed discount — redemption emits
// `loyalty.discount.requested` (order id, discount amount,
// currency) for the store module, and the discount only takes
// effect once the store applies it; actual product fulfilment for
// points-to-product redemptions (the redemption row records the
// product; the store owns fulfilment); and any review earn
// automation — there is no review event to listen to yet, so
// review points are awarded through the manual earn endpoint.
//
// Staff recognition points live in the gamification module with a
// SEPARATE ledger. Customer loyalty points here must never mix
// with staff points: this module has no staff concept at all, its
// ledger rows are keyed by customer id only, and no gamification
// event is consumed or emitted here.
//
// Public-access note: this module has no public routes. Every
// route below checks a capability; an affiliate-style anonymous
// endpoint does not exist here.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
// A customer may only ever see their own balance, tier, and
// ledger rows: every customer-scoped read checks the manage
// capability OR that the requested customer id equals the caller,
// server-side, before any data is returned.
//
// Events emitted on the hook bus:
//   - `loyalty.program.created`, `loyalty.program.activated`,
//     `loyalty.program.deactivated`
//   - `loyalty.points.earned`, `loyalty.points.spent`,
//     `loyalty.points.expired`, `loyalty.points.adjusted`
//   - `loyalty.tier.changed`
//   - `loyalty.redemption.completed`, `loyalty.redemption.cancelled`
//   - `loyalty.discount.requested` (payload carries `order_id`,
//     `discount_amount`, `currency`, `customer_id`, and
//     `redemption_id` for the store module — the store owns
//     applying the discount to the order)
// Events this module listens to:
//   - `store.order.completed` — purchase earn under the active
//     program of the order's company (or the default program when
//     the order carries no company), using the active purchase
//     earn rule, its caps, and its minimum order amount.
//   - `user.created` — signup earn for the new customer, and a
//     referral earn for the referrer when the payload carries
//     `referred_by` (a customer user id).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_REASON_LEN: usize = 1000;
const MAX_USER_ID_LEN: usize = 128;
const MAX_COMPANY_ID_LEN: usize = 128;
const MAX_PRODUCT_ID_LEN: usize = 128;
const MAX_ORDER_ID_LEN: usize = 200;
const MAX_POINTS_NAME_LEN: usize = 64;
/// Default currency value of one point when a program does not set
/// its own conversion rate.
const DEFAULT_DISCOUNT_PER_POINT: f64 = 0.01;

/// Capability names checked through the `role.check` hook.
const PERM_LOYALTY_VIEW: &str = "loyalty.view";
const PERM_LOYALTY_MANAGE: &str = "loyalty.manage";

/// Hook event names this module listens to (literals repeated here
/// so the module stays decoupled from the users / store modules).
const USER_CREATED: &str = "user.created";
const STORE_ORDER_COMPLETED: &str = "store.order.completed";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// How points expire. `fixed` expires each earn after
/// `expiry_days` from the earn date; `inactivity` expires the
/// whole balance after `expiry_days` without any ledger activity.
/// Expiry is always a ledger movement, never a silent delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpiryMode {
    Fixed,
    Inactivity,
}

impl ExpiryMode {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Inactivity => "inactivity",
        }
    }
}

/// What earns points. `purchase` awards per currency unit spent,
/// the other kinds award a fixed amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EarnRuleKind {
    Purchase,
    Referral,
    Review,
    Signup,
}

impl EarnRuleKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Purchase => "purchase",
            Self::Referral => "referral",
            Self::Review => "review",
            Self::Signup => "signup",
        }
    }
}

/// Direction of a ledger movement. `earn` and `adjust` may be
/// positive or negative; `spend` and `expire` are always negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerKind {
    Earn,
    Spend,
    Expire,
    Adjust,
}

impl LedgerKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Earn => "earn",
            Self::Spend => "spend",
            Self::Expire => "expire",
            Self::Adjust => "adjust",
        }
    }
}

/// One points program. `company_id` is `None` for the default
// program (used when an order or signup carries no company).
/// At most one program is active per company. `expiry_days` is
/// `None` when points in this program never expire.
/// `discount_per_point` converts points to currency on redemption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoyaltyProgram {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Display name of the points unit (for example "points").
    pub points_name: String,
    pub currency: String,
    /// Currency value of one point on redemption.
    pub discount_per_point: f64,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry_days: Option<i64>,
    pub expiry_mode: ExpiryMode,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One earn rule of a program. `points_per_unit` is points per one
/// unit of currency for purchase rules; `fixed_points` is the flat
/// award for referral / review / signup rules (and the fallback
/// for a purchase rule without a per-unit rate). Caps are enforced
/// per customer: `cap_per_day` bounds earns of this kind per
/// calendar day (UTC), `cap_total` bounds them over all time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoyaltyEarnRule {
    pub id: Uuid,
    pub program_id: Uuid,
    pub kind: EarnRuleKind,
    pub points_per_unit: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_points: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap_per_day: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap_total: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_order_amount: Option<f64>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One tier of a program. A customer qualifies when their lifetime
/// earned points reach `min_lifetime_points` AND their lifetime
/// spend reaches `min_lifetime_spend`. The highest qualifying tier
/// (by `sort_order`, then thresholds) is the customer's tier.
/// `benefits` are free-form benefit descriptions; `discount_percent`
/// is the tier discount the store may apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoyaltyTier {
    pub id: Uuid,
    pub program_id: Uuid,
    pub name: String,
    pub min_lifetime_points: i64,
    pub min_lifetime_spend: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub benefits: Vec<String>,
    pub discount_percent: f64,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One ledger row. EVERY points movement is a row: `earn`,
/// `spend`, `expire`, or `adjust`, with a signed `points` delta,
/// the reason, the actor who caused it, and the balance after the
/// movement. Balances are always computed by summing these rows,
/// never stored as a bare number. `earn_kind` records which earn
/// rule kind produced an earn row (used for caps and lifetime
/// spend); `expires_at` is set on earn rows when the program has
/// fixed expiry; `reference_id` is the order / referral id the
/// movement relates to; `reference_amount` is the order total for
/// purchase earns (used for lifetime spend).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoyaltyLedgerEntry {
    pub id: Uuid,
    pub program_id: Uuid,
    pub customer_id: String,
    pub kind: LedgerKind,
    /// Signed points delta (negative for spend / expire).
    pub points: i64,
    /// Balance after this movement (sum of all rows so far).
    pub balance_after: i64,
    pub reason: String,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub earn_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_amount: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// One redemption: points spent for a discount on an order or for a
/// product. `discount_amount` is computed from the program's
/// `discount_per_point` for discount redemptions. A cancelled
/// redemption refunds its points through a new ledger row (the
/// original spend row is never rewritten).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoyaltyRedemption {
    pub id: Uuid,
    pub program_id: Uuid,
    pub customer_id: String,
    pub points_spent: i64,
    /// `discount` or `product`.
    pub kind: String,
    /// Discount value for discount redemptions, 0 for products.
    /// Money: NUMERIC(19,4) in the migration.
    pub discount_amount: f64,
    pub currency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// `completed` or `cancelled`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProgramRequest {
    #[serde(default)]
    pub company_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub points_name: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub discount_per_point: Option<f64>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub expiry_days: Option<i64>,
    #[serde(default)]
    pub expiry_mode: Option<ExpiryMode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProgramRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub points_name: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub discount_per_point: Option<f64>,
    #[serde(default)]
    pub expiry_days: Option<i64>,
    #[serde(default)]
    pub expiry_mode: Option<ExpiryMode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateEarnRuleRequest {
    pub kind: EarnRuleKind,
    #[serde(default)]
    pub points_per_unit: Option<f64>,
    #[serde(default)]
    pub fixed_points: Option<i64>,
    #[serde(default)]
    pub cap_per_day: Option<i64>,
    #[serde(default)]
    pub cap_total: Option<i64>,
    #[serde(default)]
    pub min_order_amount: Option<f64>,
    #[serde(default)]
    pub active: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateEarnRuleRequest {
    #[serde(default)]
    pub points_per_unit: Option<f64>,
    #[serde(default)]
    pub fixed_points: Option<i64>,
    #[serde(default)]
    pub cap_per_day: Option<i64>,
    #[serde(default)]
    pub cap_total: Option<i64>,
    #[serde(default)]
    pub min_order_amount: Option<f64>,
    #[serde(default)]
    pub active: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTierRequest {
    pub name: String,
    #[serde(default)]
    pub min_lifetime_points: Option<i64>,
    #[serde(default)]
    pub min_lifetime_spend: Option<f64>,
    #[serde(default)]
    pub benefits: Option<Vec<String>>,
    #[serde(default)]
    pub discount_percent: Option<f64>,
    #[serde(default)]
    pub sort_order: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTierRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub min_lifetime_points: Option<i64>,
    #[serde(default)]
    pub min_lifetime_spend: Option<f64>,
    #[serde(default)]
    pub benefits: Option<Vec<String>>,
    #[serde(default)]
    pub discount_percent: Option<f64>,
    #[serde(default)]
    pub sort_order: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EarnPointsRequest {
    pub customer_id: String,
    pub points: i64,
    pub reason: String,
    #[serde(default)]
    pub reference_id: Option<String>,
    #[serde(default)]
    pub earn_kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdjustPointsRequest {
    pub customer_id: String,
    /// Signed delta: positive adds points, negative removes them.
    pub points: i64,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RedeemRequest {
    #[serde(default)]
    pub customer_id: Option<String>,
    pub points: i64,
    pub kind: String,
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub product_id: Option<String>,
}

/// In-memory stores for loyalty. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000045_loyalty`; until that migration is executed,
/// loyalty data does not survive a restart.
#[derive(Clone)]
pub struct LoyaltyState {
    pub programs: Arc<DashMap<Uuid, LoyaltyProgram>>,
    pub earn_rules: Arc<DashMap<Uuid, LoyaltyEarnRule>>,
    pub tiers: Arc<DashMap<Uuid, LoyaltyTier>>,
    pub ledger: Arc<DashMap<Uuid, LoyaltyLedgerEntry>>,
    pub redemptions: Arc<DashMap<Uuid, LoyaltyRedemption>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for LoyaltyState {
    fn default() -> Self {
        Self {
            programs: Arc::new(DashMap::new()),
            earn_rules: Arc::new(DashMap::new()),
            tiers: Arc::new(DashMap::new()),
            ledger: Arc::new(DashMap::new()),
            redemptions: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl LoyaltyState {
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

fn is_manager(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_LOYALTY_MANAGE)
}

/// Customer-scoped access: a manager may see any customer; any
/// other caller may only see their own customer id. Enforced
/// server-side on every customer-scoped read.
fn require_customer_access(ctx: &AppContext, caller: &Caller, customer_id: &str) -> UnifiedResult<()> {
    if is_manager(ctx, &caller.role) || caller.actor == customer_id {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "A customer may only access their own loyalty records",
        ))
    }
}

fn load_program(state: &LoyaltyState, raw_id: &str) -> UnifiedResult<LoyaltyProgram> {
    let id = parse_uuid(raw_id, "loyalty program")?;
    state
        .programs
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Loyalty program not found: {id}")))
}

fn load_earn_rule(
    state: &LoyaltyState,
    program_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<LoyaltyEarnRule> {
    let id = parse_uuid(raw_id, "earn rule")?;
    state
        .earn_rules
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|rule| rule.program_id == program_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Earn rule not found: {id}")))
}

fn load_tier(state: &LoyaltyState, program_id: Uuid, raw_id: &str) -> UnifiedResult<LoyaltyTier> {
    let id = parse_uuid(raw_id, "loyalty tier")?;
    state
        .tiers
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|tier| tier.program_id == program_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Loyalty tier not found: {id}")))
}

fn load_redemption(state: &LoyaltyState, raw_id: &str) -> UnifiedResult<LoyaltyRedemption> {
    let id = parse_uuid(raw_id, "redemption")?;
    state
        .redemptions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Redemption not found: {id}")))
}

/// The active program for a company, falling back to the active
/// default program (no company) when the company has none. `None`
/// when no program matches at all.
fn active_program_for(state: &LoyaltyState, company_id: Option<&str>) -> Option<LoyaltyProgram> {
    if let Some(company_id) = company_id {
        if let Some(program) = state
            .programs
            .iter()
            .map(|entry| entry.value().clone())
            .find(|program| program.active && program.company_id.as_deref() == Some(company_id))
        {
            return Some(program);
        }
    }
    state
        .programs
        .iter()
        .map(|entry| entry.value().clone())
        .find(|program| program.active && program.company_id.is_none())
}

/// Balance of one customer in one program, computed from the
/// ledger (sum of all signed deltas). Never stored anywhere else.
fn balance(state: &LoyaltyState, program_id: Uuid, customer_id: &str) -> i64 {
    state
        .ledger
        .iter()
        .filter(|entry| {
            entry.value().program_id == program_id && entry.value().customer_id == customer_id
        })
        .map(|entry| entry.value().points)
        .sum()
}

/// Lifetime earned points (sum of positive earn rows) of one
/// customer in one program.
fn lifetime_points(state: &LoyaltyState, program_id: Uuid, customer_id: &str) -> i64 {
    state
        .ledger
        .iter()
        .filter(|entry| {
            entry.value().program_id == program_id
                && entry.value().customer_id == customer_id
                && entry.value().kind == LedgerKind::Earn
        })
        .map(|entry| entry.value().points.max(0))
        .sum()
}

/// Lifetime spend (sum of order totals on purchase earn rows) of
/// one customer in one program.
fn lifetime_spend(state: &LoyaltyState, program_id: Uuid, customer_id: &str) -> f64 {
    state
        .ledger
        .iter()
        .filter(|entry| {
            entry.value().program_id == program_id
                && entry.value().customer_id == customer_id
                && entry.value().kind == LedgerKind::Earn
                && entry.value().earn_kind.as_deref() == Some("purchase")
        })
        .map(|entry| entry.value().reference_amount.unwrap_or(0.0))
        .sum()
}

/// The customer's tier: the highest qualifying tier (by
/// `sort_order`, then thresholds). `None` when no tier qualifies.
fn tier_for(state: &LoyaltyState, program_id: Uuid, customer_id: &str) -> Option<LoyaltyTier> {
    let points = lifetime_points(state, program_id, customer_id);
    let spend = lifetime_spend(state, program_id, customer_id);
    let mut tiers: Vec<LoyaltyTier> = state
        .tiers
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|tier| {
            tier.program_id == program_id
                && points >= tier.min_lifetime_points
                && spend >= tier.min_lifetime_spend
        })
        .collect();
    tiers.sort_by(|a, b| {
        b.sort_order
            .cmp(&a.sort_order)
            .then(b.min_lifetime_points.cmp(&a.min_lifetime_points))
            .then(b.min_lifetime_spend.partial_cmp(&a.min_lifetime_spend).unwrap_or(std::cmp::Ordering::Equal))
    });
    tiers.into_iter().next()
}

/// Balance + tier summary for one customer, as returned by the
/// balance endpoints.
fn balance_view(state: &LoyaltyState, program: &LoyaltyProgram, customer_id: &str) -> Value {
    let tier = tier_for(state, program.id, customer_id);
    json!({
        "program_id": program.id,
        "customer_id": customer_id,
        "balance": balance(state, program.id, customer_id),
        "lifetime_points": lifetime_points(state, program.id, customer_id),
        "lifetime_spend": round_money(lifetime_spend(state, program.id, customer_id)),
        "points_name": program.points_name,
        "currency": program.currency,
        "tier": tier,
    })
}

/// Append one ledger row. The balance after the movement is
/// computed from the ledger, and a movement that would drive the
/// balance below zero is refused (points can never go negative).
#[allow(clippy::too_many_arguments)]
fn append_ledger(
    state: &LoyaltyState,
    program: &LoyaltyProgram,
    customer_id: &str,
    kind: LedgerKind,
    points: i64,
    reason: &str,
    actor: &str,
    earn_kind: Option<String>,
    reference_id: Option<String>,
    reference_amount: Option<f64>,
    now: DateTime<Utc>,
) -> UnifiedResult<LoyaltyLedgerEntry> {
    if points == 0 {
        return Err(UnifiedError::bad_request("points must not be zero"));
    }
    let current = balance(state, program.id, customer_id);
    let after = current + points;
    if after < 0 {
        return Err(UnifiedError::bad_request(format!(
            "Insufficient points: balance is {current} and the movement is {points}"
        )));
    }
    let expires_at = match (kind, program.expiry_mode, program.expiry_days) {
        (LedgerKind::Earn, ExpiryMode::Fixed, Some(days)) => Some(now + Duration::days(days)),
        _ => None,
    };
    let entry = LoyaltyLedgerEntry {
        id: Uuid::new_v4(),
        program_id: program.id,
        customer_id: customer_id.to_string(),
        kind,
        points,
        balance_after: after,
        reason: reason.to_string(),
        actor: actor.to_string(),
        earn_kind,
        reference_id,
        reference_amount,
        expires_at,
        created_at: now,
    };
    state.ledger.insert(entry.id, entry.clone());
    Ok(entry)
}

/// Award points under an earn rule, applying its caps. Returns the
/// ledger entry, or `None` when the caps leave nothing to award
/// (the caller reports that as a skip, not an error). Tier changes
/// are emitted when the award moves the customer to another tier.
fn earn_under_rule(
    state: &LoyaltyState,
    program: &LoyaltyProgram,
    rule: &LoyaltyEarnRule,
    customer_id: &str,
    mut points: i64,
    reason: &str,
    actor: &str,
    reference_id: Option<String>,
    reference_amount: Option<f64>,
    now: DateTime<Utc>,
) -> UnifiedResult<Option<LoyaltyLedgerEntry>> {
    if points <= 0 {
        return Ok(None);
    }
    let kind_name = rule.kind.as_str();
    // Daily cap: earns of this kind today (UTC calendar day).
    if let Some(cap) = rule.cap_per_day {
        let earned_today: i64 = state
            .ledger
            .iter()
            .filter(|entry| {
                let value = entry.value();
                value.program_id == program.id
                    && value.customer_id == customer_id
                    && value.kind == LedgerKind::Earn
                    && value.earn_kind.as_deref() == Some(kind_name)
                    && value.created_at.date_naive() == now.date_naive()
            })
            .map(|entry| entry.value().points.max(0))
            .sum();
        let remaining = cap - earned_today;
        if remaining <= 0 {
            return Ok(None);
        }
        points = points.min(remaining);
    }
    // Lifetime cap: earns of this kind over all time.
    if let Some(cap) = rule.cap_total {
        let earned_total: i64 = state
            .ledger
            .iter()
            .filter(|entry| {
                let value = entry.value();
                value.program_id == program.id
                    && value.customer_id == customer_id
                    && value.kind == LedgerKind::Earn
                    && value.earn_kind.as_deref() == Some(kind_name)
            })
            .map(|entry| entry.value().points.max(0))
            .sum();
        let remaining = cap - earned_total;
        if remaining <= 0 {
            return Ok(None);
        }
        points = points.min(remaining);
    }
    if points <= 0 {
        return Ok(None);
    }
    let tier_before = tier_for(state, program.id, customer_id).map(|tier| tier.id);
    let entry = append_ledger(
        state,
        program,
        customer_id,
        LedgerKind::Earn,
        points,
        reason,
        actor,
        Some(kind_name.to_string()),
        reference_id,
        reference_amount,
        now,
    )?;
    state.emit(
        "loyalty.points.earned",
        json!({
            "program_id": program.id,
            "customer_id": customer_id,
            "points": points,
            "balance_after": entry.balance_after,
            "earn_kind": kind_name,
            "reason": reason,
        }),
    );
    let tier_after = tier_for(state, program.id, customer_id).map(|tier| tier.id);
    if tier_before != tier_after {
        state.emit(
            "loyalty.tier.changed",
            json!({
                "program_id": program.id,
                "customer_id": customer_id,
                "previous_tier_id": tier_before,
                "tier_id": tier_after,
            }),
        );
    }
    Ok(Some(entry))
}

/// Expiry sweep for one program. Fixed mode expires the aggregate
/// of earn rows whose `expires_at` has passed, minus amounts
/// already expired, capped at the current balance. Inactivity mode
/// expires the whole balance when the customer has had no ledger
/// activity for `expiry_days`. Every expiry is a ledger row; the
/// function returns how many customers were expired and how many
/// points in total.
fn expire_program(state: &LoyaltyState, program: &LoyaltyProgram, actor: &str, now: DateTime<Utc>) -> (usize, i64) {
    let Some(days) = program.expiry_days else {
        return (0, 0);
    };
    let mut customers: Vec<String> = state
        .ledger
        .iter()
        .filter(|entry| entry.value().program_id == program.id)
        .map(|entry| entry.value().customer_id.clone())
        .collect();
    customers.sort();
    customers.dedup();
    let mut expired_customers = 0usize;
    let mut expired_points = 0i64;
    for customer_id in customers {
        let current = balance(state, program.id, &customer_id);
        if current <= 0 {
            continue;
        }
        let amount = match program.expiry_mode {
            ExpiryMode::Fixed => {
                let expired_earns: i64 = state
                    .ledger
                    .iter()
                    .filter(|entry| {
                        let value = entry.value();
                        value.program_id == program.id
                            && value.customer_id == customer_id
                            && value.kind == LedgerKind::Earn
                            && value
                                .expires_at
                                .is_some_and(|expires_at| expires_at <= now)
                    })
                    .map(|entry| entry.value().points.max(0))
                    .sum();
                let already_expired: i64 = state
                    .ledger
                    .iter()
                    .filter(|entry| {
                        let value = entry.value();
                        value.program_id == program.id
                            && value.customer_id == customer_id
                            && value.kind == LedgerKind::Expire
                    })
                    .map(|entry| entry.value().points.min(0).abs())
                    .sum();
                (expired_earns - already_expired).clamp(0, current)
            }
            ExpiryMode::Inactivity => {
                let last_activity = state
                    .ledger
                    .iter()
                    .filter(|entry| {
                        entry.value().program_id == program.id
                            && entry.value().customer_id == customer_id
                    })
                    .map(|entry| entry.value().created_at)
                    .max();
                match last_activity {
                    Some(last) if now - last > Duration::days(days) => current,
                    _ => 0,
                }
            }
        };
        if amount <= 0 {
            continue;
        }
        let tier_before = tier_for(state, program.id, &customer_id).map(|tier| tier.id);
        let reason = match program.expiry_mode {
            ExpiryMode::Fixed => format!("points expired after {days} days"),
            ExpiryMode::Inactivity => format!("points expired after {days} days of inactivity"),
        };
        if append_ledger(
            state,
            program,
            &customer_id,
            LedgerKind::Expire,
            -amount,
            &reason,
            actor,
            None,
            None,
            None,
            now,
        )
        .is_ok()
        {
            expired_customers += 1;
            expired_points += amount;
            state.emit(
                "loyalty.points.expired",
                json!({
                    "program_id": program.id,
                    "customer_id": customer_id,
                    "points": amount,
                    "expiry_mode": program.expiry_mode.as_str(),
                }),
            );
            let tier_after = tier_for(state, program.id, &customer_id).map(|tier| tier.id);
            if tier_before != tier_after {
                state.emit(
                    "loyalty.tier.changed",
                    json!({
                        "program_id": program.id,
                        "customer_id": customer_id,
                        "previous_tier_id": tier_before,
                        "tier_id": tier_after,
                    }),
                );
            }
        }
    }
    (expired_customers, expired_points)
}

// ---------------------------------------------------------------------------
// Hook listeners
// ---------------------------------------------------------------------------

/// `store.order.completed` -> purchase earn. The order payload is
/// the store `Order` JSON (`id`, `user_id`, `total`, `currency`,
/// `company_id` when present). The active program of the order's
/// company is used, or the default program when the order carries
/// no company. An order that already earned points (duplicate
/// emit) earns nothing twice: the order id is matched against
/// purchase earn rows.
fn on_store_order_completed(state: Arc<LoyaltyState>, ctx: &mut HookContext) {
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
            "loyalty": "skipped",
            "reason": "store.order.completed carries no order id or user id",
        }));
        return;
    };
    if state.ledger.iter().any(|entry| {
        let value = entry.value();
        value.kind == LedgerKind::Earn
            && value.earn_kind.as_deref() == Some("purchase")
            && value.reference_id.as_deref() == Some(order_id.as_str())
    }) {
        ctx.add_result(json!({
            "loyalty": "already_earned",
            "order_id": order_id,
        }));
        return;
    }
    let company_id = order
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Some(program) = active_program_for(&state, company_id.as_deref()) else {
        ctx.add_result(json!({
            "loyalty": "skipped",
            "order_id": order_id,
            "reason": "no active loyalty program matches this order",
        }));
        return;
    };
    let Some(rule) = state
        .earn_rules
        .iter()
        .map(|entry| entry.value().clone())
        .find(|rule| rule.program_id == program.id && rule.kind == EarnRuleKind::Purchase && rule.active)
    else {
        ctx.add_result(json!({
            "loyalty": "skipped",
            "order_id": order_id,
            "reason": "no active purchase earn rule on this program",
        }));
        return;
    };
    let total = order.get("total").and_then(|value| value.as_f64()).unwrap_or(0.0);
    if let Some(minimum) = rule.min_order_amount {
        if total < minimum {
            ctx.add_result(json!({
                "loyalty": "skipped",
                "order_id": order_id,
                "reason": format!("order total {total} is below the minimum {minimum}"),
            }));
            return;
        }
    }
    let points = match rule.fixed_points {
        Some(fixed) => fixed,
        None => (total * rule.points_per_unit).floor() as i64,
    };
    let now = Utc::now();
    match earn_under_rule(
        &state,
        &program,
        &rule,
        &customer_id,
        points,
        &format!("purchase {order_id}"),
        "system",
        Some(order_id.clone()),
        Some(round_money(total)),
        now,
    ) {
        Ok(Some(entry)) => {
            ctx.add_result(json!({
                "loyalty": "points_earned",
                "order_id": order_id,
                "points": entry.points,
                "balance_after": entry.balance_after,
            }));
        }
        Ok(None) => {
            ctx.add_result(json!({
                "loyalty": "skipped",
                "order_id": order_id,
                "reason": "earn rule caps leave nothing to award",
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "loyalty": "skipped",
                "order_id": order_id,
                "reason": error.message,
            }));
        }
    }
}

/// `user.created` -> signup earn for the new customer, plus a
/// referral earn for the referrer when the payload carries
/// `referred_by` (a customer user id). The customer id is read
/// from `id` / `user_id` / `user.id`; the company from
/// `company_id` when present.
fn on_user_created(state: Arc<LoyaltyState>, ctx: &mut HookContext) {
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
            "loyalty": "skipped",
            "reason": "user.created carries no customer id",
        }));
        return;
    };
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Some(program) = active_program_for(&state, company_id.as_deref()) else {
        ctx.add_result(json!({
            "loyalty": "skipped",
            "reason": "no active loyalty program matches this signup",
        }));
        return;
    };
    let now = Utc::now();
    let mut results: Vec<Value> = Vec::new();
    // Signup earn for the new customer.
    if let Some(rule) = state
        .earn_rules
        .iter()
        .map(|entry| entry.value().clone())
        .find(|rule| rule.program_id == program.id && rule.kind == EarnRuleKind::Signup && rule.active)
    {
        let points = rule.fixed_points.unwrap_or_else(|| rule.points_per_unit as i64);
        match earn_under_rule(
            &state,
            &program,
            &rule,
            &customer_id,
            points,
            "signup",
            "system",
            Some(customer_id.clone()),
            None,
            now,
        ) {
            Ok(Some(entry)) => results.push(json!({
                "loyalty": "points_earned",
                "earn_kind": "signup",
                "customer_id": customer_id,
                "points": entry.points,
            })),
            Ok(None) => results.push(json!({
                "loyalty": "skipped",
                "earn_kind": "signup",
                "reason": "earn rule caps leave nothing to award",
            })),
            Err(error) => results.push(json!({
                "loyalty": "skipped",
                "earn_kind": "signup",
                "reason": error.message,
            })),
        }
    }
    // Referral earn for the referrer.
    if let Some(referrer_id) = ctx
        .get("referred_by")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    {
        if referrer_id != customer_id {
            if let Some(rule) = state
                .earn_rules
                .iter()
                .map(|entry| entry.value().clone())
                .find(|rule| {
                    rule.program_id == program.id
                        && rule.kind == EarnRuleKind::Referral
                        && rule.active
                })
            {
                let points = rule.fixed_points.unwrap_or_else(|| rule.points_per_unit as i64);
                match earn_under_rule(
                    &state,
                    &program,
                    &rule,
                    &referrer_id,
                    points,
                    &format!("referral signup {customer_id}"),
                    "system",
                    Some(customer_id.clone()),
                    None,
                    now,
                ) {
                    Ok(Some(entry)) => results.push(json!({
                        "loyalty": "points_earned",
                        "earn_kind": "referral",
                        "customer_id": referrer_id,
                        "points": entry.points,
                    })),
                    Ok(None) => results.push(json!({
                        "loyalty": "skipped",
                        "earn_kind": "referral",
                        "reason": "earn rule caps leave nothing to award",
                    })),
                    Err(error) => results.push(json!({
                        "loyalty": "skipped",
                        "earn_kind": "referral",
                        "reason": error.message,
                    })),
                }
            }
        }
    }
    for result in results {
        ctx.add_result(result);
    }
    if ctx.results.is_empty() {
        ctx.add_result(json!({
            "loyalty": "skipped",
            "reason": "no active signup or referral earn rule on this program",
        }));
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct LoyaltyModule {
    state: LoyaltyState,
}

impl LoyaltyModule {
    pub fn new() -> Self {
        Self {
            state: LoyaltyState::default(),
        }
    }
}

impl Default for LoyaltyModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for LoyaltyModule {
    fn id(&self) -> &'static str {
        "loyalty"
    }

    fn name(&self) -> &'static str {
        "Loyalty / Rewards"
    }

    fn description(&self) -> &'static str {
        "Customer loyalty and rewards: points programs with earn rules for purchases, referrals, reviews, and signups, tiers by lifetime points or spend, a ledger-backed points balance where every movement is a row, expiry as a ledger movement, and redemptions for order discounts or products. Staff recognition points live in the gamification module with a separate ledger and never mix with these customer points."
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
        &["users", "roles", "store"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["dashmap", "serde_json"]
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
            STORE_ORDER_COMPLETED,
            "loyalty",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_completed(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            USER_CREATED,
            "loyalty",
            Arc::new(move |ctx: &mut HookContext| {
                on_user_created(Arc::clone(&state), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/loyalty")
                .app_data(state)
                .route("/programs", web::get().to(list_programs))
                .route("/programs", web::post().to(create_program))
                .route("/redemptions/{redemption_id}/cancel", web::post().to(cancel_redemption))
                .route("/programs/{id}", web::get().to(get_program))
                .route("/programs/{id}", web::patch().to(update_program))
                .route("/programs/{id}", web::delete().to(delete_program))
                .route("/programs/{id}/activate", web::post().to(activate_program))
                .route("/programs/{id}/deactivate", web::post().to(deactivate_program))
                .route("/programs/{id}/earn-rules", web::get().to(list_earn_rules))
                .route("/programs/{id}/earn-rules", web::post().to(create_earn_rule))
                .route("/programs/{id}/earn-rules/{rule_id}", web::get().to(get_earn_rule))
                .route("/programs/{id}/earn-rules/{rule_id}", web::patch().to(update_earn_rule))
                .route("/programs/{id}/earn-rules/{rule_id}", web::delete().to(delete_earn_rule))
                .route("/programs/{id}/tiers", web::get().to(list_tiers))
                .route("/programs/{id}/tiers", web::post().to(create_tier))
                .route("/programs/{id}/tiers/{tier_id}", web::get().to(get_tier))
                .route("/programs/{id}/tiers/{tier_id}", web::patch().to(update_tier))
                .route("/programs/{id}/tiers/{tier_id}", web::delete().to(delete_tier))
                .route("/programs/{id}/earn", web::post().to(earn_points))
                .route("/programs/{id}/adjust", web::post().to(adjust_points))
                .route("/programs/{id}/expire", web::post().to(expire_points))
                .route("/programs/{id}/balance/me", web::get().to(my_balance))
                .route("/programs/{id}/balance/{customer_id}", web::get().to(get_balance))
                .route("/programs/{id}/tier/{customer_id}", web::get().to(get_tier_for_customer))
                .route("/programs/{id}/ledger", web::get().to(list_ledger))
                .route("/programs/{id}/redeem", web::post().to(redeem_points)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/loyalty/programs", owner: "loyalty", description: "List loyalty programs (paginated, filter by company_id, active)." },
            RouteInfo { method: "POST", path: "/api/v1/loyalty/programs", owner: "loyalty", description: "Create a loyalty program (manage)." },
            RouteInfo { method: "GET", path: "/api/v1/loyalty/programs/{id}", owner: "loyalty", description: "Get one loyalty program." },
            RouteInfo { method: "PATCH", path: "/api/v1/loyalty/programs/{id}", owner: "loyalty", description: "Update a loyalty program (manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/loyalty/programs/{id}", owner: "loyalty", description: "Delete a loyalty program with no ledger rows (manage)." },
            RouteInfo { method: "POST", path: "/api/v1/loyalty/programs/{id}/activate", owner: "loyalty", description: "Activate a program (deactivates the other active program of the same company; manage)." },
            RouteInfo { method: "POST", path: "/api/v1/loyalty/programs/{id}/deactivate", owner: "loyalty", description: "Deactivate a program (manage)." },
            RouteInfo { method: "GET", path: "/api/v1/loyalty/programs/{id}/earn-rules", owner: "loyalty", description: "List earn rules of a program (paginated, filter kind, active)." },
            RouteInfo { method: "POST", path: "/api/v1/loyalty/programs/{id}/earn-rules", owner: "loyalty", description: "Create an earn rule (manage; one active rule per kind)." },
            RouteInfo { method: "GET", path: "/api/v1/loyalty/programs/{id}/earn-rules/{rule_id}", owner: "loyalty", description: "Get one earn rule." },
            RouteInfo { method: "PATCH", path: "/api/v1/loyalty/programs/{id}/earn-rules/{rule_id}", owner: "loyalty", description: "Update an earn rule (manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/loyalty/programs/{id}/earn-rules/{rule_id}", owner: "loyalty", description: "Delete an earn rule (manage)." },
            RouteInfo { method: "GET", path: "/api/v1/loyalty/programs/{id}/tiers", owner: "loyalty", description: "List tiers of a program (paginated, ordered by sort_order)." },
            RouteInfo { method: "POST", path: "/api/v1/loyalty/programs/{id}/tiers", owner: "loyalty", description: "Create a tier (manage)." },
            RouteInfo { method: "GET", path: "/api/v1/loyalty/programs/{id}/tiers/{tier_id}", owner: "loyalty", description: "Get one tier." },
            RouteInfo { method: "PATCH", path: "/api/v1/loyalty/programs/{id}/tiers/{tier_id}", owner: "loyalty", description: "Update a tier (manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/loyalty/programs/{id}/tiers/{tier_id}", owner: "loyalty", description: "Delete a tier (manage)." },
            RouteInfo { method: "POST", path: "/api/v1/loyalty/programs/{id}/earn", owner: "loyalty", description: "Award points to a customer (manage; creates a ledger earn row)." },
            RouteInfo { method: "POST", path: "/api/v1/loyalty/programs/{id}/adjust", owner: "loyalty", description: "Signed points adjustment for a customer (manage; creates a ledger adjust row)." },
            RouteInfo { method: "POST", path: "/api/v1/loyalty/programs/{id}/expire", owner: "loyalty", description: "Run the expiry sweep for a program (manage; expiry is a ledger row, never a delete)." },
            RouteInfo { method: "GET", path: "/api/v1/loyalty/programs/{id}/balance/{customer_id}", owner: "loyalty", description: "Balance, lifetime points, lifetime spend, and tier of a customer (manager or the customer)." },
            RouteInfo { method: "GET", path: "/api/v1/loyalty/programs/{id}/balance/me", owner: "loyalty", description: "The caller's own balance and tier in a program." },
            RouteInfo { method: "GET", path: "/api/v1/loyalty/programs/{id}/tier/{customer_id}", owner: "loyalty", description: "Tier evaluation of a customer (manager or the customer)." },
            RouteInfo { method: "GET", path: "/api/v1/loyalty/programs/{id}/ledger", owner: "loyalty", description: "Ledger rows of a program (paginated, filter customer_id, kind, earn_kind; a customer sees only their own rows)." },
            RouteInfo { method: "POST", path: "/api/v1/loyalty/programs/{id}/redeem", owner: "loyalty", description: "Redeem points for an order discount or a product (manager or the customer; emits loyalty.discount.requested for the store)." },
            RouteInfo { method: "POST", path: "/api/v1/loyalty/redemptions/{redemption_id}/cancel", owner: "loyalty", description: "Cancel a redemption and refund its points through a ledger row (manage)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(LoyaltyModule::new()));
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_programs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<LoyaltyProgram>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let active_filter = params.filters.get("active").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<LoyaltyProgram> = state
            .programs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|program| {
                if let Some(company_id) = &company_filter {
                    if program.company_id.as_deref() != Some(company_id.as_str()) {
                        return false;
                    }
                }
                if let Some(raw) = &active_filter {
                    if program.active.to_string() != *raw {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !program.name.to_lowercase().contains(&search)
                    && !program.points_name.to_lowercase().contains(&search)
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

async fn create_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    body: web::Json<CreateProgramRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyProgram> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let points_name = match &input.points_name {
            Some(raw) => require_text(raw, "points_name", MAX_POINTS_NAME_LEN)?,
            None => "points".to_string(),
        };
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => "USD".to_string(),
        };
        let discount_per_point = match input.discount_per_point {
            Some(raw) => valid_price(raw)?,
            None => DEFAULT_DISCOUNT_PER_POINT,
        };
        let company_id = match &input.company_id {
            Some(raw) => Some(require_text(raw, "company_id", MAX_COMPANY_ID_LEN)?),
            None => None,
        };
        let expiry_days = match input.expiry_days {
            Some(days) if days < 1 => {
                return Err(UnifiedError::bad_request(
                    "expiry_days must be at least 1 when set",
                ));
            }
            other => other,
        };
        let active = input.active.unwrap_or(false);
        let now = Utc::now();
        let program = LoyaltyProgram {
            id: Uuid::new_v4(),
            company_id: company_id.clone(),
            name,
            description,
            points_name,
            currency,
            discount_per_point,
            active,
            expiry_days,
            expiry_mode: input.expiry_mode.unwrap_or(ExpiryMode::Fixed),
            created_at: now,
            updated_at: now,
        };
        // One active program at a time per company: activating this
        // one deactivates the others in the same company group.
        if active {
            for mut entry in state.programs.iter_mut() {
                let other = entry.value_mut();
                if other.id != program.id
                    && other.active
                    && other.company_id == company_id
                {
                    other.active = false;
                    other.updated_at = now;
                }
            }
        }
        state.programs.insert(program.id, program.clone());
        state.emit(
            "loyalty.program.created",
            json!({
                "program_id": program.id,
                "company_id": company_id,
                "active": program.active,
            }),
        );
        Ok(program)
    })();
    match result {
        Ok(program) => created(program),
        Err(error) => err(error, debug),
    }
}

async fn get_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyProgram> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        load_program(&state, &path)
    })();
    match result {
        Ok(program) => ok(program),
        Err(error) => err(error, debug),
    }
}

async fn update_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateProgramRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyProgram> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let input = body.into_inner();
        let mut program = load_program(&state, &path)?;
        if let Some(raw) = &input.name {
            program.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(raw) = &input.description {
            program.description = Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?);
        }
        if let Some(raw) = &input.points_name {
            program.points_name = require_text(raw, "points_name", MAX_POINTS_NAME_LEN)?;
        }
        if let Some(raw) = &input.currency {
            program.currency = valid_currency(raw)?;
        }
        if let Some(raw) = input.discount_per_point {
            program.discount_per_point = valid_price(raw)?;
        }
        if let Some(days) = input.expiry_days {
            if days < 1 {
                return Err(UnifiedError::bad_request(
                    "expiry_days must be at least 1 when set",
                ));
            }
            program.expiry_days = Some(days);
        }
        if let Some(mode) = input.expiry_mode {
            program.expiry_mode = mode;
        }
        program.updated_at = Utc::now();
        state.programs.insert(program.id, program.clone());
        Ok(program)
    })();
    match result {
        Ok(program) => ok(program),
        Err(error) => err(error, debug),
    }
}

async fn delete_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let program = load_program(&state, &path)?;
        if state
            .ledger
            .iter()
            .any(|entry| entry.value().program_id == program.id)
        {
            return Err(UnifiedError::conflict(
                "Program has ledger rows and cannot be deleted; deactivate it instead",
            ));
        }
        state
            .earn_rules
            .retain(|_, rule| rule.program_id != program.id);
        state.tiers.retain(|_, tier| tier.program_id != program.id);
        state
            .redemptions
            .retain(|_, redemption| redemption.program_id != program.id);
        state.programs.remove(&program.id);
        Ok(json!({ "deleted": program.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn activate_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyProgram> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let mut program = load_program(&state, &path)?;
        let now = Utc::now();
        for mut entry in state.programs.iter_mut() {
            let other = entry.value_mut();
            if other.id != program.id && other.active && other.company_id == program.company_id {
                other.active = false;
                other.updated_at = now;
            }
        }
        program.active = true;
        program.updated_at = now;
        state.programs.insert(program.id, program.clone());
        state.emit(
            "loyalty.program.activated",
            json!({
                "program_id": program.id,
                "company_id": program.company_id,
            }),
        );
        Ok(program)
    })();
    match result {
        Ok(program) => ok(program),
        Err(error) => err(error, debug),
    }
}

async fn deactivate_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyProgram> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let mut program = load_program(&state, &path)?;
        program.active = false;
        program.updated_at = Utc::now();
        state.programs.insert(program.id, program.clone());
        state.emit(
            "loyalty.program.deactivated",
            json!({
                "program_id": program.id,
                "company_id": program.company_id,
            }),
        );
        Ok(program)
    })();
    match result {
        Ok(program) => ok(program),
        Err(error) => err(error, debug),
    }
}

fn validate_rule_values(
    points_per_unit: f64,
    fixed_points: Option<i64>,
    cap_per_day: Option<i64>,
    cap_total: Option<i64>,
    min_order_amount: Option<f64>,
) -> UnifiedResult<()> {
    if !points_per_unit.is_finite() || points_per_unit < 0.0 {
        return Err(UnifiedError::bad_request(
            "points_per_unit must be a finite number >= 0",
        ));
    }
    if let Some(fixed) = fixed_points {
        if fixed < 0 {
            return Err(UnifiedError::bad_request("fixed_points must be >= 0"));
        }
    }
    if let Some(cap) = cap_per_day {
        if cap < 1 {
            return Err(UnifiedError::bad_request("cap_per_day must be at least 1"));
        }
    }
    if let Some(cap) = cap_total {
        if cap < 1 {
            return Err(UnifiedError::bad_request("cap_total must be at least 1"));
        }
    }
    if let Some(minimum) = min_order_amount {
        if !minimum.is_finite() || minimum < 0.0 {
            return Err(UnifiedError::bad_request(
                "min_order_amount must be a finite number >= 0",
            ));
        }
    }
    Ok(())
}

async fn list_earn_rules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<LoyaltyEarnRule>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let program = load_program(&state, &path)?;
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let active_filter = params.filters.get("active").cloned();
        let mut items: Vec<LoyaltyEarnRule> = state
            .earn_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| {
                rule.program_id == program.id
                    && kind_filter
                        .as_deref()
                        .is_none_or(|raw| rule.kind.as_str() == raw)
                    && active_filter
                        .as_deref()
                        .is_none_or(|raw| rule.active.to_string() == raw)
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

async fn create_earn_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateEarnRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyEarnRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let program = load_program(&state, &path)?;
        let input = body.into_inner();
        let points_per_unit = input.points_per_unit.unwrap_or(0.0);
        validate_rule_values(
            points_per_unit,
            input.fixed_points,
            input.cap_per_day,
            input.cap_total,
            input.min_order_amount,
        )?;
        if input.active.unwrap_or(true)
            && state.earn_rules.iter().any(|entry| {
                let rule = entry.value();
                rule.program_id == program.id && rule.kind == input.kind && rule.active
            })
        {
            return Err(UnifiedError::conflict(format!(
                "An active earn rule already exists for kind \"{}\" on this program",
                input.kind.as_str()
            )));
        }
        let now = Utc::now();
        let rule = LoyaltyEarnRule {
            id: Uuid::new_v4(),
            program_id: program.id,
            kind: input.kind,
            points_per_unit,
            fixed_points: input.fixed_points,
            cap_per_day: input.cap_per_day,
            cap_total: input.cap_total,
            min_order_amount: input.min_order_amount,
            active: input.active.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        state.earn_rules.insert(rule.id, rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(rule) => created(rule),
        Err(error) => err(error, debug),
    }
}

async fn get_earn_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyEarnRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let (program_id, rule_id) = path.into_inner();
        let program = load_program(&state, &program_id)?;
        load_earn_rule(&state, program.id, &rule_id)
    })();
    match result {
        Ok(rule) => ok(rule),
        Err(error) => err(error, debug),
    }
}

async fn update_earn_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateEarnRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyEarnRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let (program_id, rule_id) = path.into_inner();
        let program = load_program(&state, &program_id)?;
        let input = body.into_inner();
        let mut rule = load_earn_rule(&state, program.id, &rule_id)?;
        let points_per_unit = input.points_per_unit.unwrap_or(rule.points_per_unit);
        let fixed_points = input.fixed_points.or(rule.fixed_points);
        let cap_per_day = input.cap_per_day.or(rule.cap_per_day);
        let cap_total = input.cap_total.or(rule.cap_total);
        let min_order_amount = input.min_order_amount.or(rule.min_order_amount);
        validate_rule_values(points_per_unit, fixed_points, cap_per_day, cap_total, min_order_amount)?;
        if input.active == Some(true)
            && state.earn_rules.iter().any(|entry| {
                let other = entry.value();
                other.program_id == program.id
                    && other.kind == rule.kind
                    && other.active
                    && other.id != rule.id
            })
        {
            return Err(UnifiedError::conflict(format!(
                "An active earn rule already exists for kind \"{}\" on this program",
                rule.kind.as_str()
            )));
        }
        rule.points_per_unit = points_per_unit;
        rule.fixed_points = fixed_points;
        rule.cap_per_day = cap_per_day;
        rule.cap_total = cap_total;
        rule.min_order_amount = min_order_amount;
        if let Some(active) = input.active {
            rule.active = active;
        }
        rule.updated_at = Utc::now();
        state.earn_rules.insert(rule.id, rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(rule) => ok(rule),
        Err(error) => err(error, debug),
    }
}

async fn delete_earn_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let (program_id, rule_id) = path.into_inner();
        let program = load_program(&state, &program_id)?;
        let rule = load_earn_rule(&state, program.id, &rule_id)?;
        state.earn_rules.remove(&rule.id);
        Ok(json!({ "deleted": rule.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_tiers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<LoyaltyTier>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let program = load_program(&state, &path)?;
        let params = query.into_inner();
        let mut items: Vec<LoyaltyTier> = state
            .tiers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|tier| tier.program_id == program.id)
            .collect();
        items.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(a.min_lifetime_points.cmp(&b.min_lifetime_points))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn create_tier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateTierRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyTier> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let program = load_program(&state, &path)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let min_lifetime_points = input.min_lifetime_points.unwrap_or(0);
        if min_lifetime_points < 0 {
            return Err(UnifiedError::bad_request("min_lifetime_points must be >= 0"));
        }
        let min_lifetime_spend = match input.min_lifetime_spend {
            Some(raw) => valid_price(raw)?,
            None => 0.0,
        };
        let discount_percent = match input.discount_percent {
            Some(raw) => {
                if !raw.is_finite() || !(0.0..=100.0).contains(&raw) {
                    return Err(UnifiedError::bad_request(
                        "discount_percent must be between 0 and 100",
                    ));
                }
                raw
            }
            None => 0.0,
        };
        let benefits = match input.benefits {
            Some(raw) => {
                let mut cleaned = Vec::new();
                for benefit in raw {
                    cleaned.push(require_text(&benefit, "benefits entry", MAX_NAME_LEN)?);
                }
                if cleaned.len() > 20 {
                    return Err(UnifiedError::bad_request(
                        "benefits must have at most 20 entries",
                    ));
                }
                cleaned
            }
            None => Vec::new(),
        };
        let now = Utc::now();
        let tier = LoyaltyTier {
            id: Uuid::new_v4(),
            program_id: program.id,
            name,
            min_lifetime_points,
            min_lifetime_spend,
            benefits,
            discount_percent,
            sort_order: input.sort_order.unwrap_or(0),
            created_at: now,
            updated_at: now,
        };
        state.tiers.insert(tier.id, tier.clone());
        Ok(tier)
    })();
    match result {
        Ok(tier) => created(tier),
        Err(error) => err(error, debug),
    }
}

async fn get_tier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyTier> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let (program_id, tier_id) = path.into_inner();
        let program = load_program(&state, &program_id)?;
        load_tier(&state, program.id, &tier_id)
    })();
    match result {
        Ok(tier) => ok(tier),
        Err(error) => err(error, debug),
    }
}

async fn update_tier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateTierRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyTier> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let (program_id, tier_id) = path.into_inner();
        let program = load_program(&state, &program_id)?;
        let input = body.into_inner();
        let mut tier = load_tier(&state, program.id, &tier_id)?;
        if let Some(raw) = &input.name {
            tier.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(value) = input.min_lifetime_points {
            if value < 0 {
                return Err(UnifiedError::bad_request("min_lifetime_points must be >= 0"));
            }
            tier.min_lifetime_points = value;
        }
        if let Some(raw) = input.min_lifetime_spend {
            tier.min_lifetime_spend = valid_price(raw)?;
        }
        if let Some(raw) = input.benefits {
            let mut cleaned = Vec::new();
            for benefit in raw {
                cleaned.push(require_text(&benefit, "benefits entry", MAX_NAME_LEN)?);
            }
            if cleaned.len() > 20 {
                return Err(UnifiedError::bad_request(
                    "benefits must have at most 20 entries",
                ));
            }
            tier.benefits = cleaned;
        }
        if let Some(raw) = input.discount_percent {
            if !raw.is_finite() || !(0.0..=100.0).contains(&raw) {
                return Err(UnifiedError::bad_request(
                    "discount_percent must be between 0 and 100",
                ));
            }
            tier.discount_percent = raw;
        }
        if let Some(value) = input.sort_order {
            tier.sort_order = value;
        }
        tier.updated_at = Utc::now();
        state.tiers.insert(tier.id, tier.clone());
        Ok(tier)
    })();
    match result {
        Ok(tier) => ok(tier),
        Err(error) => err(error, debug),
    }
}

async fn delete_tier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let (program_id, tier_id) = path.into_inner();
        let program = load_program(&state, &program_id)?;
        let tier = load_tier(&state, program.id, &tier_id)?;
        state.tiers.remove(&tier.id);
        Ok(json!({ "deleted": tier.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn earn_points(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<EarnPointsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyLedgerEntry> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let program = load_program(&state, &path)?;
        let input = body.into_inner();
        let customer_id = require_text(&input.customer_id, "customer_id", MAX_USER_ID_LEN)?;
        if input.points < 1 {
            return Err(UnifiedError::bad_request("points must be at least 1"));
        }
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        let earn_kind = match &input.earn_kind {
            Some(raw) => {
                let kind = raw.trim().to_lowercase();
                if !["purchase", "referral", "review", "signup"].contains(&kind.as_str()) {
                    return Err(UnifiedError::bad_request(
                        "earn_kind must be one of: purchase, referral, review, signup",
                    ));
                }
                Some(kind)
            }
            None => None,
        };
        let reference_id = match &input.reference_id {
            Some(raw) => Some(require_text(raw, "reference_id", MAX_ORDER_ID_LEN)?),
            None => None,
        };
        let tier_before = tier_for(&state, program.id, &customer_id).map(|tier| tier.id);
        let now = Utc::now();
        let entry = append_ledger(
            &state,
            &program,
            &customer_id,
            LedgerKind::Earn,
            input.points,
            &reason,
            &caller.actor,
            earn_kind.clone(),
            reference_id,
            None,
            now,
        )?;
        state.emit(
            "loyalty.points.earned",
            json!({
                "program_id": program.id,
                "customer_id": customer_id,
                "points": input.points,
                "balance_after": entry.balance_after,
                "earn_kind": earn_kind,
                "reason": reason,
            }),
        );
        let tier_after = tier_for(&state, program.id, &customer_id).map(|tier| tier.id);
        if tier_before != tier_after {
            state.emit(
                "loyalty.tier.changed",
                json!({
                    "program_id": program.id,
                    "customer_id": customer_id,
                    "previous_tier_id": tier_before,
                    "tier_id": tier_after,
                }),
            );
        }
        Ok(entry)
    })();
    match result {
        Ok(entry) => created(entry),
        Err(error) => err(error, debug),
    }
}

async fn adjust_points(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AdjustPointsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyLedgerEntry> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let program = load_program(&state, &path)?;
        let input = body.into_inner();
        let customer_id = require_text(&input.customer_id, "customer_id", MAX_USER_ID_LEN)?;
        if input.points == 0 {
            return Err(UnifiedError::bad_request("points must not be zero"));
        }
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        let tier_before = tier_for(&state, program.id, &customer_id).map(|tier| tier.id);
        let now = Utc::now();
        let entry = append_ledger(
            &state,
            &program,
            &customer_id,
            LedgerKind::Adjust,
            input.points,
            &reason,
            &caller.actor,
            None,
            None,
            None,
            now,
        )?;
        state.emit(
            "loyalty.points.adjusted",
            json!({
                "program_id": program.id,
                "customer_id": customer_id,
                "points": input.points,
                "balance_after": entry.balance_after,
                "reason": reason,
            }),
        );
        let tier_after = tier_for(&state, program.id, &customer_id).map(|tier| tier.id);
        if tier_before != tier_after {
            state.emit(
                "loyalty.tier.changed",
                json!({
                    "program_id": program.id,
                    "customer_id": customer_id,
                    "previous_tier_id": tier_before,
                    "tier_id": tier_after,
                }),
            );
        }
        Ok(entry)
    })();
    match result {
        Ok(entry) => created(entry),
        Err(error) => err(error, debug),
    }
}

async fn expire_points(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let program = load_program(&state, &path)?;
        if program.expiry_days.is_none() {
            return Err(UnifiedError::bad_request(
                "Program has no expiry_days set; points in this program never expire",
            ));
        }
        let now = Utc::now();
        let (customers, points) = expire_program(&state, &program, &caller.actor, now);
        Ok(json!({
            "program_id": program.id,
            "customers_expired": customers,
            "points_expired": points,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_balance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let (program_id, customer_id) = path.into_inner();
        require_customer_access(&ctx, &caller, &customer_id)?;
        let program = load_program(&state, &program_id)?;
        Ok(balance_view(&state, &program, &customer_id))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn my_balance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let program = load_program(&state, &path)?;
        Ok(balance_view(&state, &program, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_tier_for_customer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let (program_id, customer_id) = path.into_inner();
        require_customer_access(&ctx, &caller, &customer_id)?;
        let program = load_program(&state, &program_id)?;
        let tier = tier_for(&state, program.id, &customer_id);
        Ok(json!({
            "program_id": program.id,
            "customer_id": customer_id,
            "lifetime_points": lifetime_points(&state, program.id, &customer_id),
            "lifetime_spend": round_money(lifetime_spend(&state, program.id, &customer_id)),
            "tier": tier,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_ledger(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<LoyaltyLedgerEntry>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let program = load_program(&state, &path)?;
        let params = query.into_inner();
        let customer_filter = params.filters.get("customer_id").cloned();
        let kind_filter = params.filters.get("kind").cloned();
        let earn_kind_filter = params.filters.get("earn_kind").cloned();
        let manager = is_manager(&ctx, &caller.role);
        // A non-manager only ever sees their own rows, whatever
        // filter they asked for.
        let effective_customer: Option<String> = if manager {
            customer_filter.clone()
        } else {
            Some(caller.actor.clone())
        };
        if let Some(requested) = &customer_filter {
            if !manager && requested != &caller.actor {
                return Err(UnifiedError::forbidden(
                    "A customer may only access their own loyalty records",
                ));
            }
        }
        let mut items: Vec<LoyaltyLedgerEntry> = state
            .ledger
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                row.program_id == program.id
                    && effective_customer
                        .as_deref()
                        .is_none_or(|customer_id| row.customer_id == customer_id)
                    && kind_filter
                        .as_deref()
                        .is_none_or(|raw| row.kind.as_str() == raw)
                    && earn_kind_filter
                        .as_deref()
                        .is_none_or(|raw| row.earn_kind.as_deref() == Some(raw))
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

async fn redeem_points(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RedeemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyRedemption> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_VIEW)?;
        let program = load_program(&state, &path)?;
        let input = body.into_inner();
        // A customer redeems their own points; a manager may redeem
        // on behalf of any customer.
        let customer_id = match &input.customer_id {
            Some(other) if other.trim() != caller.actor => {
                require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
                require_text(other, "customer_id", MAX_USER_ID_LEN)?
            }
            _ => caller.actor.clone(),
        };
        if input.points < 1 {
            return Err(UnifiedError::bad_request("points must be at least 1"));
        }
        let kind = input.kind.trim().to_lowercase();
        if kind != "discount" && kind != "product" {
            return Err(UnifiedError::bad_request(
                "kind must be \"discount\" or \"product\"",
            ));
        }
        let order_id = match &input.order_id {
            Some(raw) => Some(require_text(raw, "order_id", MAX_ORDER_ID_LEN)?),
            None => None,
        };
        let product_id = match &input.product_id {
            Some(raw) => Some(require_text(raw, "product_id", MAX_PRODUCT_ID_LEN)?),
            None => None,
        };
        if kind == "discount" && order_id.is_none() {
            return Err(UnifiedError::bad_request(
                "order_id is required for a discount redemption",
            ));
        }
        if kind == "product" && product_id.is_none() {
            return Err(UnifiedError::bad_request(
                "product_id is required for a product redemption",
            ));
        }
        let discount_amount = if kind == "discount" {
            round_money(input.points as f64 * program.discount_per_point)
        } else {
            0.0
        };
        let tier_before = tier_for(&state, program.id, &customer_id).map(|tier| tier.id);
        let now = Utc::now();
        let entry = append_ledger(
            &state,
            &program,
            &customer_id,
            LedgerKind::Spend,
            -input.points,
            &format!("redemption: {kind}"),
            &caller.actor,
            None,
            order_id.clone(),
            None,
            now,
        )?;
        let redemption = LoyaltyRedemption {
            id: Uuid::new_v4(),
            program_id: program.id,
            customer_id: customer_id.clone(),
            points_spent: input.points,
            kind: kind.clone(),
            discount_amount,
            currency: program.currency.clone(),
            product_id: product_id.clone(),
            order_id: order_id.clone(),
            status: "completed".to_string(),
            created_at: now,
            updated_at: now,
        };
        state.redemptions.insert(redemption.id, redemption.clone());
        state.emit(
            "loyalty.points.spent",
            json!({
                "program_id": program.id,
                "customer_id": customer_id,
                "points": input.points,
                "balance_after": entry.balance_after,
                "redemption_id": redemption.id,
            }),
        );
        state.emit(
            "loyalty.redemption.completed",
            json!({
                "redemption_id": redemption.id,
                "program_id": program.id,
                "customer_id": customer_id,
                "points_spent": input.points,
                "kind": kind,
                "discount_amount": discount_amount,
                "currency": program.currency,
                "product_id": product_id,
                "order_id": order_id,
            }),
        );
        if kind == "discount" {
            // Request only: the store module owns applying the
            // discount to the order.
            state.emit(
                "loyalty.discount.requested",
                json!({
                    "redemption_id": redemption.id,
                    "program_id": program.id,
                    "customer_id": customer_id,
                    "order_id": order_id,
                    "discount_amount": discount_amount,
                    "currency": program.currency,
                }),
            );
        }
        let tier_after = tier_for(&state, program.id, &customer_id).map(|tier| tier.id);
        if tier_before != tier_after {
            state.emit(
                "loyalty.tier.changed",
                json!({
                    "program_id": program.id,
                    "customer_id": customer_id,
                    "previous_tier_id": tier_before,
                    "tier_id": tier_after,
                }),
            );
        }
        Ok(redemption)
    })();
    match result {
        Ok(redemption) => created(redemption),
        Err(error) => err(error, debug),
    }
}

async fn cancel_redemption(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LoyaltyState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoyaltyRedemption> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOYALTY_MANAGE)?;
        let mut redemption = load_redemption(&state, &path)?;
        if redemption.status != "completed" {
            return Err(UnifiedError::conflict(format!(
                "Redemption is {} and cannot be cancelled",
                redemption.status
            )));
        }
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?
            .unwrap_or_else(|| "redemption cancelled".to_string());
        let program = load_program(&state, &redemption.program_id.to_string())?;
        let now = Utc::now();
        // Refund through a new ledger row; the original spend row
        // is never rewritten.
        let entry = append_ledger(
            &state,
            &program,
            &redemption.customer_id,
            LedgerKind::Adjust,
            redemption.points_spent,
            &format!("refund: {reason}"),
            &caller.actor,
            None,
            Some(redemption.id.to_string()),
            None,
            now,
        )?;
        redemption.status = "cancelled".to_string();
        redemption.updated_at = now;
        state.redemptions.insert(redemption.id, redemption.clone());
        state.emit(
            "loyalty.redemption.cancelled",
            json!({
                "redemption_id": redemption.id,
                "program_id": program.id,
                "customer_id": redemption.customer_id,
                "points_refunded": redemption.points_spent,
                "balance_after": entry.balance_after,
                "reason": reason,
            }),
        );
        Ok(redemption)
    })();
    match result {
        Ok(redemption) => ok(redemption),
        Err(error) => err(error, debug),
    }
}

/// Reused by the cancel-redemption handler: an optional free-text
/// reason for a manual review action.
#[derive(Debug, Clone, Deserialize)]
struct ReviewRequest {
    #[serde(default)]
    pub reason: Option<String>,
}
