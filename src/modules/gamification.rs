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
//   - `gamification.points.earned` (per ledger entry with
//     positive points: `{"entry_id", "user_id", "action",
//     "points", "balance_after", "rule_id", "season_id"}`)
//   - `gamification.points.deducted` (per ledger entry with
//     negative points, same payload — deductions never emit
//     `earned`)
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
//     (`assigned_to`, falling back to `user_id`). A transition is
//     required (an update that carries a `previous_status` of
//     `resolved`/`closed` awards nothing), and one ticket awards
//     at most once per day.
//   - `store.order.completed`, `order.paid` — a completed order
//     awards the `sale_completed` action to its buyer
//     (`user_id`, falling back to `customer_id`). Both events fire
//     for one order; the order id is recorded on the ledger entry
//     (`source_ref`) and a repeat event for the same order awards
//     nothing.
//   - `surveys.response.received` — a submitted survey response
//     awards the `review_received` action to its recipient
//     (`recipient_id`).
//   - `community.post.replied` — a reply awards the
//     `community_help` action to its author (`author_id`,
//     falling back to `user_id`).
//   - `community.topic.created` — a new topic awards the
//     `topic_created` action to its author.

// Split into one file per concern under `src/modules/gamification/`.
// This root only declares the submodules and re-exports the
// public API at its original paths.

pub mod ai_capabilities;
pub mod constants;
pub mod types;
pub mod requests;
pub mod state;
pub mod helpers;
pub mod listeners;
pub mod rules;
pub mod ledger;
pub mod badges;
pub mod seasons;
pub mod leaderboard;
pub mod rewards;
pub mod module;

pub(crate) use self::constants::{MAX_NAME_LEN, MAX_DESCRIPTION_LEN, MAX_REASON_LEN, MAX_ACTION_LEN, PERM_GAMIFICATION_VIEW, PERM_GAMIFICATION_MANAGE, KNOWN_ACTIONS, TICKET_UPDATED, STORE_ORDER_COMPLETED, ORDER_PAID, SURVEY_RESPONSE_RECEIVED, COMMUNITY_POST_REPLIED, COMMUNITY_TOPIC_CREATED};
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::{GamificationRule, PointsLedgerEntry, GamificationBadge, GamificationAward, GamificationSeason, GamificationReward, GamificationRewardRequest};
pub use self::requests::{CreateRuleRequest, UpdateRuleRequest, AwardPointsRequest, AdjustPointsRequest, CreateBadgeRequest, UpdateBadgeRequest, CreateSeasonRequest, UpdateSeasonRequest, CreateRewardRequest, UpdateRewardRequest, RequestRewardScope};
pub use self::state::{GamificationState};
pub use self::helpers::{AwardOutcome};
pub(crate) use self::helpers::{
    Caller, action_count, active_season_for, append_ledger_entry, award_points_for_action, caller,
    caller_company, can_manage, check_badges, check_capability, created, err, in_company_scope,
    load_badge, load_reward, load_reward_request, load_rule, load_season, ok, opt_string, paginate,
    parse_uuid, points_from_rule_today, require_company_for_write, require_company_match,
    require_company_match_for, require_permission, require_record_in_company_scope,
    require_record_writable_in_company_scope, require_tenant, require_text, user_balance,
    valid_action,
};
pub(crate) use self::listeners::{on_ticket_updated, on_order_completed, on_survey_response_received, on_community_post_replied, on_community_topic_created};
pub(crate) use self::rules::{list_rules, create_rule, get_rule, update_rule, delete_rule};
pub(crate) use self::ledger::{list_ledger, get_ledger_entry, balance_view, get_own_balance, get_user_balance, award_points, adjust_points};
pub(crate) use self::badges::{list_badges, create_badge, get_badge, update_badge, delete_badge, list_awards, get_award};
pub(crate) use self::seasons::{list_seasons, create_season, get_season, update_season, start_season, end_season};
pub(crate) use self::leaderboard::{get_leaderboard, progress_view, get_own_progress, get_user_progress};
pub(crate) use self::rewards::{list_rewards, create_reward, get_reward, update_reward, delete_reward, request_reward, list_reward_requests, get_reward_request, approve_reward_request, reject_reward_request, fulfill_reward_request};
pub use self::module::{GamificationModule, register};
