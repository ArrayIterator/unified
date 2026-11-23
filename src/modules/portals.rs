// Client Portal (white-label) module — one branded portal per
// company: logo, colours, custom domain / subdomain, configurable
// pages (which sections are visible, in which order) and menus, plus
// read-only client data views (my invoices, tickets, downloads,
// subscriptions, licenses) that aggregate the owning modules'
// records.
//
// Routes live under `/api/v1/portals`:
//
//   Portals (admin):
//     GET/POST / — list portals / create a portal.
//     GET/PATCH/DELETE /{id}.
//     GET  /{id}/analytics — page-view analytics (paginated views
//       plus totals).
//   Portal menus (admin):
//     GET/POST /{id}/menus.
//     GET/PATCH/DELETE /{id}/menus/{menu_id}.
//     POST /{id}/menus/reorder — reorder menus by id list.
//   Public (no auth; see the public-access note below):
//     GET  /public/{slug} — public portal branding, visible
//       sections and menus; records a page view and emits
//       `portals.page.viewed`.
//   Client data (authenticated client, scoped server-side):
//     GET  /{slug}/my — the caller's portal home: branding plus a
//       per-section record count for the caller.
//     GET  /{slug}/my/invoices — the caller's invoices only.
//     GET  /{slug}/my/tickets — the caller's tickets only.
//     GET  /{slug}/my/downloads — the caller's downloads only.
//     GET  /{slug}/my/subscriptions — the caller's subscriptions only.
//     GET  /{slug}/my/licenses — the caller's licenses only.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory portal, menu, page-view, and read-only projection
// management; validated hex colours, slugs, hostnames, and section
// lists; per-company portals with unique slugs and unique custom
// domains; menu ordering and per-role menu visibility; client
// scoping enforced server-side on every `my/*` endpoint (the
// portal must belong to the caller's company and every record
// returned must belong to the caller); page-view recording with a
// per-portal cap; and `portals.created`, `portals.updated`,
// `portals.page.viewed`, and `portals.menu.updated` events on the
// hook bus. What is NOT built here: actual domain provisioning
// (the domains module owns domains — this module only stores the
// requested custom domain and emits `portals.domain.requested` so
// the domains module can verify and provision it; until the
// domains module claims that event, `domain_status` stays
// `requested`); a second auth system (client login uses the auth
// manager — this module resolves the caller server-side from the
// verified caller and the tenant context, and never trusts the
// `X-Company-Id` header, exactly like the events module); and live billing / ticket / store / licensing
// data: the `my/*` views are a read-only projection built from
// the owning modules' hook events (`invoice.created`,
// `invoice.paid`, `invoice.voided`, `ticket.created`,
// `ticket.updated`, `store.order.created`,
// `store.order.completed`, `store.order.cancelled`,
// `license.issued`, `license.activated`, `license.suspended`,
// `license.revoked`, `download.started`, `download.finished`).
// Records created before this module started, or announced without
// a company id and a user id, are not in the projection — the
// owning modules remain the source of truth and this module never
// writes back to them. The logo is metadata only (storage target
// + key, exactly like the events module cover image): the bytes
// live in the storage module.
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
// `GET /api/v1/portals/public/*`. The route is registered in
// `route_info()` with `route_version_support()` defaulting to
// `All`, so the API Manager's per-version registry picks it up. Do
// NOT treat the absence of a capability check on a public route as
// an open door: without the rule the API Manager denies the call.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Client data endpoints are
// company-scoped: the portal and every record must belong to the
// verified caller's company (the `X-Company-Id` header is never
// trusted). Capability checks use the `role.check` hook and
// fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `portals.created`, `portals.updated`, `portals.deleted`
//   - `portals.page.viewed`
//   - `portals.menu.updated` (menu created, updated, deleted, or
//     reordered)
//   - `portals.domain.requested` (a custom domain was set or
//     changed; payload `{"portal_id", "slug", "company_id",
//     "domain"}` for the domains module to verify and provision)
// Events this module listens to (read-only projection only; this
// module never writes back to the owning modules):
//   - `invoice.created`, `invoice.paid`, `invoice.voided`
//   - `ticket.created`, `ticket.updated`
//   - `store.order.created`, `store.order.completed`,
//     `store.order.cancelled`
//   - `license.issued`, `license.activated`, `license.suspended`,
//     `license.revoked`
//   - `download.started`, `download.finished`

pub mod ai_capabilities;
mod constants;
mod handlers;
mod helpers;
mod menus;
pub mod module;
mod projection;
mod public;
pub mod state;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::*;
pub use self::state::*;
pub use self::types::{
    CreateMenuRequest, CreatePortalRequest, Portal, PortalMenu, PortalRecord, PortalStatus,
    PortalView, ReorderMenusRequest, UpdateMenuRequest, UpdatePortalRequest,
};
