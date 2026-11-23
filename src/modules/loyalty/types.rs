//! Loyalty - programs, earn rules, tiers, ledger and state types.
//!
//! Part of the `loyalty` module split; see `src/modules/loyalty.rs`.

use unified_core::hooks::HookBus;
use chrono::DateTime;
use chrono::Utc;
use dashmap::DashMap;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use std::sync::RwLock;
use uuid::Uuid;

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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
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
/// `discount_per_point` for discount redemptions. A discount
/// redemption starts `pending` (points spent, discount not yet
/// confirmed by the store) and only becomes `completed` when the
/// store confirms it; a cancelled redemption refunds its points
/// through a new ledger row (the original spend row is never
/// rewritten).
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
    /// `completed` or `cancelled`. A discount redemption starts
    /// `pending`: its points are spent but the discount is not
    /// confirmed until the store applies it (see
    /// `store.discount.applied`); a product redemption starts
    /// `completed`. Cancelling any non-cancelled redemption
    /// refunds its points through a new ledger row.
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
    /// Serialises every ledger mutation (balance check + insert,
    /// cap check + insert, dedup check + insert). Held only
    /// around the synchronous check-and-insert; events are
    /// emitted after it is released, so a listener can never
    /// re-enter a ledger mutation on the same thread and
    /// deadlock. See `helpers::ledger_lock`.
    pub ledger_lock: Arc<std::sync::Mutex<()>>,
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
            ledger_lock: Arc::new(std::sync::Mutex::new(())),
        }
    }
}

impl LoyaltyState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_labels() {
        assert_eq!(ExpiryMode::Fixed.as_str(), "fixed");
        assert_eq!(ExpiryMode::Inactivity.as_str(), "inactivity");
        assert_eq!(EarnRuleKind::Purchase.as_str(), "purchase");
        assert_eq!(EarnRuleKind::Referral.as_str(), "referral");
        assert_eq!(EarnRuleKind::Review.as_str(), "review");
        assert_eq!(EarnRuleKind::Signup.as_str(), "signup");
        assert_eq!(LedgerKind::Earn.as_str(), "earn");
        assert_eq!(LedgerKind::Spend.as_str(), "spend");
        assert_eq!(LedgerKind::Expire.as_str(), "expire");
        assert_eq!(LedgerKind::Adjust.as_str(), "adjust");
    }

    #[test]
    fn enum_serde_round_trip() {
        assert_eq!(
            serde_json::to_string(&ExpiryMode::Fixed).unwrap(),
            "\"fixed\""
        );
        assert_eq!(
            serde_json::from_str::<ExpiryMode>("\"inactivity\"").unwrap(),
            ExpiryMode::Inactivity
        );
        assert_eq!(
            serde_json::to_string(&LedgerKind::Spend).unwrap(),
            "\"spend\""
        );
    }

    #[test]
    fn default_state_is_empty() {
        let state = LoyaltyState::default();
        assert!(state.programs.is_empty());
        assert!(state.ledger.is_empty());
        assert!(state.redemptions.is_empty());
    }
}
