// Gamification module — staff recognition: points, badges,
// seasons, team leaderboards, personal progress, and staff
// reward redemption.
//
// STAFF LEDGER SEPARATION — READ THIS FIRST. The points in this
// module are STAFF recognition points (tickets closed, sales
// made, reviews received, community help). They live in a
// SEPARATE ledger from customer loyalty points (the loyalty
// module owns the customer ledger). The two ledgers are never
// mixed: no endpoint here reads or writes loyalty points, no
// loyalty event awards staff points, and staff points are never
// spent as an automatic discount. Redemption of staff points is
// a separate, explicit flow (request a reward -> a manager
// approves -> points are deducted once, on approval).
//
// Routes live under `/api/v1/gamification`:
//
//   Rules (the rules engine):
//     GET/POST /rules — list / create a rule.
//     GET/PATCH/DELETE /rules/{id}.
//   Points ledger:
//     GET  /ledger — ledger entries (own only unless manager).
//     GET  /ledger/{id}.
//     GET  /balance — the caller's balances.
//     GET  /balance/{user_id} — one user's balances (own or
//          manager).
//     POST /points/award — run the rules engine for an action
//          (manager).
//     POST /points/adjust — direct ledger adjustment, positive or
//          negative (manager; balance may never go below zero).
//   Badges and awards:
//     GET/POST /badges — list / define a badge.
//     GET/PATCH/DELETE /badges/{id}.
//     GET  /awards — award records (own only unless manager).
//     GET  /awards/{id}.
//   Seasons:
//     GET/POST /seasons — list / create a season.
//     GET/PATCH /seasons/{id}.
//     POST /seasons/{id}/start — start it (ends any other active
//          season of the same company first; one active at a
//          time per company).
//     POST /seasons/{id}/end — end it.
//   Leaderboards and progress:
//     GET  /leaderboard — ranked points per user, optionally per
//          season, company, or department.
//     GET  /progress — the caller's points, badges, and progress
//          toward the next badges.
//     GET  /progress/{user_id} — one user's progress (own or
//          manager).
//   Rewards (staff redemption, explicit approval flow):
//     GET/POST /rewards — list / define a reward.
//     GET/PATCH/DELETE /rewards/{id}.
//     POST /rewards/{id}/request — request a reward.
//     GET  /reward-requests — requests (own only unless manager).
//     GET  /reward-requests/{id}.
//     POST /reward-requests/{id}/approve — approve and deduct the
//          points once (manager).
//     POST /reward-requests/{id}/reject — reject (manager).
//     POST /reward-requests/{id}/fulfill — mark fulfilled
//          (manager).
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory rule, ledger, badge, award, season, reward, and
// reward-request management; a rules engine (which action earns
// how many points, per company, configurable, with a per-day cap
// per rule and user); seasons with one active at a time per
// company; badges awarded automatically when a rule's action
// count reaches a badge's threshold, recorded as audited award
// rows (never silent); leaderboards computed on read from the
// ledger (balances are always sums of ledger entries — no
// separate balance column exists anywhere); personal progress
// with progress toward the next badges; and the staff reward
// flow (request -> approve with a single points deduction ->
// fulfill). What is NOT built here: customer loyalty points
// (the loyalty module owns them — see the separation note
// above); automatic discounts or any automatic spending of
// points; push / email delivery of award notices (the
// notifications module owns delivery — this module only emits
// `notification.requested` when a badge is awarded); and durable
// storage (the DashMap stores are authoritative; migration
// `2026-11-22-000058_gamification` is the persistence target).
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `gamification.rule.created`, `gamification.rule.updated`,
//     `gamification.rule.deleted`
//   - `gamification.points.earned` (per ledger entry, including
//     adjustments: `{"entry_id", "user_id", "action", "points",
//     "balance_after", "rule_id", "season_id"}`)
//   - `gamification.badge.created`, `gamification.badge.awarded`
//     (the community module listens to the awarded event to keep
//     member badge counts)
//   - `gamification.season.created`, `gamification.season.started`,
//     `gamification.season.ended`
//   - `gamification.reward.created`,
//     `gamification.reward.requested`,
//     `gamification.reward.approved`,
//     `gamification.reward.rejected`,
//     `gamification.reward.fulfilled`
//   - `notification.requested` (badge awarded, in the
//     notifications module's payload shape).
// Events this module listens to (all read defensively — a missing
// user id or an unknown action is skipped, never an error):
//   - `ticket.updated` — a ticket moving to `resolved` or
//     `closed` awards the `ticket_closed` action to its assignee
//     (`assigned_to`, falling back to `user_id`).
//   - `store.order.completed`, `order.paid` — a completed order
//     awards the `sale_completed` action to its buyer
//     (`user_id`, falling back to `customer_id`).
//   - `surveys.response.received` — a submitted survey response
//     awards the `review_received` action to its recipient
//     (`recipient_id`).
//   - `community.post.replied` — a reply awards the
//     `community_help` action to its author (`author_id`,
//     falling back to `user_id`).
//   - `community.topic.created` — a new topic awards the
//     `topic_created` action to its author.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_REASON_LEN: usize = 2000;
const MAX_ACTION_LEN: usize = 64;

/// Capability names checked through the `role.check` hook.
const PERM_GAMIFICATION_VIEW: &str = "gamification.view";
const PERM_GAMIFICATION_MANAGE: &str = "gamification.manage";

/// Actions the rules engine and badges recognise. A rule or badge
/// naming anything outside this list is rejected, so a typo can
/// never create a rule that never fires.
const KNOWN_ACTIONS: [&str; 8] = [
    "ticket_closed",
    "sale_completed",
    "review_received",
    "community_help",
    "post_created",
    "topic_created",
    "survey_completed",
    "manual_adjustment",
];

