// Rental module — rentable products with data-driven per-period
// pricing, orders with a status machine, hand-out / return
// flows, damage reports, and deposit handling.
//
// Routes live under `/api/v1/rental`:
//
//   Public (no auth; see the public-access note below):
//     GET  /public/listings — active listings with their rates.
//     GET  /public/listings/{slug} — one active listing.
//   Listings:
//     GET/POST /listings — list listings / create a listing.
//     GET/PATCH/DELETE /listings/{listing_id}.
//     GET/POST /listings/{listing_id}/rates — list / create.
//     PATCH/DELETE /listings/{listing_id}/rates/{rate_id}.
//     GET  /listings/{listing_id}/availability
//       — ?starts_at=, ends_at=, quantity=.
//   Orders:
//     GET/POST /orders — list orders / create a draft order.
//     GET/PATCH/DELETE /orders/{order_id}.
//     POST /orders/{order_id}/reserve — draft -> reserved.
//     POST /orders/{order_id}/cancel — draft/reserved ->
//       cancelled.
//     POST /orders/{order_id}/complete — returned -> completed
//       (manager).
//     POST /orders/{order_id}/deposit — refund or deduct the
//       held deposit (manager).
//   Hand-out and return:
//     POST /orders/{order_id}/handover — reserved -> out, with
//       condition notes and photo metadata (manager).
//     POST /orders/{order_id}/return — out -> returned, late
//       fee computed from the listing rule (manager).
//   Deliveries and damage:
//     GET  /orders/{order_id}/deliveries — hand-out and return
//       records.
//     GET  /orders/{order_id}/deliveries/{delivery_id}.
//     GET/POST /orders/{order_id}/damage-reports — list / report.
//     GET  /orders/{order_id}/damage-reports/{report_id}.
//     POST /orders/{order_id}/damage-reports/{report_id}/resolve
//       — resolved (charges the order once) or dismissed.
//   Settings:
//     GET  /settings — per-tenant rental settings.
//     PATCH /settings (manager).
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory listing, rate, order, delivery, damage-report, and
// settings management; per-period pricing as data (each rate
// names its own period unit and length, so hourly, daily,
// weekly, monthly, and custom periods are all just rows);
// order pricing computed from the chosen rate with partial
// periods rounded up; an availability calendar that counts
// overlapping reserved/out/returned orders against the
// listing quantity and rejects double-booking at create and at
// reserve (back-to-back ranges do not overlap); the status
// machine draft -> reserved -> out -> returned -> completed
// plus cancelled, enforced by a single pure transition
// function; hand-out and return records with condition notes
// and photo metadata (document/storage references only, never
// file bytes); late fees computed from the listing's rule
// (basis, amount, grace hours, optional cap) at return; damage
// reports that add their estimated cost to the order total
// exactly once on resolve; deposit hold / refund / deduct /
// partial-refund handling; per-tenant settings (late-fee grace,
// advance booking window, cancel cutoff); tenant scoping on
// every row with cross-tenant access reading as 404, never
// 403; and customers seeing only their own orders. What is NOT
// built here: durable storage until the migration runs (the
// DashMap stores are authoritative; migration
// `2026-11-22-000111_rental` is the persistence target);
// money movement (the payments module owns capture and refund
// — this module only records deposit state from
// `payment.captured` / `payment.refunded`); shipment creation
// (the logistics module owns shipments — this module only
// records the shipment id from
// `logistics.shipment.delivered`); the rental agreement
// document itself (the contracts module owns it — this module
// links it by id and marks it signed from
// `contracts.signed`); and email / push delivery (the
// notifications module owns delivery — this module only emits
// `notification.requested`).
//
// Public-access gate: the API Manager module enforces public
// access through its admin-managed `ApiAccessRule` records
// (default deny: an endpoint is public only when a rule exists
// with `public_access: true`, and denied outright when a rule
// exists with `allowed: false`). There is no reusable
// per-request "is this endpoint public?" helper to call from a
// handler — the check lives in the API Manager middleware /
// `resolve_public` path — so the public routes below rely on
// that gate: an administrator must create the matching
// `ApiAccessRule` rows for `GET /api/v1/rental/public/*`. The
// routes are registered in `route_info()` with
// `route_version_support()` defaulting to `All`, so the API
// Manager's per-version registry picks them up. Do NOT treat
// the absence of a capability check on a public route as an
// open door: without the rule the API Manager denies the call.
//
// Actor identity: the caller is the server-verified caller from
// `unified_core::caller::require_caller` (user id plus
// server-resolved role, both required for capability checks),
// exactly like the forums module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `rental.listing.created`, `rental.listing.updated`,
//     `rental.listing.deleted`
//   - `rental.rate.created`, `rental.rate.updated`,
//     `rental.rate.deleted`
//   - `rental.order.created`, `rental.order.updated`,
//     `rental.order.deleted`, `rental.order.reserved`,
//     `rental.order.cancelled`, `rental.order.out`,
//     `rental.order.returned`, `rental.order.late`,
//     `rental.order.completed`
//   - `rental.deposit.updated`
//   - `rental.damage.reported`, `rental.damage.resolved`,
//     `rental.damage.dismissed`
//   - `notification.requested` (order created, out, returned,
//     in the notifications module's payload shape).
// Events this module listens to:
//   - `user.deleted` — anonymizes the user's orders, damage
//     reports, and deliveries to `deleted-user`.
//   - `payment.captured` — marks the referenced order's
//     deposit held.
//   - `payment.refunded` — marks the referenced order's
//     deposit refunded (or partially refunded when the payload
//     amount is below the held deposit).
//   - `logistics.shipment.delivered` — records the shipment id
//     on the referenced order.
//   - `contracts.signed` — marks the rental agreement signed
//     on every order linked to that contract id.

pub mod ai_capabilities;
mod handlers_fulfilment;
mod handlers_listings;
mod handlers_orders;
mod handlers_settings;
mod helpers;
mod hooks;
mod module;
pub mod state;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::register;
pub use self::module::RentalModule;
pub use self::state::RentalState;
pub use self::types::CreateDamageReportRequest;
pub use self::types::CreateListingRequest;
pub use self::types::CreateOrderRequest;
pub use self::types::CreateRateRequest;
pub use self::types::DepositRequest;
pub use self::types::HandoverRequest;
pub use self::types::LateFeeRule;
pub use self::types::RentalDamageReport;
pub use self::types::RentalDelivery;
pub use self::types::RentalListing;
pub use self::types::RentalOrder;
pub use self::types::RentalOrderStatus;
pub use self::types::RentalRate;
pub use self::types::RentalSettings;
pub use self::types::ResolveDamageReportRequest;
pub use self::types::ReturnRequest;
pub use self::types::UpdateListingRequest;
pub use self::types::UpdateOrderRequest;
pub use self::types::UpdateRateRequest;
pub use self::types::UpdateSettingsRequest;
