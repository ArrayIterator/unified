// Referral module — employee referral: recruitment jobs
// marked referral-eligible with a per-job reward rule
// override or the tenant default rule, unique token-based
// shareable referral links per (employee, job) with click
// counts only, referrals with one entry per (candidate
// email, job), a fully data-driven pipeline (admin-defined
// stages with allowed transitions, audited moves), reward
// rules as data (amount in integer minor units, payout
// trigger stage, retention days, eligibility conditions)
// with rewards that become payable through events for
// billing / HR to pay out, anti-gaming rules as data
// (self-referral, same-candidate cooldown, per-period cap,
// active-employee check), and an append-only audit trail.
//
// Routes live under `/api/v1/referral`:
//
//   Public (token-gated — the token is the credential; see
//     the public-access note below):
//     GET  /public/links/{token} — follow a referral link
//       (click count only).
//   Job eligibility:
//     GET  /jobs/eligibility — list eligibility rows.
//     GET  /jobs/{job_id}/eligibility — one job's row.
//     PUT  /jobs/{job_id}/eligibility — set eligibility
//       and the per-job reward rule override.
//   Links:
//     GET/POST /links — list / create (one active link per
//       employee per job; raw token returned once).
//     GET  /links/{link_id}.
//     POST /links/{link_id}/rotate — fresh token.
//     DELETE /links/{link_id} — revoke.
//   Referrals:
//     GET/POST /referrals — list / submit.
//     GET/PATCH/DELETE /referrals/{referral_id}.
//     POST /referrals/{referral_id}/stage — audited move.
//     GET  /referrals/{referral_id}/history.
//   Rewards:
//     GET  /rewards — list.
//     GET  /rewards/{reward_id}.
//     POST /rewards/{reward_id}/mark-paid — record a payout
//       made elsewhere.
//     POST /rewards/{reward_id}/void.
//   Reward rules:
//     GET/POST /rules — list / create.
//     GET/PATCH/DELETE /rules/{rule_id}.
//   Pipeline stages:
//     GET/POST /stages — list / create.
//     GET/PATCH/DELETE /stages/{stage_id}.
//   Anti-gaming rules:
//     GET/POST /anti-gaming-rules — list / create.
//     GET/PATCH/DELETE /anti-gaming-rules/{rule_id}.
//   Audit:
//     GET  /audit — list the audit trail.
//   AI:
//     POST /ai/{capability} — one registered capability
//       through the AI module's gated runner.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory job-eligibility, link, stage, reward-rule,
// referral, stage-history, reward, anti-gaming-rule, and
// audit management; the pipeline state machine enforced by
// the stages' own allowed-next lists (no hard-coded stage
// names anywhere — a tenant with no stage rows cannot
// accept referrals); one referral per (candidate email,
// job) with case-insensitive duplicate detection; referral
// links following the sign module's token pattern (48 random
// alphanumeric characters, raw token returned exactly once
// at create / rotate, only the SHA-256 hash stored, rotate
// replaces, revoke disables); click tracking as counts
// only (no IP address, user agent, or fingerprint is
// stored); reward rules as data with integer minor-unit
// amounts and retention-day math; rewards created on
// reaching the trigger stage, payable through the
// `referral.reward.payable` event for billing / HR — this
// module never moves money, and mark-paid only records a
// payout made elsewhere; anti-gaming evaluation for the
// same-candidate cooldown and the per-period cap (real,
// from stored rows) with block-or-flag enforcement; tenant
// scoping on every row with cross-tenant access reading as
// 404, never 403. What is NOT built here: durable storage
// until the migration runs (the DashMap stores are
// authoritative; migration `2026-11-22-000120_referral` is
// the persistence target); any check that a `job_id` names
// a real recruitment job — jobs are plain reference ids,
// exactly like the recruitment module stores
// `department_id`; live verification of the referrer as an
// active employee — no `hr.employee.exists` hook exists in
// this tree (verified from source: HR answers only
// `user.manager.query` and emits `hr.employee_created`
// with `{employee_id, user_id, department_id}` and no
// tenant), so this module builds a registry from those
// events and records `unverified` whenever the referrer is
// not in it, never blocking on an unanswerable check and
// never claiming a pass it cannot prove; self-referral
// blocking by email — no user-email lookup hook exists, so
// the pure check is implemented and unit-tested but the
// handler records it as `unverified` until a lookup exists;
// recruitment linkage — `recruitment.applicant.moved`
// (`{applicant_id, job_id, from_stage, to_stage, actor}`)
// and `recruitment.hired` (`{applicant_id, job_id,
// offer_id, actor}`) carry no referral id (verified from
// source), so the listeners sync a stage only when a
// future payload adds `referral_id`, and otherwise
// acknowledge and skip; actual email / push delivery (the
// notifications module owns delivery — this module only
// emits `notification.requested` through its helper when a
// future flow needs it).
//
// Public-access gate: the API Manager module enforces public
// access through its admin-managed `ApiAccessRule` records
// (default deny: an endpoint is public only when a rule exists
// with `public_access: true`, and denied outright when a rule
// exists with `allowed: false`). There is no reusable
// per-request "is this endpoint public?" helper to call from a
// handler — the check lives in the API Manager middleware /
// `resolve_public` path — so the public route below relies on
// that gate: an administrator must create the matching
// `ApiAccessRule` row for
// `GET /api/v1/referral/public/links/{token}`. The route is
// registered in `route_info()` with
// `route_version_support()` defaulting to `All`, so the API
// Manager's per-version registry picks it up. Do NOT treat the
// absence of a capability check on a public route as an open
// door: without the rule the API Manager denies the call. The
// token itself is a second gate: unknown and revoked tokens all
// answer 404 with the same message, so tokens cannot be probed
// apart.
//
// Actor identity: the caller is the server-verified caller from
// `unified_core::caller::require_caller` (user id plus
// server-resolved role, both required for capability checks),
// exactly like the sign module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
// Handler signatures take `web::Data<Arc<AppContext>>` because
// the server registers `Arc<AppContext>` (see
// `src/http/server.rs`: `run_server(ctx: Arc<AppContext>)`);
// `web::Data<AppContext>` would fail to extract.
//
// Events emitted on the hook bus:
//   - `referral.link.created`
//   - `referral.job.eligibility_set`
//   - `referral.submitted`, `referral.stage_changed`,
//     `referral.hired`
//   - `referral.reward.payable`, `referral.reward.paid`,
//     `referral.reward.voided`
//   - `notification.requested` (helper available; no handler
//     sends one today — delivery belongs to the notifications
//     module when a future flow needs it).
// Events this module listens to:
//   - `user.deleted` — anonymizes the user's links,
//     referrals, rewards, and audit rows to `deleted-user`
//     (candidate rows only when the payload names the
//     candidate email); rows are never deleted; the
//     known-employee registry entry is removed.
//   - `hr.employee_created` — records the employee's user id
//     in the active-employee registry used by the
//     require-active-employee rule.
//   - `recruitment.applicant.moved`, `recruitment.hired` —
//     sync a referral's stage only when the payload carries
//     a `referral_id` (it does not today — see the honesty
//     note above).

pub mod ai_capabilities;
mod handlers_ai;
mod handlers_audit;
mod handlers_gaming;
mod handlers_jobs;
mod handlers_links;
mod handlers_referrals;
mod handlers_rewards;
mod handlers_rules;
mod handlers_stages;
mod helpers;
mod hooks;
mod module;
pub mod state;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::ReferralModule;
pub use self::module::register;
pub use self::state::ReferralState;
pub use self::types::{
    AntiGamingKind, AntiGamingRule, CreateAntiGamingRuleRequest, CreateLinkRequest,
    CreateReferralRequest, CreateRewardRuleRequest, CreateStageRequest, Enforcement,
    JobEligibility, JobReferralSummary, LinkCreated, PipelineStage, Referral, ReferralLink,
    ReferralStageHistory, Reward, RewardRule, RewardStatus, SetEligibilityRequest,
    TransitionReferralRequest, UpdateAntiGamingRuleRequest, UpdateReferralRequest,
    UpdateRewardRuleRequest, UpdateStageRequest,
};
