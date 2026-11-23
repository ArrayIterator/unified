// Gamification — Types.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One rule of the rules engine: which action earns how many
/// points. `company_id` None means the rule applies to every
/// company; a company-specific rule wins over a global one for
/// the same action. `daily_cap` caps the points one user can earn
/// from this rule per calendar day (UTC); None means no cap.
/// Inactive rules never fire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GamificationRule {
    pub id: Uuid,
    pub name: String,
    pub action: String,
    pub points: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_cap: Option<i64>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One ledger entry. The ledger is the only source of truth for
/// balances: a user's balance is the sum of their entries (see
/// `user_balance`). `points` may be negative for adjustments and
/// for reward deductions. `balance_after` is the running balance
/// recorded at write time for audit convenience; it is never used
/// as the balance itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointsLedgerEntry {
    pub id: Uuid,
    pub user_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<Uuid>,
    pub action: String,
    pub points: i64,
    pub balance_after: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Idempotency key for event-driven awards: the external id of
    /// the thing that earned these points (order id, ticket id).
    /// Two entries for the same user, action and source_ref are a
    /// duplicate award, never a second one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One badge definition. A badge is awarded automatically when
/// the user's count of ledger entries with
/// `criteria_action` reaches `criteria_threshold`. The count is
/// all-time, across seasons, so a badge once earned is kept.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GamificationBadge {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub criteria_action: String,
    pub criteria_threshold: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One audited badge award: who earned which badge, when, and
/// why. One row per (badge, user) at most — re-earning the same
/// badge is a no-op.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GamificationAward {
    pub id: Uuid,
    pub badge_id: Uuid,
    pub user_id: String,
    pub reason: String,
    pub awarded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// One time-boxed competition. `status` is `upcoming`, `active`,
/// or `ended`. At most one season is `active` per company (a
/// company of None is the global scope); starting a season ends
/// any other active season of the same company.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GamificationSeason {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One reward a staff member can redeem points for. `active`
/// rewards can be requested; `points_cost` is deducted once, on
/// approval — never on request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GamificationReward {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub points_cost: i64,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One reward redemption request: `requested` -> `approved` (the
/// points are deducted here, exactly once) or `rejected`;
/// `approved` -> `fulfilled` once the reward is delivered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GamificationRewardRequest {
    pub id: Uuid,
    pub reward_id: Uuid,
    pub user_id: String,
    pub status: String,
    pub points_spent: i64,
    /// Scope the points are spent from: the company and season
    /// whose balance was checked at request time and deducted at
    /// approval time. A `None` company means the global scope; a
    /// `None` season means the balance was checked without a
    /// season filter (no active season at request time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
