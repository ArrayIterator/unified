// Affiliate / Referral module - partner affiliates, referral links
// and codes, tracked conversions (click -> signup -> purchase),
// commissions (percent or fixed, per product or global default),
// payouts (requested through billing events; this module never
// moves money), affiliate dashboards, and fraud flags that are
// review-only and never auto-ban or auto-void.
//
// Routes live under `/api/v1/affiliates`:
//
//   Affiliates:
//     GET/POST / - list affiliates / apply as an affiliate.
//     GET/PATCH/DELETE /{id}.
//     POST /{id}/approve, /{id}/reject, /{id}/suspend,
//          /{id}/reactivate.
//   Links:
//     GET/POST /{id}/links - links of one affiliate.
//     GET/DELETE /links/{link_id}.
//   Clicks (public tracking endpoint, see the public-access note):
//     POST /track - record a click on a referral code.
//     GET  /{id}/clicks - clicks of one affiliate (filter flagged).
//   Conversions:
//     GET  /{id}/conversions - conversions of one affiliate.
//     POST /conversions - record a conversion manually (manage).
//   Commissions:
//     GET  /{id}/commissions - commissions of one affiliate.
//     POST /commissions/{commission_id}/approve.
//     POST /commissions/{commission_id}/void.
//   Payouts:
//     GET/POST /{id}/payouts - payout history / request a payout.
//     POST /payouts/{payout_id}/approve.
//     POST /payouts/{payout_id}/mark-paid.
//     POST /payouts/{payout_id}/reject.
//   Dashboard:
//     GET  /{id}/dashboard - clicks, conversions, earnings, payouts.
//     GET  /me/dashboard - the caller's own dashboard.
//
// HONESTY - READ THIS FIRST. What is real in this module:
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
// owns that - this module only emits
// `affiliates.payout.requested`); automatic approval or payment of
// anything (every commission approval and every payout step is a
// manual, capability-checked action); automatic banning or
// voiding on fraud (flags are advisory only, except that a
// refunded order voids its still-pending commissions - a refund
// is a business fact, not a fraud judgement, and it never touches
// approved or paid commissions); and attribution without a
// referral signal - a purchase is only attributed when the order
// payload carries a referral code, click id, or affiliate id (the
// store / checkout must forward the code the visitor arrived
// with). Without that signal no conversion is guessed.
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`, and denied outright when a rule exists
// with `allowed: false`). There is no reusable per-request
// "is this endpoint public?" helper to call from a handler - the
// check lives in the API Manager middleware / `resolve_public`
// path - so the public route below relies on that gate: an
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
//     `commission_ids` for the billing module - no money moves here)
//   - `affiliates.payout.approved`, `affiliates.payout.paid`,
//     `affiliates.payout.rejected`
//   - `affiliates.click.flagged` (advisory only; see fraud note)
// Events this module listens to:
//   - `user.created` - signup conversion when the payload carries
//     a referral code (`referral_code`), a click id (`click_id`),
//     or an affiliate id (`affiliate_id`); otherwise skipped.
//   - `store.order.completed` - purchase conversion when the order
//     payload carries the same referral signal; the order total
//     and first line-item product id drive the commission. The
//     `payment.captured` event is deliberately NOT consumed as
//     well: the store emits both for one purchase and consuming
//     both would attribute (and pay) the same order twice.
//   - `store.order.refunded` - voids still-pending commissions of
//     the refunded order (approved / paid commissions are never
//     touched by this listener).

pub mod ai_capabilities;
mod constants;
pub mod types;
mod helpers;
mod hooks;
pub mod module;
mod handlers_affiliates;
mod handlers_tracking;

// Public API (unchanged): re-exported from the split files.
// From types.rs:
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::Affiliate;
pub use self::types::AffiliateClick;
pub use self::types::AffiliateCommission;
pub use self::types::AffiliateCommissionRule;
pub use self::types::AffiliateConversion;
pub use self::types::AffiliateLink;
pub use self::types::AffiliatePayout;
pub use self::types::AffiliateStatus;
pub use self::types::AffiliatesState;
pub use self::types::CommissionStatus;
pub use self::types::CommissionType;
pub use self::types::CreateAffiliateRequest;
pub use self::types::CreateCommissionRuleRequest;
pub use self::types::CreateConversionRequest;
pub use self::types::CreateLinkRequest;
pub use self::types::MarkPaidRequest;
pub use self::types::PayoutStatus;
pub use self::types::RequestPayoutRequest;
pub use self::types::ReviewRequest;
pub use self::types::TrackClickRequest;
pub use self::types::UpdateAffiliateRequest;
// From module.rs:
pub use self::module::AffiliatesModule;
pub use self::module::register;