/// Hook event names this module listens to (literals repeated here
/// so the module stays decoupled from the tickets / store /
/// surveys / community modules, exactly like the events module
/// repeats the payments constants).
const TICKET_UPDATED: &str = "ticket.updated";
const STORE_ORDER_COMPLETED: &str = "store.order.completed";
const ORDER_PAID: &str = "order.paid";
const SURVEY_RESPONSE_RECEIVED: &str = "surveys.response.received";
const COMMUNITY_POST_REPLIED: &str = "community.post.replied";
const COMMUNITY_TOPIC_CREATED: &str = "community.topic.created";

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateRuleRequest {
    pub name: String,
    pub action: String,
    pub points: i64,
    pub company_id: Option<String>,
    pub daily_cap: Option<i64>,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRuleRequest {
    pub name: Option<String>,
    pub action: Option<String>,
    pub points: Option<i64>,
    pub company_id: Option<Option<String>>,
    pub daily_cap: Option<Option<i64>>,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct AwardPointsRequest {
    pub user_id: String,
    pub action: String,
    pub company_id: Option<String>,
    pub department_id: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AdjustPointsRequest {
    pub user_id: String,
    pub points: i64,
    pub reason: Option<String>,
    pub company_id: Option<String>,
    pub department_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateBadgeRequest {
    pub name: String,
    pub description: String,
    pub criteria_action: String,
    pub criteria_threshold: i64,
    pub icon: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateBadgeRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub criteria_action: Option<String>,
    pub criteria_threshold: Option<i64>,
    pub icon: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSeasonRequest {
    pub name: String,
    pub company_id: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSeasonRequest {
    pub name: Option<String>,
    pub company_id: Option<Option<String>>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateRewardRequest {
    pub name: String,
    pub description: String,
    pub points_cost: i64,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRewardRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub points_cost: Option<i64>,
    pub active: Option<bool>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for gamification. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000058_gamification`; until that migration is
/// executed, gamification data does not survive a restart.
#[derive(Clone)]
pub struct GamificationState {
    pub rules: Arc<DashMap<Uuid, GamificationRule>>,
    pub ledger: Arc<DashMap<Uuid, PointsLedgerEntry>>,
    pub badges: Arc<DashMap<Uuid, GamificationBadge>>,
    pub awards: Arc<DashMap<Uuid, GamificationAward>>,
    pub seasons: Arc<DashMap<Uuid, GamificationSeason>>,
    pub rewards: Arc<DashMap<Uuid, GamificationReward>>,
    pub reward_requests: Arc<DashMap<Uuid, GamificationRewardRequest>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for GamificationState {
    fn default() -> Self {
        Self {
            rules: Arc::new(DashMap::new()),
            ledger: Arc::new(DashMap::new()),
            badges: Arc::new(DashMap::new()),
            awards: Arc::new(DashMap::new()),
            seasons: Arc::new(DashMap::new()),
            rewards: Arc::new(DashMap::new()),
            reward_requests: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl GamificationState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "gamification",
                "category": "gamification",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the events module.
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
/// (same fail-closed shape as the events module's capability check).
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

fn can_manage(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_GAMIFICATION_MANAGE)
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

fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn valid_action(action: &str) -> bool {
    KNOWN_ACTIONS.contains(&action)
}

fn load_rule(state: &GamificationState, raw_id: &str) -> UnifiedResult<GamificationRule> {
    let id = parse_uuid(raw_id, "rule")?;
    state
        .rules
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Rule not found: {id}")))
}

fn load_badge(state: &GamificationState, raw_id: &str) -> UnifiedResult<GamificationBadge> {
    let id = parse_uuid(raw_id, "badge")?;
    state
        .badges
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Badge not found: {id}")))
}

fn load_season(state: &GamificationState, raw_id: &str) -> UnifiedResult<GamificationSeason> {
    let id = parse_uuid(raw_id, "season")?;
    state
        .seasons
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Season not found: {id}")))
}

fn load_reward(state: &GamificationState, raw_id: &str) -> UnifiedResult<GamificationReward> {
    let id = parse_uuid(raw_id, "reward")?;
    state
        .rewards
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Reward not found: {id}")))
}

fn load_reward_request(
    state: &GamificationState,
    raw_id: &str,
) -> UnifiedResult<GamificationRewardRequest> {
    let id = parse_uuid(raw_id, "reward request")?;
    state
        .reward_requests
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Reward request not found: {id}")))
}

/// A user's balance is the sum of their ledger entries, optionally
/// scoped to one company and/or one season. No balance column is
/// ever stored or trusted.
fn user_balance(
    state: &GamificationState,
    user_id: &str,
    company_id: Option<&str>,
    season_id: Option<Uuid>,
) -> i64 {
    state
        .ledger
        .iter()
        .filter(|entry| {
            let row = entry.value();
            if row.user_id != user_id {
                return false;
            }
            if let Some(company) = company_id {
                if row.company_id.as_deref() != Some(company) {
                    return false;
                }
            }
            if let Some(season) = season_id {
                if row.season_id != Some(season) {
                    return false;
                }
            }
            true
        })
        .map(|entry| entry.value().points)
        .sum()
}

/// The active season for a company (None = global scope), when one
/// exists. At most one can be active per company (enforced on
/// start), so the first match is the answer.
fn active_season_for(state: &GamificationState, company_id: Option<&str>) -> Option<GamificationSeason> {
    state
        .seasons
        .iter()
        .map(|entry| entry.value().clone())
        .find(|season| season.status == "active" && season.company_id.as_deref() == company_id)
}

/// Points the user already earned from one rule today (UTC day),
/// for the daily cap check.
fn points_from_rule_today(state: &GamificationState, user_id: &str, rule_id: Uuid) -> i64 {
    let today = Utc::now().date_naive();
    state
        .ledger
        .iter()
        .filter(|entry| {
            let row = entry.value();
            row.user_id == user_id
                && row.rule_id == Some(rule_id)
                && row.created_at.date_naive() == today
                && row.points > 0
        })
        .map(|entry| entry.value().points)
        .sum()
}

/// How many times the user performed one action (ledger entries
/// with positive points for that action), for badge thresholds.
fn action_count(state: &GamificationState, user_id: &str, action: &str) -> i64 {
    state
        .ledger
        .iter()
        .filter(|entry| {
            let row = entry.value();
            row.user_id == user_id && row.action == action && row.points > 0
        })
        .count() as i64
}

/// Award every badge whose criteria the user now meets for the
/// given action. An award is an audited record: one row per
/// (badge, user), never silent, never duplicated.
fn check_badges(state: &GamificationState, user_id: &str, action: &str) {
    let badges: Vec<GamificationBadge> = state
        .badges
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|badge| badge.criteria_action == action)
        .collect();
    if badges.is_empty() {
        return;
    }
    let count = action_count(state, user_id, action);
    for badge in badges {
        if count < badge.criteria_threshold {
            continue;
        }
        if state.awards.iter().any(|entry| {
            entry.value().badge_id == badge.id && entry.value().user_id == user_id
        }) {
            continue;
        }
        let now = Utc::now();
        let award = GamificationAward {
            id: Uuid::now_v7(),
            badge_id: badge.id,
            user_id: user_id.to_string(),
            reason: format!(
                "Reached {count} {} (threshold {})",
                action, badge.criteria_threshold
            ),
            awarded_at: now,
            created_at: now,
        };
        state.awards.insert(award.id, award.clone());
        state.emit(
            "gamification.badge.awarded",
            json!({
                "award_id": award.id.to_string(),
                "badge_id": badge.id.to_string(),
                "badge_name": badge.name,
                "user_id": user_id,
                "action": action,
                "count": count,
            }),
        );
        state.notify(
            "gamification.badge.awarded",
            user_id,
            &format!("Badge earned: {}", badge.name),
            &format!(
                "You earned the \"{}\" badge for {} ({} times).",
                badge.name, action, count
            ),
            Some("/api/v1/gamification/progress".to_string()),
        );
    }
}

/// Append one ledger entry and emit `gamification.points.earned`.
/// The caller is responsible for the points value (rule engine,
/// adjustment, or reward deduction).
fn append_ledger_entry(
    state: &GamificationState,
    user_id: &str,
    company_id: Option<String>,
    department_id: Option<String>,
    rule_id: Option<Uuid>,
    action: &str,
    points: i64,
    season_id: Option<Uuid>,
    reason: Option<String>,
) -> PointsLedgerEntry {
    let balance_after =
        user_balance(state, user_id, company_id.as_deref(), None) + points;
    let entry = PointsLedgerEntry {
        id: Uuid::now_v7(),
        user_id: user_id.to_string(),
        company_id,
        department_id,
        rule_id,
        action: action.to_string(),
        points,
        balance_after,
        season_id,
        reason,
        created_at: Utc::now(),
    };
    state.ledger.insert(entry.id, entry.clone());
    state.emit(
        "gamification.points.earned",
        json!({
            "entry_id": entry.id.to_string(),
            "user_id": entry.user_id,
            "action": entry.action,
            "points": entry.points,
            "balance_after": entry.balance_after,
            "rule_id": entry.rule_id.map(|id| id.to_string()),
            "season_id": entry.season_id.map(|id| id.to_string()),
            "company_id": entry.company_id,
            "department_id": entry.department_id,
        }),
    );
    entry
}

/// Outcome of running the rules engine for one action.
#[derive(Debug, Clone, Serialize)]
pub struct AwardOutcome {
    pub awarded: bool,
    pub points: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The rules engine: find the best matching active rule for the
/// action (a company-specific rule wins over a global one), apply
/// its daily cap, append the ledger entry, and check badges.
/// Returns `awarded: false` with a reason when no rule matches
/// or the daily cap is already reached — never an error, so hook
/// listeners stay safe.
fn award_points_for_action(
    state: &GamificationState,
    user_id: &str,
    action: &str,
    company_id: Option<String>,
    department_id: Option<String>,
    reason: Option<String>,
) -> AwardOutcome {
    let rules: Vec<GamificationRule> = state
        .rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| rule.active && rule.action == action)
        .collect();
    let rule = rules
        .iter()
        .find(|rule| rule.company_id.as_deref() == company_id.as_deref())
        .or_else(|| rules.iter().find(|rule| rule.company_id.is_none()));
    let Some(rule) = rule else {
        return AwardOutcome {
            awarded: false,
            points: 0,
            entry_id: None,
            rule_id: None,
            reason: Some(format!("No active rule matches action \"{action}\"")),
        };
    };
    let mut points = rule.points;
    if let Some(cap) = rule.daily_cap {
        let earned_today = points_from_rule_today(state, user_id, rule.id);
        if earned_today >= cap {
            return AwardOutcome {
                awarded: false,
                points: 0,
                entry_id: None,
                rule_id: Some(rule.id.to_string()),
                reason: Some(format!(
                    "Daily cap of {cap} points for rule \"{}\" already reached",
                    rule.name
                )),
            };
        }
        if earned_today + points > cap {
            points = cap - earned_today;
        }
    }
    if points <= 0 {
        return AwardOutcome {
            awarded: false,
            points: 0,
            entry_id: None,
            rule_id: Some(rule.id.to_string()),
            reason: Some("Rule awards zero points".to_string()),
        };
    }
    let season_id = active_season_for(state, company_id.as_deref()).map(|season| season.id);
    let entry = append_ledger_entry(
        state,
        user_id,
        company_id,
        department_id,
        Some(rule.id),
        action,
        points,
        season_id,
        reason,
    );
    check_badges(state, user_id, action);
    AwardOutcome {
        awarded: true,
        points,
        entry_id: Some(entry.id.to_string()),
        rule_id: Some(rule.id.to_string()),
        reason: None,
    }
}

// ---------------------------------------------------------------------------
// Event listeners (defensive: missing ids are skipped, never errors)
// ---------------------------------------------------------------------------

fn on_ticket_updated(state: GamificationState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let status = opt_string(&data, "status").unwrap_or_default();
    if status != "resolved" && status != "closed" {
        ctx.add_result(json!({ "gamification": "skipped", "reason": format!("ticket status is \"{status}\", not resolved/closed") }));
        return;
    }
    let Some(user_id) = opt_string(&data, "assigned_to")
        .or_else(|| opt_string(&data, "assignee_id"))
        .or_else(|| opt_string(&data, "user_id"))
    else {
        ctx.add_result(json!({ "gamification": "skipped", "reason": "ticket.updated carries no assignee" }));
        return;
    };
    let outcome = award_points_for_action(
        &state,
        &user_id,
        "ticket_closed",
        opt_string(&data, "company_id"),
        opt_string(&data, "department_id"),
        Some(format!("Ticket {} closed", opt_string(&data, "id").unwrap_or_default())),
    );
    ctx.add_result(json!({ "gamification": "ticket_closed", "user_id": user_id, "awarded": outcome.awarded, "points": outcome.points }));
}

fn on_order_completed(state: GamificationState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = opt_string(&data, "user_id").or_else(|| opt_string(&data, "customer_id")) else {
        ctx.add_result(json!({ "gamification": "skipped", "reason": "order event carries no user_id or customer_id" }));
        return;
    };
    let outcome = award_points_for_action(
        &state,
        &user_id,
        "sale_completed",
        opt_string(&data, "company_id"),
        None,
        Some(format!("Order {} completed", opt_string(&data, "order_id").or_else(|| opt_string(&data, "id")).unwrap_or_default())),
    );
    ctx.add_result(json!({ "gamification": "sale_completed", "user_id": user_id, "awarded": outcome.awarded, "points": outcome.points }));
}

fn on_survey_response_received(state: GamificationState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = opt_string(&data, "recipient_id").or_else(|| opt_string(&data, "user_id")) else {
        ctx.add_result(json!({ "gamification": "skipped", "reason": "surveys.response.received carries no recipient_id" }));
        return;
    };
    let outcome = award_points_for_action(
        &state,
        &user_id,
        "review_received",
        opt_string(&data, "company_id"),
        opt_string(&data, "department_id"),
        Some("Survey response received".to_string()),
    );
    ctx.add_result(json!({ "gamification": "review_received", "user_id": user_id, "awarded": outcome.awarded, "points": outcome.points }));
}

fn on_community_post_replied(state: GamificationState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = opt_string(&data, "author_id").or_else(|| opt_string(&data, "user_id")) else {
        ctx.add_result(json!({ "gamification": "skipped", "reason": "community.post.replied carries no author_id" }));
        return;
    };
    let outcome = award_points_for_action(
        &state,
        &user_id,
        "community_help",
        opt_string(&data, "company_id"),
        None,
        Some(format!("Reply to community topic {}", opt_string(&data, "topic_id").unwrap_or_default())),
    );
    ctx.add_result(json!({ "gamification": "community_help", "user_id": user_id, "awarded": outcome.awarded, "points": outcome.points }));
}

fn on_community_topic_created(state: GamificationState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = opt_string(&data, "author_id").or_else(|| opt_string(&data, "user_id")) else {
        ctx.add_result(json!({ "gamification": "skipped", "reason": "community.topic.created carries no author_id" }));
        return;
    };
    let outcome = award_points_for_action(
        &state,
        &user_id,
        "topic_created",
        opt_string(&data, "company_id"),
        None,
        Some(format!("Topic {} created", opt_string(&data, "topic_id").unwrap_or_default())),
    );
    ctx.add_result(json!({ "gamification": "topic_created", "user_id": user_id, "awarded": outcome.awarded, "points": outcome.points }));
}

// ---------------------------------------------------------------------------
// Rule handlers
// ---------------------------------------------------------------------------

async fn list_rules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let params = query.into_inner();
        let action_filter = params.filters.get("action").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let active_filter = params
            .filters
            .get("active")
            .and_then(|raw| raw.parse::<bool>().ok());
        let mut items: Vec<GamificationRule> = state
            .rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| {
                if let Some(action) = &action_filter {
                    if &rule.action != action {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    if rule.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                if let Some(active) = active_filter {
                    if rule.active != active {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|rule| (rule.action.clone(), rule.name.clone()));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    body: web::Json<CreateRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let action = require_text(&input.action, "action", MAX_ACTION_LEN)?;
        if !valid_action(&action) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown action \"{action}\"; known actions: {}",
                KNOWN_ACTIONS.join(", ")
            )));
        }
        if input.points < 0 || input.points > 1_000_000 {
            return Err(UnifiedError::bad_request(
                "points must be between 0 and 1000000",
            ));
        }
        if let Some(cap) = input.daily_cap {
            if cap < 0 || cap > 1_000_000 {
                return Err(UnifiedError::bad_request(
                    "daily_cap must be between 0 and 1000000",
                ));
            }
        }
        let company_id = match &input.company_id {
            Some(raw) => Some(require_text(raw, "company_id", MAX_NAME_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let rule = GamificationRule {
            id: Uuid::now_v7(),
            name,
            action,
            points: input.points,
            company_id,
            daily_cap: input.daily_cap,
            active: input.active.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        state.rules.insert(rule.id, rule.clone());
        state.emit(
            "gamification.rule.created",
            json!({
                "rule_id": rule.id.to_string(),
                "name": rule.name,
                "action": rule.action,
                "points": rule.points,
            }),
        );
        Ok(rule)
    })();
    match result {
        Ok(rule) => created(rule),
        Err(error) => err(error, debug),
    }
}

async fn get_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        load_rule(&state, &path.into_inner())
    })();
    match result {
        Ok(rule) => ok(rule),
        Err(error) => err(error, debug),
    }
}

async fn update_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut rule = load_rule(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            rule.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(action) = &input.action {
            let checked = require_text(action, "action", MAX_ACTION_LEN)?;
            if !valid_action(&checked) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown action \"{checked}\"; known actions: {}",
                    KNOWN_ACTIONS.join(", ")
                )));
            }
            rule.action = checked;
        }
        if let Some(points) = input.points {
            if points < 0 || points > 1_000_000 {
                return Err(UnifiedError::bad_request(
                    "points must be between 0 and 1000000",
                ));
            }
            rule.points = points;
        }
        if let Some(company_id) = &input.company_id {
            rule.company_id = match company_id {
                Some(raw) => Some(require_text(raw, "company_id", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(daily_cap) = &input.daily_cap {
            if let Some(cap) = daily_cap {
                if *cap < 0 || *cap > 1_000_000 {
                    return Err(UnifiedError::bad_request(
                        "daily_cap must be between 0 and 1000000",
                    ));
                }
            }
            rule.daily_cap = *daily_cap;
        }
        if let Some(active) = input.active {
            rule.active = active;
        }
        rule.updated_at = Utc::now();
        state.rules.insert(rule.id, rule.clone());
        state.emit(
            "gamification.rule.updated",
            json!({ "rule_id": rule.id.to_string(), "action": rule.action, "points": rule.points }),
        );
        Ok(rule)
    })();
    match result {
        Ok(rule) => ok(rule),
        Err(error) => err(error, debug),
    }
}

async fn delete_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let rule = load_rule(&state, &path.into_inner())?;
        let id = rule.id;
        state.rules.remove(&id);
        state.emit(
            "gamification.rule.deleted",
            json!({ "rule_id": id.to_string(), "name": rule.name }),
        );
        Ok(json!({ "deleted": true, "rule_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Ledger / balance / points handlers
// ---------------------------------------------------------------------------

async fn list_ledger(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        let action_filter = params.filters.get("action").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let season_filter = params
            .filters
            .get("season_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        let department_filter = params.filters.get("department_id").cloned();
        let mut items: Vec<PointsLedgerEntry> = state
            .ledger
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                // A staff member sees only their own ledger unless
                // they hold the manage capability.
                if !is_manager && row.user_id != caller.actor {
                    return false;
                }
                if let Some(user) = &user_filter {
                    if &row.user_id != user {
                        return false;
                    }
                }
                if let Some(action) = &action_filter {
                    if &row.action != action {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    if row.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                if let Some(season) = season_filter {
                    if row.season_id != Some(season) {
                        return false;
                    }
                }
                if let Some(department) = &department_filter {
                    if row.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|row| std::cmp::Reverse(row.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_ledger_entry(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PointsLedgerEntry> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "ledger entry")?;
        let entry = state
            .ledger
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Ledger entry not found: {id}")))?;
        if entry.user_id != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::not_found(format!("Ledger entry not found: {id}")));
        }
        Ok(entry)
    })();
    match result {
        Ok(entry) => ok(entry),
        Err(error) => err(error, debug),
    }
}

fn balance_view(
    state: &GamificationState,
    user_id: &str,
    company_id: Option<&str>,
    season_id: Option<Uuid>,
) -> Value {
    let total = user_balance(state, user_id, company_id, None);
    let season_points = season_id.map(|season| user_balance(state, user_id, company_id, Some(season)));
    let mut seasons: Vec<Value> = Vec::new();
    for season in state.seasons.iter().map(|entry| entry.value().clone()) {
        if company_id.is_some() && season.company_id.as_deref() != company_id {
            continue;
        }
        seasons.push(json!({
            "season_id": season.id.to_string(),
            "season_name": season.name,
            "status": season.status,
            "points": user_balance(state, user_id, company_id, Some(season.id)),
        }));
    }
    json!({
        "user_id": user_id,
        "company_id": company_id,
        "total_points": total,
        "season_id": season_id.map(|id| id.to_string()),
        "season_points": season_points,
        "seasons": seasons,
    })
}

async fn get_own_balance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let params = query.into_inner();
        let company_id = params.filters.get("company_id").cloned();
        let season_id = params
            .filters
            .get("season_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        Ok(balance_view(
            &state,
            &caller.actor,
            company_id.as_deref(),
            season_id,
        ))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_user_balance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let user_id = path.into_inner();
        if user_id != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::not_found(format!("User not found: {user_id}")));
        }
        let params = query.into_inner();
        let company_id = params.filters.get("company_id").cloned();
        let season_id = params
            .filters
            .get("season_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        Ok(balance_view(
            &state,
            &user_id,
            company_id.as_deref(),
            season_id,
        ))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn award_points(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    body: web::Json<AwardPointsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", MAX_NAME_LEN)?;
        let action = require_text(&input.action, "action", MAX_ACTION_LEN)?;
        if !valid_action(&action) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown action \"{action}\"; known actions: {}",
                KNOWN_ACTIONS.join(", ")
            )));
        }
        let company_id = match &input.company_id {
            Some(raw) => Some(require_text(raw, "company_id", MAX_NAME_LEN)?),
            None => None,
        };
        let department_id = match &input.department_id {
            Some(raw) => Some(require_text(raw, "department_id", MAX_NAME_LEN)?),
            None => None,
        };
        let reason = match &input.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let outcome = award_points_for_action(
            &state,
            &user_id,
            &action,
            company_id,
            department_id,
            reason,
        );
        Ok(json!({
            "user_id": user_id,
            "action": action,
            "awarded": outcome.awarded,
            "points": outcome.points,
            "entry_id": outcome.entry_id,
            "rule_id": outcome.rule_id,
            "reason": outcome.reason,
            "balance_after": user_balance(&state, &user_id, None, None),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn adjust_points(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    body: web::Json<AdjustPointsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PointsLedgerEntry> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", MAX_NAME_LEN)?;
        if input.points == 0 || input.points < -1_000_000 || input.points > 1_000_000 {
            return Err(UnifiedError::bad_request(
                "points must be non-zero and between -1000000 and 1000000",
            ));
        }
        let company_id = match &input.company_id {
            Some(raw) => Some(require_text(raw, "company_id", MAX_NAME_LEN)?),
            None => None,
        };
        let department_id = match &input.department_id {
            Some(raw) => Some(require_text(raw, "department_id", MAX_NAME_LEN)?),
            None => None,
        };
        let reason = match &input.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => Some(format!("Manual adjustment by {}", caller.actor)),
        };
        // A balance may never go below zero.
        let current = user_balance(&state, &user_id, company_id.as_deref(), None);
        if current + input.points < 0 {
            return Err(UnifiedError::conflict(format!(
                "Adjustment of {} points would take the balance below zero (current balance is {current})",
                input.points
            )));
        }
        let season_id = active_season_for(&state, company_id.as_deref()).map(|season| season.id);
        let entry = append_ledger_entry(
            &state,
            &user_id,
            company_id,
            department_id,
            None,
            "manual_adjustment",
            input.points,
            season_id,
            reason,
        );
        Ok(entry)
    })();
    match result {
        Ok(entry) => created(entry),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Badge / award handlers
// ---------------------------------------------------------------------------

async fn list_badges(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let params = query.into_inner();
        let action_filter = params.filters.get("criteria_action").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<GamificationBadge> = state
            .badges
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|badge| {
                if let Some(action) = &action_filter {
                    if &badge.criteria_action != action {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !format!("{} {}", badge.name, badge.description)
                        .to_lowercase()
                        .contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|badge| badge.name.clone());
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_badge(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    body: web::Json<CreateBadgeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationBadge> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let criteria_action = require_text(&input.criteria_action, "criteria_action", MAX_ACTION_LEN)?;
        if !valid_action(&criteria_action) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown action \"{criteria_action}\"; known actions: {}",
                KNOWN_ACTIONS.join(", ")
            )));
        }
        if input.criteria_threshold < 1 || input.criteria_threshold > 1_000_000 {
            return Err(UnifiedError::bad_request(
                "criteria_threshold must be between 1 and 1000000",
            ));
        }
        let icon = match &input.icon {
            Some(raw) => Some(require_text(raw, "icon", MAX_NAME_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let badge = GamificationBadge {
            id: Uuid::now_v7(),
            name,
            description,
            criteria_action,
            criteria_threshold: input.criteria_threshold,
            icon,
            created_at: now,
            updated_at: now,
        };
        state.badges.insert(badge.id, badge.clone());
        state.emit(
            "gamification.badge.created",
            json!({
                "badge_id": badge.id.to_string(),
                "name": badge.name,
                "criteria_action": badge.criteria_action,
                "criteria_threshold": badge.criteria_threshold,
            }),
        );
        Ok(badge)
    })();
    match result {
        Ok(badge) => created(badge),
        Err(error) => err(error, debug),
    }
}

async fn get_badge(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationBadge> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        load_badge(&state, &path.into_inner())
    })();
    match result {
        Ok(badge) => ok(badge),
        Err(error) => err(error, debug),
    }
}

async fn update_badge(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateBadgeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationBadge> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut badge = load_badge(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            badge.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            badge.description = require_text(description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(action) = &input.criteria_action {
            let checked = require_text(action, "criteria_action", MAX_ACTION_LEN)?;
            if !valid_action(&checked) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown action \"{checked}\"; known actions: {}",
                    KNOWN_ACTIONS.join(", ")
                )));
            }
            badge.criteria_action = checked;
        }
        if let Some(threshold) = input.criteria_threshold {
            if threshold < 1 || threshold > 1_000_000 {
                return Err(UnifiedError::bad_request(
                    "criteria_threshold must be between 1 and 1000000",
                ));
            }
            badge.criteria_threshold = threshold;
        }
        if let Some(icon) = &input.icon {
            badge.icon = match icon {
                Some(raw) => Some(require_text(raw, "icon", MAX_NAME_LEN)?),
                None => None,
            };
        }
        badge.updated_at = Utc::now();
        state.badges.insert(badge.id, badge.clone());
        Ok(badge)
    })();
    match result {
        Ok(badge) => ok(badge),
        Err(error) => err(error, debug),
    }
}

async fn delete_badge(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let badge = load_badge(&state, &path.into_inner())?;
        if state
            .awards
            .iter()
            .any(|entry| entry.value().badge_id == badge.id)
        {
            return Err(UnifiedError::conflict(
                "A badge with awards cannot be deleted; its award records must stay auditable",
            ));
        }
        let id = badge.id;
        state.badges.remove(&id);
        Ok(json!({ "deleted": true, "badge_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_awards(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        let badge_filter = params
            .filters
            .get("badge_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        let mut items: Vec<Value> = state
            .awards
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|award| {
                if !is_manager && award.user_id != caller.actor {
                    return false;
                }
                if let Some(user) = &user_filter {
                    if &award.user_id != user {
                        return false;
                    }
                }
                if let Some(badge) = badge_filter {
                    if award.badge_id != badge {
                        return false;
                    }
                }
                true
            })
            .map(|award| {
                let badge_name = state
                    .badges
                    .get(&award.badge_id)
                    .map(|entry| entry.value().name.clone());
                let mut value = serde_json::to_value(&award).unwrap_or(Value::Null);
                if let Value::Object(map) = &mut value {
                    map.insert(
                        "badge_name".to_string(),
                        badge_name.map(Value::String).unwrap_or(Value::Null),
                    );
                }
                value
            })
            .collect();
        items.sort_by(|a, b| {
            b.get("awarded_at")
                .and_then(Value::as_str)
                .cmp(&a.get("awarded_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_award(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationAward> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "award")?;
        let award = state
            .awards
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Award not found: {id}")))?;
        if award.user_id != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::not_found(format!("Award not found: {id}")));
        }
        Ok(award)
    })();
    match result {
        Ok(award) => ok(award),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Season handlers
// ---------------------------------------------------------------------------

async fn list_seasons(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let mut items: Vec<GamificationSeason> = state
            .seasons
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|season| {
                if let Some(status) = &status_filter {
                    if &season.status != status {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    if season.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|season| std::cmp::Reverse(season.starts_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_season(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    body: web::Json<CreateSeasonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationSeason> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        if input.ends_at <= input.starts_at {
            return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
        }
        let company_id = match &input.company_id {
            Some(raw) => Some(require_text(raw, "company_id", MAX_NAME_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let season = GamificationSeason {
            id: Uuid::now_v7(),
            name,
            company_id,
            starts_at: input.starts_at,
            ends_at: input.ends_at,
            status: "upcoming".to_string(),
            created_at: now,
            updated_at: now,
        };
        state.seasons.insert(season.id, season.clone());
        state.emit(
            "gamification.season.created",
            json!({
                "season_id": season.id.to_string(),
                "name": season.name,
                "starts_at": season.starts_at.to_rfc3339(),
                "ends_at": season.ends_at.to_rfc3339(),
            }),
        );
        Ok(season)
    })();
    match result {
        Ok(season) => created(season),
        Err(error) => err(error, debug),
    }
}

async fn get_season(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationSeason> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        load_season(&state, &path.into_inner())
    })();
    match result {
        Ok(season) => ok(season),
        Err(error) => err(error, debug),
    }
}

async fn update_season(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSeasonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationSeason> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut season = load_season(&state, &path.into_inner())?;
        if season.status == "ended" {
            return Err(UnifiedError::conflict("An ended season cannot be updated"));
        }
        let input = body.into_inner();
        if let Some(name) = &input.name {
            season.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(company_id) = &input.company_id {
            season.company_id = match company_id {
                Some(raw) => Some(require_text(raw, "company_id", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(starts_at) = input.starts_at {
            season.starts_at = starts_at;
        }
        if let Some(ends_at) = input.ends_at {
            season.ends_at = ends_at;
        }
        if season.ends_at <= season.starts_at {
            return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
        }
        season.updated_at = Utc::now();
        state.seasons.insert(season.id, season.clone());
        Ok(season)
    })();
    match result {
        Ok(season) => ok(season),
        Err(error) => err(error, debug),
    }
}

async fn start_season(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationSeason> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut season = load_season(&state, &path.into_inner())?;
        if season.status != "upcoming" {
            return Err(UnifiedError::conflict(format!(
                "Only an upcoming season can be started (status is \"{}\")",
                season.status
            )));
        }
        let now = Utc::now();
        // One active season at a time per company: end any other
        // active season of the same company first.
        let others: Vec<GamificationSeason> = state
            .seasons
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|other| {
                other.id != season.id
                    && other.status == "active"
                    && other.company_id == season.company_id
            })
            .collect();
        for mut other in others {
            other.status = "ended".to_string();
            other.updated_at = now;
            state.seasons.insert(other.id, other.clone());
            state.emit(
                "gamification.season.ended",
                json!({
                    "season_id": other.id.to_string(),
                    "name": other.name,
                    "reason": "superseded",
                }),
            );
        }
        season.status = "active".to_string();
        season.updated_at = now;
        state.seasons.insert(season.id, season.clone());
        state.emit(
            "gamification.season.started",
            json!({
                "season_id": season.id.to_string(),
                "name": season.name,
                "starts_at": season.starts_at.to_rfc3339(),
                "ends_at": season.ends_at.to_rfc3339(),
            }),
        );
        Ok(season)
    })();
    match result {
        Ok(season) => ok(season),
        Err(error) => err(error, debug),
    }
}

async fn end_season(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationSeason> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut season = load_season(&state, &path.into_inner())?;
        if season.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Only an active season can be ended (status is \"{}\")",
                season.status
            )));
        }
        season.status = "ended".to_string();
        season.updated_at = Utc::now();
        state.seasons.insert(season.id, season.clone());
        state.emit(
            "gamification.season.ended",
            json!({
                "season_id": season.id.to_string(),
                "name": season.name,
            }),
        );
        Ok(season)
    })();
    match result {
        Ok(season) => ok(season),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Leaderboard / progress handlers
// ---------------------------------------------------------------------------

async fn get_leaderboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let season_filter = params
            .filters
            .get("season_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        // Aggregate the ledger per user; the balance is always a
        // sum of ledger entries, never a stored column.
        let mut totals: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        for entry in state.ledger.iter() {
            let row = entry.value();
            if let Some(company) = &company_filter {
                if row.company_id.as_deref() != Some(company.as_str()) {
                    continue;
                }
            }
            if let Some(department) = &department_filter {
                if row.department_id.as_deref() != Some(department.as_str()) {
                    continue;
                }
            }
            if let Some(season) = season_filter {
                if row.season_id != Some(season) {
                    continue;
                }
            }
            *totals.entry(row.user_id.clone()).or_insert(0) += row.points;
        }
        let mut rows: Vec<(String, i64)> = totals.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut items: Vec<Value> = Vec::new();
        for (index, (user_id, points)) in rows.into_iter().enumerate() {
            let badge_count = state
                .awards
                .iter()
                .filter(|entry| entry.value().user_id == user_id)
                .count();
            items.push(json!({
                "rank": index + 1,
                "user_id": user_id,
                "points": points,
                "badge_count": badge_count,
            }));
        }
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn progress_view(state: &GamificationState, user_id: &str) -> Value {
    let total_points = user_balance(state, user_id, None, None);
    let awards: Vec<Value> = {
        let mut rows: Vec<GamificationAward> = state
            .awards
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|award| award.user_id == user_id)
            .collect();
        rows.sort_by_key(|award| std::cmp::Reverse(award.awarded_at));
        rows.into_iter()
            .map(|award| {
                let badge_name = state
                    .badges
                    .get(&award.badge_id)
                    .map(|entry| entry.value().name.clone());
                let mut value = serde_json::to_value(&award).unwrap_or(Value::Null);
                if let Value::Object(map) = &mut value {
                    map.insert(
                        "badge_name".to_string(),
                        badge_name.map(Value::String).unwrap_or(Value::Null),
                    );
                }
                value
            })
            .collect()
    };
    // Progress toward badges not yet earned: current action count
    // against the threshold.
    let earned_badge_ids: Vec<Uuid> = state
        .awards
        .iter()
        .filter(|entry| entry.value().user_id == user_id)
        .map(|entry| entry.value().badge_id)
        .collect();
    let mut next_badges: Vec<Value> = Vec::new();
    for badge in state.badges.iter().map(|entry| entry.value().clone()) {
        if earned_badge_ids.contains(&badge.id) {
            continue;
        }
        let count = action_count(state, user_id, &badge.criteria_action);
        next_badges.push(json!({
            "badge_id": badge.id.to_string(),
            "badge_name": badge.name,
            "criteria_action": badge.criteria_action,
            "criteria_threshold": badge.criteria_threshold,
            "current_count": count,
            "remaining": (badge.criteria_threshold - count).max(0),
        }));
    }
    next_badges.sort_by_key(|item| {
        item.get("remaining")
            .and_then(Value::as_i64)
            .unwrap_or(i64::MAX)
    });
    let mut season_points: Vec<Value> = Vec::new();
    for season in state.seasons.iter().map(|entry| entry.value().clone()) {
        season_points.push(json!({
            "season_id": season.id.to_string(),
            "season_name": season.name,
            "status": season.status,
            "points": user_balance(state, user_id, None, Some(season.id)),
        }));
    }
    json!({
        "user_id": user_id,
        "total_points": total_points,
        "badge_count": awards.len(),
        "awards": awards,
        "next_badges": next_badges,
        "seasons": season_points,
    })
}

async fn get_own_progress(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        Ok(progress_view(&state, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_user_progress(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let user_id = path.into_inner();
        if user_id != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::not_found(format!("User not found: {user_id}")));
        }
        Ok(progress_view(&state, &user_id))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Reward handlers
// ---------------------------------------------------------------------------

async fn list_rewards(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let params = query.into_inner();
        let active_filter = params
            .filters
            .get("active")
            .and_then(|raw| raw.parse::<bool>().ok());
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<GamificationReward> = state
            .rewards
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|reward| {
                if let Some(active) = active_filter {
                    if reward.active != active {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !format!("{} {}", reward.name, reward.description)
                        .to_lowercase()
                        .contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|reward| reward.points_cost);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_reward(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    body: web::Json<CreateRewardRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationReward> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        if input.points_cost < 1 || input.points_cost > 1_000_000 {
            return Err(UnifiedError::bad_request(
                "points_cost must be between 1 and 1000000",
            ));
        }
        let now = Utc::now();
        let reward = GamificationReward {
            id: Uuid::now_v7(),
            name,
            description,
            points_cost: input.points_cost,
            active: input.active.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        state.rewards.insert(reward.id, reward.clone());
        state.emit(
            "gamification.reward.created",
            json!({
                "reward_id": reward.id.to_string(),
                "name": reward.name,
                "points_cost": reward.points_cost,
            }),
        );
        Ok(reward)
    })();
    match result {
        Ok(reward) => created(reward),
        Err(error) => err(error, debug),
    }
}

async fn get_reward(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationReward> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        load_reward(&state, &path.into_inner())
    })();
    match result {
        Ok(reward) => ok(reward),
        Err(error) => err(error, debug),
    }
}

async fn update_reward(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRewardRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationReward> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut reward = load_reward(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            reward.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            reward.description = require_text(description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(cost) = input.points_cost {
            if cost < 1 || cost > 1_000_000 {
                return Err(UnifiedError::bad_request(
                    "points_cost must be between 1 and 1000000",
                ));
            }
            reward.points_cost = cost;
        }
        if let Some(active) = input.active {
            reward.active = active;
        }
        reward.updated_at = Utc::now();
        state.rewards.insert(reward.id, reward.clone());
        Ok(reward)
    })();
    match result {
        Ok(reward) => ok(reward),
        Err(error) => err(error, debug),
    }
}

async fn delete_reward(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let reward = load_reward(&state, &path.into_inner())?;
        if state.reward_requests.iter().any(|entry| {
            entry.value().reward_id == reward.id && entry.value().status == "requested"
        }) {
            return Err(UnifiedError::conflict(
                "A reward with open requests cannot be deleted; decide the requests first",
            ));
        }
        let id = reward.id;
        state.rewards.remove(&id);
        Ok(json!({ "deleted": true, "reward_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn request_reward(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationRewardRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let reward = load_reward(&state, &path.into_inner())?;
        if !reward.active {
            return Err(UnifiedError::conflict("This reward is not active"));
        }
        // No points are deducted on request — only on approval.
        // The request is refused up front when the balance cannot
        // cover the cost, so a request can never be approved into
        // a negative balance.
        let balance = user_balance(&state, &caller.actor, None, None);
        if balance < reward.points_cost {
            return Err(UnifiedError::conflict(format!(
                "Not enough points: this reward costs {} points and your balance is {balance}",
                reward.points_cost
            )));
        }
        if state.reward_requests.iter().any(|entry| {
            entry.value().reward_id == reward.id
                && entry.value().user_id == caller.actor
                && entry.value().status == "requested"
        }) {
            return Err(UnifiedError::conflict(
                "You already have an open request for this reward",
            ));
        }
        let now = Utc::now();
        let request = GamificationRewardRequest {
            id: Uuid::now_v7(),
            reward_id: reward.id,
            user_id: caller.actor.clone(),
            status: "requested".to_string(),
            points_spent: reward.points_cost,
            decided_by: None,
            decided_at: None,
            created_at: now,
            updated_at: now,
        };
        state.reward_requests.insert(request.id, request.clone());
        state.emit(
            "gamification.reward.requested",
            json!({
                "request_id": request.id.to_string(),
                "reward_id": reward.id.to_string(),
                "reward_name": reward.name,
                "user_id": request.user_id,
                "points_cost": reward.points_cost,
            }),
        );
        Ok(request)
    })();
    match result {
        Ok(request) => created(request),
        Err(error) => err(error, debug),
    }
}

async fn list_reward_requests(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let user_filter = params.filters.get("user_id").cloned();
        let reward_filter = params
            .filters
            .get("reward_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        let mut items: Vec<Value> = state
            .reward_requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| {
                if !is_manager && request.user_id != caller.actor {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &request.status != status {
                        return false;
                    }
                }
                if let Some(user) = &user_filter {
                    if &request.user_id != user {
                        return false;
                    }
                }
                if let Some(reward) = reward_filter {
                    if request.reward_id != reward {
                        return false;
                    }
                }
                true
            })
            .map(|request| {
                let reward_name = state
                    .rewards
                    .get(&request.reward_id)
                    .map(|entry| entry.value().name.clone());
                let mut value = serde_json::to_value(&request).unwrap_or(Value::Null);
                if let Value::Object(map) = &mut value {
                    map.insert(
                        "reward_name".to_string(),
                        reward_name.map(Value::String).unwrap_or(Value::Null),
                    );
                }
                value
            })
            .collect();
        items.sort_by(|a, b| {
            b.get("created_at")
                .and_then(Value::as_str)
                .cmp(&a.get("created_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_reward_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationRewardRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let request = load_reward_request(&state, &path.into_inner())?;
        if request.user_id != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::not_found(format!(
                "Reward request not found: {}",
                request.id
            )));
        }
        Ok(request)
    })();
    match result {
        Ok(request) => ok(request),
        Err(error) => err(error, debug),
    }
}

async fn approve_reward_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationRewardRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut request = load_reward_request(&state, &path.into_inner())?;
        if request.status != "requested" {
            return Err(UnifiedError::conflict(format!(
                "Only a requested reward can be approved (status is \"{}\")",
                request.status
            )));
        }
        // Deduct the points exactly once, on approval. The balance
        // is re-checked here because it may have changed since the
        // request was made.
        let balance = user_balance(&state, &request.user_id, None, None);
        if balance < request.points_spent {
            return Err(UnifiedError::conflict(format!(
                "Not enough points: the reward costs {} points and the user's balance is now {balance}",
                request.points_spent
            )));
        }
        let now = Utc::now();
        append_ledger_entry(
            &state,
            &request.user_id,
            None,
            None,
            None,
            "reward_redemption",
            -request.points_spent,
            None,
            Some(format!("Reward redemption: {}", request.reward_id)),
        );
        request.status = "approved".to_string();
        request.decided_by = Some(caller.actor.clone());
        request.decided_at = Some(now);
        request.updated_at = now;
        state
            .reward_requests
            .insert(request.id, request.clone());
        state.emit(
            "gamification.reward.approved",
            json!({
                "request_id": request.id.to_string(),
                "reward_id": request.reward_id.to_string(),
                "user_id": request.user_id,
                "points_spent": request.points_spent,
                "approved_by": caller.actor,
            }),
        );
        Ok(request)
    })();
    match result {
        Ok(request) => ok(request),
        Err(error) => err(error, debug),
    }
}

async fn reject_reward_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationRewardRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut request = load_reward_request(&state, &path.into_inner())?;
        if request.status != "requested" {
            return Err(UnifiedError::conflict(format!(
                "Only a requested reward can be rejected (status is \"{}\")",
                request.status
            )));
        }
        // Rejection deducts nothing: points were never spent.
        let now = Utc::now();
        request.status = "rejected".to_string();
        request.decided_by = Some(caller.actor.clone());
        request.decided_at = Some(now);
        request.updated_at = now;
        state
            .reward_requests
            .insert(request.id, request.clone());
        state.emit(
            "gamification.reward.rejected",
            json!({
                "request_id": request.id.to_string(),
                "reward_id": request.reward_id.to_string(),
                "user_id": request.user_id,
                "rejected_by": caller.actor,
            }),
        );
        Ok(request)
    })();
    match result {
        Ok(request) => ok(request),
        Err(error) => err(error, debug),
    }
}

async fn fulfill_reward_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationRewardRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut request = load_reward_request(&state, &path.into_inner())?;
        if request.status != "approved" {
            return Err(UnifiedError::conflict(format!(
                "Only an approved reward can be fulfilled (status is \"{}\")",
                request.status
            )));
        }
        request.status = "fulfilled".to_string();
        request.updated_at = Utc::now();
        state
            .reward_requests
            .insert(request.id, request.clone());
        state.emit(
            "gamification.reward.fulfilled",
            json!({
                "request_id": request.id.to_string(),
                "reward_id": request.reward_id.to_string(),
                "user_id": request.user_id,
            }),
        );
        Ok(request)
    })();
    match result {
        Ok(request) => ok(request),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct GamificationModule {
    state: GamificationState,
}

impl GamificationModule {
    pub fn new() -> Self {
        Self {
            state: GamificationState::default(),
        }
    }
}

impl Default for GamificationModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for GamificationModule {
    fn id(&self) -> &'static str {
        "gamification"
    }

    fn name(&self) -> &'static str {
        "Gamification"
    }

    fn description(&self) -> &'static str {
        "Staff recognition gamification: a configurable rules engine (action -> points, per company, daily caps), a points ledger that is the only source of truth for balances, badges awarded automatically on action thresholds with audited award records, time-boxed seasons with one active at a time per company, team leaderboards computed from the ledger, personal progress, and staff reward redemption with an explicit approve-then-deduct flow. Staff points are a separate ledger from customer loyalty points and are never mixed."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
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
        let state = self.state.clone();
        bus.register(
            TICKET_UPDATED,
            "gamification",
            Arc::new(move |ctx: &mut HookContext| {
                on_ticket_updated(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STORE_ORDER_COMPLETED,
            "gamification",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_completed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            ORDER_PAID,
            "gamification",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_completed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            SURVEY_RESPONSE_RECEIVED,
            "gamification",
            Arc::new(move |ctx: &mut HookContext| {
                on_survey_response_received(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            COMMUNITY_POST_REPLIED,
            "gamification",
            Arc::new(move |ctx: &mut HookContext| {
                on_community_post_replied(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            COMMUNITY_TOPIC_CREATED,
            "gamification",
            Arc::new(move |ctx: &mut HookContext| {
                on_community_topic_created(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/gamification")
                .app_data(state)
                .route("/rules", web::get().to(list_rules))
                .route("/rules", web::post().to(create_rule))
                .route("/rules/{id}", web::get().to(get_rule))
                .route("/rules/{id}", web::patch().to(update_rule))
                .route("/rules/{id}", web::delete().to(delete_rule))
                .route("/ledger", web::get().to(list_ledger))
                .route("/ledger/{id}", web::get().to(get_ledger_entry))
                .route("/balance", web::get().to(get_own_balance))
                .route("/balance/{user_id}", web::get().to(get_user_balance))
                .route("/points/award", web::post().to(award_points))
                .route("/points/adjust", web::post().to(adjust_points))
                .route("/badges", web::get().to(list_badges))
                .route("/badges", web::post().to(create_badge))
                .route("/badges/{id}", web::get().to(get_badge))
                .route("/badges/{id}", web::patch().to(update_badge))
                .route("/badges/{id}", web::delete().to(delete_badge))
                .route("/awards", web::get().to(list_awards))
                .route("/awards/{id}", web::get().to(get_award))
                .route("/seasons", web::get().to(list_seasons))
                .route("/seasons", web::post().to(create_season))
                .route("/seasons/{id}", web::get().to(get_season))
                .route("/seasons/{id}", web::patch().to(update_season))
                .route("/seasons/{id}/start", web::post().to(start_season))
                .route("/seasons/{id}/end", web::post().to(end_season))
                .route("/leaderboard", web::get().to(get_leaderboard))
                .route("/progress", web::get().to(get_own_progress))
                .route("/progress/{user_id}", web::get().to(get_user_progress))
                .route("/rewards", web::get().to(list_rewards))
                .route("/rewards", web::post().to(create_reward))
                .route("/rewards/{id}", web::get().to(get_reward))
                .route("/rewards/{id}", web::patch().to(update_reward))
                .route("/rewards/{id}", web::delete().to(delete_reward))
                .route("/rewards/{id}/request", web::post().to(request_reward))
                .route("/reward-requests", web::get().to(list_reward_requests))
                .route("/reward-requests/{id}", web::get().to(get_reward_request))
                .route(
                    "/reward-requests/{id}/approve",
                    web::post().to(approve_reward_request),
                )
                .route(
                    "/reward-requests/{id}/reject",
                    web::post().to(reject_reward_request),
                )
                .route(
                    "/reward-requests/{id}/fulfill",
                    web::post().to(fulfill_reward_request),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/gamification/rules", owner: "gamification", description: "List points rules (paginated, filter by action, company_id, active)." },
            RouteInfo { method: "POST", path: "/api/v1/gamification/rules", owner: "gamification", description: "Create a points rule: action, points, optional company, optional daily cap (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/rules/{id}", owner: "gamification", description: "Get one points rule." },
            RouteInfo { method: "PATCH", path: "/api/v1/gamification/rules/{id}", owner: "gamification", description: "Update a points rule (manager)." },
            RouteInfo { method: "DELETE", path: "/api/v1/gamification/rules/{id}", owner: "gamification", description: "Delete a points rule (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/ledger", owner: "gamification", description: "List ledger entries (own only unless manager; filter by user_id, action, company_id, season_id, department_id)." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/ledger/{id}", owner: "gamification", description: "Get one ledger entry (own only unless manager)." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/balance", owner: "gamification", description: "The caller's points balances (total and per season)." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/balance/{user_id}", owner: "gamification", description: "One user's points balances (own only unless manager)." },
            RouteInfo { method: "POST", path: "/api/v1/gamification/points/award", owner: "gamification", description: "Run the rules engine for one action for one user (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/gamification/points/adjust", owner: "gamification", description: "Direct ledger adjustment, positive or negative (manager; balance may never go below zero)." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/badges", owner: "gamification", description: "List badge definitions (paginated, filter by criteria_action)." },
            RouteInfo { method: "POST", path: "/api/v1/gamification/badges", owner: "gamification", description: "Define a badge with an action-count threshold (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/badges/{id}", owner: "gamification", description: "Get one badge definition." },
            RouteInfo { method: "PATCH", path: "/api/v1/gamification/badges/{id}", owner: "gamification", description: "Update a badge definition (manager)." },
            RouteInfo { method: "DELETE", path: "/api/v1/gamification/badges/{id}", owner: "gamification", description: "Delete a badge definition with no awards (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/awards", owner: "gamification", description: "List badge awards (own only unless manager; filter by user_id, badge_id)." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/awards/{id}", owner: "gamification", description: "Get one badge award (own only unless manager)." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/seasons", owner: "gamification", description: "List seasons (paginated, filter by status, company_id)." },
            RouteInfo { method: "POST", path: "/api/v1/gamification/seasons", owner: "gamification", description: "Create a season (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/seasons/{id}", owner: "gamification", description: "Get one season." },
            RouteInfo { method: "PATCH", path: "/api/v1/gamification/seasons/{id}", owner: "gamification", description: "Update an upcoming or active season (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/gamification/seasons/{id}/start", owner: "gamification", description: "Start a season; ends any other active season of the same company (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/gamification/seasons/{id}/end", owner: "gamification", description: "End an active season (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/leaderboard", owner: "gamification", description: "Ranked points per user, optionally per season, company, or department (computed from the ledger)." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/progress", owner: "gamification", description: "The caller's points, badges, and progress toward the next badges." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/progress/{user_id}", owner: "gamification", description: "One user's progress (own only unless manager)." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/rewards", owner: "gamification", description: "List staff rewards (paginated, filter by active)." },
            RouteInfo { method: "POST", path: "/api/v1/gamification/rewards", owner: "gamification", description: "Define a staff reward with a points cost (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/rewards/{id}", owner: "gamification", description: "Get one staff reward." },
            RouteInfo { method: "PATCH", path: "/api/v1/gamification/rewards/{id}", owner: "gamification", description: "Update a staff reward (manager)." },
            RouteInfo { method: "DELETE", path: "/api/v1/gamification/rewards/{id}", owner: "gamification", description: "Delete a staff reward with no open requests (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/gamification/rewards/{id}/request", owner: "gamification", description: "Request a reward (balance checked; no points deducted yet)." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/reward-requests", owner: "gamification", description: "List reward requests (own only unless manager; filter by status, user_id, reward_id)." },
            RouteInfo { method: "GET", path: "/api/v1/gamification/reward-requests/{id}", owner: "gamification", description: "Get one reward request (own only unless manager)." },
            RouteInfo { method: "POST", path: "/api/v1/gamification/reward-requests/{id}/approve", owner: "gamification", description: "Approve a reward request and deduct its points once (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/gamification/reward-requests/{id}/reject", owner: "gamification", description: "Reject a reward request; no points are deducted (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/gamification/reward-requests/{id}/fulfill", owner: "gamification", description: "Mark an approved reward request fulfilled (manager)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(GamificationModule::new()));
}
