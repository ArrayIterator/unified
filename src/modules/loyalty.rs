// Loyalty / Rewards module - customer points programs, earn rules,
// tiers, a ledger-backed points balance, and redemptions (points
// to a discount on an order, or points to a product).
//
// Routes live under `/api/v1/loyalty`:
//
//   Programs:
//     GET/POST /programs - list programs / create a program.
//     GET/PATCH/DELETE /programs/{id}.
//     POST /programs/{id}/activate, /programs/{id}/deactivate.
//   Earn rules:
//     GET/POST /programs/{id}/earn-rules.
//     GET/PATCH/DELETE /programs/{id}/earn-rules/{rule_id}.
//   Tiers:
//     GET/POST /programs/{id}/tiers.
//     GET/PATCH/DELETE /programs/{id}/tiers/{tier_id}.
//   Points:
//     POST /programs/{id}/earn - award points (manage).
//     POST /programs/{id}/adjust - signed adjustment (manage).
//     POST /programs/{id}/expire - run the expiry sweep (manage).
//     GET  /programs/{id}/balance/{customer_id} - balance + tier.
//     GET  /programs/{id}/balance/me - the caller's own balance.
//     GET  /programs/{id}/tier/{customer_id} - tier evaluation.
//     GET  /programs/{id}/ledger - ledger rows (paginated).
//   Redemptions:
//     POST /programs/{id}/redeem - spend points (owner or manage).
//     POST /redemptions/{redemption_id}/cancel - refund (manage).
//
// HONESTY - READ THIS FIRST. What is real in this module:
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
// store-side honouring of a redeemed discount - redemption emits
// `loyalty.discount.requested` (order id, discount amount,
// currency) for the store module, and the discount only takes
// effect once the store applies it. A discount redemption starts
// `pending` (points spent) until the store confirms it with
// `store.discount.applied` or fails it with
// `store.discount.failed` (which refunds the points); product
// redemptions complete at redeem time. Actual product fulfilment for
// points-to-product redemptions (the redemption row records the
// product; the store owns fulfilment); and any review earn
// automation - there is no review event to listen to yet, so
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
//   - `loyalty.redemption.pending`, `loyalty.redemption.completed`,
//     `loyalty.redemption.cancelled`
//   - `loyalty.discount.requested` (payload carries `order_id`,
//     `discount_amount`, `currency`, `customer_id`, and
//     `redemption_id` for the store module - the store owns
//     applying the discount to the order)
// Events this module listens to:
//   - `store.order.completed` - purchase earn under the active
//     program of the order's company (or the default program when
//     the order carries no company), using the active purchase
//     earn rule, its caps, and its minimum order amount.
//   - `user.created` - signup earn for the new customer, and a
//     referral earn for the referrer when the payload carries
//     `referred_by` (a customer user id).
//   - `store.discount.applied` - confirms a pending discount
//     redemption (by `redemption_id`, fallback `order_id`).
//   - `store.discount.failed` - cancels a pending discount
//     redemption and refunds its points via a new ledger row.

pub mod ai_capabilities;
mod constants;
mod handlers;
mod helpers;
mod hooks;
pub mod module;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::*;
pub use self::types::*;
