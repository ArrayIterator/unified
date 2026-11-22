// Per-product feature limiting (entitlements) module.
//
// The selling model: a tenant (company) may buy one product at a
// time — one module, or one SaaS package bundling several modules —
// and the tenant dashboard may activate only what was purchased.
// An entitlement is DATA, never code: one row grants one module to
// one tenant from one admin-defined source. The effective
// entitlement for a module is the UNION of its active sources: any
// active source grants the module. No entitlement record means not
// entitled (fail closed).
//
// Sources (`source_kind`, a closed taxonomy — the sources
// themselves are admin-defined records):
//   - `owner_entitlement` — the implicit owner entitlement for a
//                       sellable CORE module (the same rule as
//                       `licensing::check_module_entitlement`).
//   - `license`       — a licensing-module license whose product
//                       names the module (composed through
//                       `licensing::has_module_entitlement`, never
//                       re-implemented here).
//   - `subscription`  — a billing/store subscription backing the
//                       module.
//   - `saas_package`  — a SaaS package purchase (see
//                       `crate::modules::saas_packages`).
//   - `manual_grant`  — an administrator grant, with a reason.
//
// This module is the GATE, not a second activator: enabling a
// module checks the entitlement first and then fires the existing
// `module.enabled` hook so the Extensions Manager path does the
// actual activation. It never enables a module by writing the
// Extensions Manager's state itself.
//
// Downgrade / expiry invariant (read this before touching the lapse
// flow): TENANT DATA IS NEVER DELETED by this flow. When an
// entitlement lapses the module first becomes READ-ONLY for a
// configurable grace period (data export still allowed; writes are
// rejected with `entitlement.grace_read_only`), then DISABLED after
// the grace period. No delete call exists anywhere in that flow,
// and no handler in this file issues a DELETE against any tenant
// data table. Only an administrator may delete a manual-grant row
// itself, and the history rows describing it are kept.
//
// Routes live under `/api/v1/entitlements`:
//   Tenant dashboard (own tenant, or any tenant for an admin):
//     GET    /my-modules — every registered module with its
//            per-tenant state (entitled with source + expiry, or
//            locked with an upgrade path), paginated.
//     GET    /modules/{module_id}/access — one module's access
//            state: entitled / grace_read_only / locked, and
//            whether writes are allowed.
//     POST   /modules/{module_id}/enable — enable after the
//            server-side entitlement check (403
//            `entitlement.required` + audited attempt when not
//            entitled).
//     POST   /modules/{module_id}/disable — disable a module the
//            tenant enabled.
//   Package / product-set changes:
//     POST   /package-changes/apply — compute the module diff for
//            a new package/product set (newly entitled, kept,
//            kept via another source, requires_confirmation).
//            Newly entitled modules are enabled; modules that
//            would be lost are NEVER silently disabled when the
//            tenant uses them outside the package.
//     GET    /package-changes — list change records (paginated).
//     GET    /package-changes/{id} — one change record.
//     POST   /package-changes/{id}/confirm — explicitly confirm
//            disabling the modules listed as
//            `requires_confirmation` (or discard the change).
//   Admin:
//     GET    / — list entitlements (paginated, filter by
//            company_id / module_id / source_kind / status).
//     POST   /grants — create a manual grant (reason required).
//     GET    /{id} — one entitlement.
//     PATCH  /{id} — revoke / extend / change a grant.
//     DELETE /{id} — delete a manual-grant row (admin only;
//            history is kept; never tenant data).
//     GET    /history — entitlement history per tenant/module
//            (paginated, filter by company_id / module_id /
//            action).
//     GET    /denied-attempts — denied enable attempts (paginated,
//            filter by company_id / module_id).
//     GET    /settings — entitlement settings (view).
//     PUT    /settings — update settings (manage).
//     POST   /sweep — run the lapse/grace sweep now (manage).
//   AI (smart module, suggestion only — never purchases):
//     POST   /ai/suggest — suggest which package unlocks the
//            modules this tenant tried to use most, from the
//            denied-attempt audit log.
//     GET    /ai/capabilities — the AI capabilities this module
//            offers.
//
// Events emitted on the hook bus:
//   - `entitlement.granted` — `{"company_id", "module_id",
//     "source_kind", "source_id", "entitlement_id", "expires_at"}`
//   - `entitlement.revoked` — `{"company_id", "module_id",
//     "source_kind", "source_id", "entitlement_id", "reason"}`
//   - `entitlement.grace_started` — `{"company_id", "module_id",
//     "entitlement_id", "grace_ends_at", "reason"}`
//   - `entitlement.disabled` — `{"company_id", "module_id",
//     "entitlement_id", "reason"}`
//   - `entitlement.enable_denied` — `{"company_id", "module_id",
//     "actor_id", "reason", "error_code"}`
//   - `module.enabled` / `module.disabled` (core events) when a
//     module is enabled/disabled for a tenant through this module.
//   - `module.entitlement.granted` / `module.entitlement.denied`
//     (core constants) are emitted by the licensing check this
//     module composes with; this module does not re-emit them.
//   - `entitlement.upgrade_path_requested` — hook QUERY emitted
//     when a locked module needs an upgrade path:
//     `{"company_id", "module_id", "upgrade_paths": []}`.
//     A handler (SaaS packages / products) may set
//     `upgrade_paths` to `[{"kind", "id", "name"}]`; when nothing
//     answers, the path is null with reason
//     `no_upgrade_path_provider` — never a fabricated product.
//   - `entitlement.module_usage_requested` — hook QUERY emitted
//     by the package-change diff: `{"company_id", "module_id",
//     "used_outside_package": false}`. A handler that knows the
//     tenant uses the module outside the package sets the flag;
//     when nothing answers, the diff treats the module as used
//     (fail-safe: it lands in `requires_confirmation`).
//   - `entitlements.ai.suggest_requested` — hook QUERY the AI
//     endpoint emits so the AI module can ground or enrich the
//     suggestion: `{"company_id", "denied_modules": [...]}`.
//     This module answers it from the denied-attempt log.
// Events this module listens to (all read defensively — a missing
// field or an unknown record is skipped, never an error):
//   - `license.revoked`, `license.expiring` (licensing) —
//     entitlements sourced from that license lapse.
//   - `subscription.cancelled`, `subscription.expired` (store /
//     billing) — entitlements sourced from that subscription lapse.
//   - `saas_packages.cancelled` — entitlements sourced from that
//     package purchase lapse.
//   - `tenant.module.granted` (tenants) — mirror the grant into a
//     stored entitlement row (`granted_via` maps onto the source
//     kinds; `preset` and `builtin` map onto nothing, see the
//     handler). `tenant.module.revoked` — the module's non-manual
//     rows lapse into grace; manual grants survive it.
//   - `entitlements.check_requested` and
//     `saas_packages.entitlement.check_requested` — hook QUERY:
//     `{"company_id", "module_id", "allowed": false}`; answered
//     with the effective state (this is the answer the SaaS
//     packages provisioning step waits for).
//   - `entitlements.write.check_requested` — hook QUERY:
//     `{"company_id", "module_id", "write_allowed": false}`;
//     answered false while the module is locked or in read-only
//     grace.
//   - `entitlements.grant_requested` — hook COMMAND: another
//     module (SaaS packages provisioning, store checkout) asks
//     this module to record a grant:
//     `{"company_id", "module_id", "source_kind", "source_id",
//     "source_ref", "expires_at", "actor_id"}`. Answered with
//     `granted: true` and the entitlement id, or `granted: false`
//     with a reason.
//   - `entitlements.ai.suggest_requested` — answered as above.
//
// Persistence: the live data lives in in-memory DashMap stores
// (the same approach the licensing and SaaS-packages modules use).
// The Diesel tables for durable persistence are the migration
// `2026-11-22-000092_entitlements`; the schema and model entries
// are supplied to the integrator in
// INTEGRATION_NOTES_BATCH15_B1.md. Nothing in this file references
// a table that does not exist yet, so it compiles against the
// current schema.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (actor) and
// `X-Role` (role slug the capability check runs against), with
// `?viewer_id=` / `?viewer_role=` query fallbacks, exactly like
// the licensing and AI modules. Capability checks use the
// `role.check` hook and fail closed (no answer means not
// allowed). This is the known gap the licensing module documents:
// the headers arrive as trusted input until the auth extractor
// lands.
//
// TENANT-SCOPED DATA ACCESS (standing rule). Every tenant-scoped
// endpoint in this module resolves its tenant ONLY through the
// tenant context (`TenantScope::resolve`, backed by
// `TenantContext::from_request` — the context the
// `TenantResolutionMiddleware` attaches from the Host header).
// The dashboard listing (`GET /my-modules`) and every other
// tenant endpoint fail closed with 403 `tenant.required` when no
// tenant context is present: they never list another tenant's
// modules and never fall back to the `public` schema. A requested
// `company_id` that differs from the context's company is also a
// 403. No tenant id, schema name, or package id is hard-coded
// anywhere in this file.
//   - Entitlement grants themselves derive from control-plane
//     sources (licenses, subscriptions, SaaS packages) and are
//     read ONLY through those modules' events and APIs
//     (`licensing::has_module_entitlement`, `license.revoked`,
//     `subscription.cancelled` / `.expired`,
//     `saas_packages.cancelled`, `entitlements.grant_requested`)
//     — this module never queries the licenses, subscriptions,
//     or packages tables directly (it references no control-plane
//     table at all; its only Diesel surface is its own migration
//     tables, supplied as snippets for the integrator).
//   - The per-tenant activation state (`entitlement_module_
//     activations`) and every entitlement/history/denied row IS
//     tenant business data: each row carries the tenant namespace
//     id (`tenant_id`, uuidv7) alongside the company key, is
//     written only under a resolved tenant scope, and its durable
//     tables (`2026-11-22-000092_entitlements`) are TENANT-SCHEMA
//     tables — created per tenant schema by the migration runner
//     with the search_path set to that tenant's schema — never
//     `public` tables. Control-plane tables (tenants,
//     subscriptions, domains, billing, licensing) stay in
//     `public` and are accessed only through their own
//     modules/events.
//   - Admin endpoints (`GET /entitlements`, grants, history,
//     denied-attempts, settings, sweep) run on the control plane
//     under `entitlements:manage` / `entitlements:grant`; they
//     take no tenant scope from a request and name the tenant
//     explicitly in the filter or body.
// CONVERGENCE with batch 17 (tenancy plumbing), verified on disk
// at the time of writing — the plumbing EXISTS now:
//   - `crate::core::tenancy` (`src/core/tenancy.rs`): the single
//     source of truth for schema-per-tenant isolation, with
//     `TenantContext { tenant_id: Uuid, schema_name,
//     company_id: Option<String>, site_id: Option<String> }`,
//     `TenantContext::from_http_request(req)` (fail-closed
//     extractor: 401 when no tenant context, 403 when the tenant
//     id is not a UUID), `schema_name(Uuid)` — the ONLY place a
//     tenant schema name is ever built (`tenant_` + 32 lowercase
//     hex), `is_valid_schema_name`, `schema_name_for_str`,
//     `with_tenant_connection` (the ONLY sanctioned Diesel path
//     into a tenant schema: checkout, transaction, `SET LOCAL`
//     search_path, closure, commit), `set_tenant_search_path`,
//     `run_tenant_migrations`, `schema_exists`,
//     `drop_tenant_schema`, and `ControlPlaneGuard` for
//     control-plane handlers. This module resolves tenants ONLY
//     through `TenantContext::from_http_request` (mapped to 403
//     `tenant.required` per the standing rule) and never builds
//     a schema name itself — it reports the derived
//     `schema_name` the context already carries.
//   - The middleware underneath is `TenantResolutionMiddleware`
//     (priority 950, wired in `src/http/server.rs`), which
//     resolves the Host header through `tenant.resolve_requested`
//     and attaches the resolved context (`middleware::
//     TenantContext`) the tenancy extractor reads.
//   - Migrations are embedded with `embed_migrations!
//     ("migrations")`, and `run_tenant_migrations` runs the SAME
//     embedded set inside each tenant schema, so this module's
//     `000092_entitlements` migration is applied per tenant
//     schema automatically once the integrator runs tenant
//     provisioning — no per-module registration is needed for it.
//   - `tenant_module_grants` (migration `000094_tenant_
//     isolation`, control-plane `public` table, owned by the
//     tenants module) is the DURABLE grant record: which modules
//     a tenant was granted and via what, with soft-delete
//     revocation. This module's tables are the RUNTIME gate
//     record in the tenant schema (grace, diff, history, denied
//     attempts). The two agree through events: this module
//     listens to `tenant.module.granted` / `.revoked` and mirrors
//     them into stored rows, and the tenants module gates its
//     own grant step on `licensing::check_module_entitlement`.
//     Overlap note: a grant recorded in `tenant_module_grants`
//     with `granted_via` `preset` has no counterpart row here
//     (a preset is a settings bundle, not a purchase source);
//     `builtin` needs none (non-sellable modules need no
//     entitlement). The tenants module is NOT registered in
//     `src/modules/mod.rs` yet, so none of its events fire until
//     it registers — same status as `saas_packages`.
//   - REMAINING GAP: the in-memory DashMap stores used this
//     round are process-global keyed by company id, not
//     per-schema Diesel reads/writes through
//     `with_tenant_connection`; the durable per-schema wiring
//     (schema/models entries in the notes) replaces them at
//     integration time. Until then a restart loses grants made
//     since the last durable write — the same limitation the
//     licensing and SaaS-packages modules carry.
//   - ALSO OPEN: the admin grant endpoint defaults `tenant_id`
//     to `company_id` when the administrator does not name it
//     (documented at the call site). In practice the tenants
//     module uses the tenant id itself as the company id, so the
//     default is exact for tenant-originated grants; a
//     company -> tenant-namespace lookup replaces it if a
//     company ever differs from its tenant id.
//
// CONVERGENCE with batch 14 (`saas_packages.rs`, which exists on
// disk but is NOT registered in `src/modules/mod.rs`):
//   - Migration 000091 is claimed in that file's header as its
//     persistence target, so this module uses 000092 (verified:
//     no 000092 directory existed when this file was written).
//   - Upgrade paths are resolved through the
//     `entitlement.upgrade_path_requested` hook query, because
//     `saas_packages` exposes no public accessor for its
//     published packages and is not registered, so it cannot be a
//     `requires` dependency or a direct call. The durable fix is
//     a SaaS-packages listener for that event (or a public
//     `published_packages` accessor), after which locked modules
//     resolve to the real packages that bundle them.
//   - Package purchases grant entitlements through the
//     `entitlements.grant_requested` hook command, and cancel
//     through the `saas_packages.cancelled` event this module
//     already listens to. Until the provisioning step emits the
//     grant command, a purchase enables modules without an
//     entitlement row here, and this module's diff treats those
//     modules as entitled only through the licensing check —
//     recorded as OPEN below.
//   - The AI contract (`AiCapability`, `register_ai_capabilities`,
//     `run_ai_capability` in `crate::modules::ai`) does NOT exist
//     in `ai.rs` at the time of writing (verified: no definition
//     matches in `ai.rs`, although `exams.rs`, `library.rs`, and
//     `saas_packages.rs` import those names). This file therefore
//     deliberately does NOT import them: the AI surface here is
//     the `entitlements.ai.suggest_requested` hook plus the
//     `GET /ai/capabilities` data endpoint, which compile without
//     the contract. When the AI-contract builder lands those
//     items, register the `suggest_requested` capability there and
//     have the AI endpoint delegate through `run_ai_capability`.
//
// OPEN — known gaps, recorded honestly instead of hidden:
//   - `cargo check` has never run in this environment (no Rust
//     toolchain, crates.io blocked), so compilation is NOT
//     verified; see the builder report for the verification that
//     was done instead (read-back, counts, greps).
//   - The Extensions Manager has no hook subscriber for
//     `module.enabled` today (verified: its enable path is the
//     HTTP endpoint `POST /api/v1/extensions/{id}/enable` only),
//     so firing the event is a handoff, not an activation: a
//     dashboard that enables through this module should also
//     call the Extensions enable endpoint (or the Extensions
//     Manager gains a listener). The entitlement gate itself is
//     enforced here regardless.
//   - Lapse payloads are read defensively (verified shapes:
//     `license.revoked` carries `license_id` and no company;
//     `subscription.cancelled` carries `subscription_id` plus a
//     nested `subscription` object; `saas_packages.cancelled`
//     carries `purchase_id`, `package_id`, `company_id`;
//     `tenant.module.granted` / `.revoked` carry `tenant_id`,
//     `company_id`, `module_id`, `granted_via` / `reason`). A
//     lapse whose payload names no matching source id is skipped,
//     and the sweep endpoint is the backstop.
//   - Owner-entitlement consequence: the licensing rule this
//     module composes with grants every sellable CORE module to
//     every tenant implicitly, so per-product limiting bites
//     hardest on sellable NON-core (extension) modules. If the
//     product model ever intends core modules to be individually
//     sold, the owner-entitlement rule in
//     `licensing::check_module_entitlement` must change first;
//     this module cannot override it from here.
//   - Fail-closed is an invariant, not a setting: the
//     `default_deny` setting records the admin's chosen policy
//     and is enforced for unregistered-module lookups, but no
//     entitlement record always means not entitled, whatever the
//     setting says. Setting it to false does not grant anything.
//   - The in-memory stores are authoritative this round; a
//     restart loses grants made since the last durable write, the
//     same limitation the licensing and SaaS-packages modules
//     carry until the Diesel wiring lands.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{
    Module, ModuleRegistry, RouteInfo, MODULE_ENTITLEMENT_DENIED_EVENT,
};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::tenancy::TenantContext;
use crate::modules::licensing::{has_module_entitlement, LICENSE_EXPIRING};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Events, capabilities, vocabularies, limits.
// ---------------------------------------------------------------------------

/// Emitted when an entitlement is created or a lapsed module is
/// re-granted. Payload: `{"company_id", "module_id",
/// "source_kind", "source_id", "entitlement_id", "expires_at"}`.
pub const EVENT_ENTITLEMENT_GRANTED: &str = "entitlement.granted";
/// Emitted when an entitlement is revoked by an administrator or by
/// a lapsed source. Payload: `{"company_id", "module_id",
/// "source_kind", "source_id", "entitlement_id", "reason"}`.
pub const EVENT_ENTITLEMENT_REVOKED: &str = "entitlement.revoked";
/// Emitted when a lapsed module enters read-only grace.
/// Payload: `{"company_id", "module_id", "entitlement_id",
/// "grace_ends_at", "reason"}`.
pub const EVENT_ENTITLEMENT_GRACE_STARTED: &str = "entitlement.grace_started";
/// Emitted when a module leaves grace and is disabled. Payload:
/// `{"company_id", "module_id", "entitlement_id", "reason"}`.
pub const EVENT_ENTITLEMENT_DISABLED: &str = "entitlement.disabled";
/// Emitted when an enable attempt is refused for lack of
/// entitlement. Payload: `{"company_id", "module_id", "actor_id",
/// "reason", "error_code"}`.
pub const EVENT_ENTITLEMENT_ENABLE_DENIED: &str = "entitlement.enable_denied";

/// Hook query: ask the product/package side which sellable product
/// or package unlocks a module. Payload: `{"company_id",
/// "module_id", "upgrade_paths": []}`.
pub const EVENT_UPGRADE_PATH_REQUESTED: &str = "entitlement.upgrade_path_requested";
/// Hook query: does the tenant use this module outside the package
/// being replaced? Payload: `{"company_id", "module_id",
/// "used_outside_package": false}`.
pub const EVENT_MODULE_USAGE_REQUESTED: &str = "entitlement.module_usage_requested";
/// Hook query answered by this module: is the tenant entitled to
/// the module? Payload: `{"company_id", "module_id",
/// "allowed": false}`.
pub const EVENT_CHECK_REQUESTED: &str = "entitlements.check_requested";
/// The same query as emitted by the SaaS packages module during
/// provisioning. Payload: `{"company_id", "module_id",
/// "allowed": false}`.
pub const EVENT_SAAS_ENTITLEMENT_CHECK_REQUESTED: &str =
    "saas_packages.entitlement.check_requested";
/// Hook query answered by this module: may the tenant write to the
/// module (false while locked or in read-only grace)? Payload:
/// `{"company_id", "module_id", "write_allowed": false}`.
pub const EVENT_WRITE_CHECK_REQUESTED: &str = "entitlements.write.check_requested";
/// Hook command: record a grant on behalf of another module.
/// Payload: `{"company_id", "module_id", "source_kind",
/// "source_id", "source_ref", "expires_at", "actor_id"}`.
pub const EVENT_GRANT_REQUESTED: &str = "entitlements.grant_requested";
/// Hook query the AI endpoint emits so the AI module can ground or
/// enrich the package suggestion. Payload: `{"company_id",
/// "denied_modules": [{"module_id", "denied_attempts"}]}`.
pub const EVENT_AI_SUGGEST_REQUESTED: &str = "entitlements.ai.suggest_requested";

/// Events this module listens to for lapse handling. The licensing
/// `license.expiring` constant lives in the licensing module; the
/// subscription and package events are plain strings because no
/// core constant names them.
const LICENSE_REVOKED_EVENT: &str = events::LICENSE_REVOKED;
const SUBSCRIPTION_CANCELLED_EVENT: &str = "subscription.cancelled";
const SUBSCRIPTION_EXPIRED_EVENT: &str = "subscription.expired";
const SAAS_PACKAGE_CANCELLED_EVENT: &str = "saas_packages.cancelled";
/// The tenants module grants a module to a tenant. Payload:
/// `{"tenant_id", "company_id", "module_id", "granted_via",
/// "actor_id"}`. The constants live as plain strings (not imports)
/// because the tenants module is not registered in
/// `src/modules/mod.rs` yet, so its items are not importable.
const TENANT_MODULE_GRANTED_EVENT: &str = "tenant.module.granted";
/// The tenants module revokes a module grant. Payload:
/// `{"tenant_id", "company_id", "module_id", "reason", "actor_id"}`.
const TENANT_MODULE_REVOKED_EVENT: &str = "tenant.module.revoked";

/// Capability names checked through the `role.check` hook.
pub const PERM_ENTITLEMENTS_READ: &str = "entitlements:read";
pub const PERM_ENTITLEMENTS_MANAGE: &str = "entitlements:manage";
pub const PERM_ENTITLEMENTS_GRANT: &str = "entitlements:grant";

/// Entitlement source kinds. Closed vocabulary: the kinds are the
/// fixed taxonomy; the sources themselves are admin-defined
/// records, never code lists.
pub const SOURCE_LICENSE: &str = "license";
pub const SOURCE_SUBSCRIPTION: &str = "subscription";
pub const SOURCE_SAAS_PACKAGE: &str = "saas_package";
pub const SOURCE_MANUAL_GRANT: &str = "manual_grant";
/// The implicit owner entitlement for a sellable core module
/// (see `licensing::check_module_entitlement`): a core module
/// ships with the platform, so the company running this
/// installation already holds it. Never a purchasable source and
/// never created by the admin grant endpoint (which always writes
/// `manual_grant`); it is derived at read time, and mirrored into
/// a stored row only when the tenants module grants the module
/// with `granted_via` `owner_entitlement`.
pub const SOURCE_OWNER_ENTITLEMENT: &str = "owner_entitlement";
const SOURCE_KINDS: &[&str] = &[
    SOURCE_LICENSE,
    SOURCE_SUBSCRIPTION,
    SOURCE_SAAS_PACKAGE,
    SOURCE_MANUAL_GRANT,
    SOURCE_OWNER_ENTITLEMENT,
];

/// Entitlement lifecycle statuses. Closed vocabulary.
pub const STATUS_ACTIVE: &str = "active";
pub const STATUS_GRACE: &str = "grace";
pub const STATUS_REVOKED: &str = "revoked";
pub const STATUS_EXPIRED: &str = "expired";
pub const STATUS_DISABLED: &str = "disabled";

/// Effective per-module access states reported to callers.
pub const ACCESS_ENTITLED: &str = "entitled";
pub const ACCESS_GRACE_READ_ONLY: &str = "grace_read_only";
pub const ACCESS_LOCKED: &str = "locked";

/// Machine-readable error codes carried in error metadata under
/// `error_code` (the envelope's `code` stays the HTTP status).
pub const ERROR_ENTITLEMENT_REQUIRED: &str = "entitlement.required";
pub const ERROR_ENTITLEMENT_GRACE_READ_ONLY: &str = "entitlement.grace_read_only";

/// Configurable defaults. Every value below is a DEFAULT: an
/// administrator may change it through `PUT /settings`, and the
/// live settings value is what the code reads.
const DEFAULT_GRACE_PERIOD_DAYS: i64 = 14;
const MAX_GRACE_PERIOD_DAYS: i64 = 365;
const MAX_ID_LEN: usize = 128;
const MAX_MODULE_ID_LEN: usize = 64;
const MAX_SOURCE_ID_LEN: usize = 256;
const MAX_REASON_LEN: usize = 1000;
const MAX_ACTOR_LEN: usize = 128;
const MAX_MODULES_PER_CHANGE: usize = 500;
const DENIED_LOOKBACK_DAYS: i64 = 30;
const MAX_SUGGESTIONS: usize = 5;

/// Validate a module id: lowercase letters, digits, underscore,
/// 1..=64 characters. Module ids come from the registry, so an
/// unknown id is a 404, but the shape is validated first so a
/// hostile id can never reach a query or an event payload.
fn validate_module_id(value: &str) -> UnifiedResult<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(UnifiedError::bad_request("module_id must not be empty"));
    }
    if value.len() > MAX_MODULE_ID_LEN
        || !value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(UnifiedError::bad_request(
            "module_id must be 1-64 lowercase letters, digits, or underscores",
        ));
    }
    Ok(value)
}

fn validate_company_id(value: &str) -> UnifiedResult<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(UnifiedError::bad_request("company_id must not be empty"));
    }
    if value.len() > MAX_ID_LEN {
        return Err(UnifiedError::bad_request(
            "company_id must be at most 128 characters",
        ));
    }
    Ok(value)
}

fn validate_source_kind(value: &str) -> UnifiedResult<String> {
    let value = value.trim().to_string();
    if SOURCE_KINDS.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(UnifiedError::bad_request(format!(
            "source_kind must be one of: {}",
            SOURCE_KINDS.join(", ")
        )))
    }
}

fn validate_source_id(value: &str) -> UnifiedResult<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(UnifiedError::bad_request("source_id must not be empty"));
    }
    if value.len() > MAX_SOURCE_ID_LEN {
        return Err(UnifiedError::bad_request(
            "source_id must be at most 256 characters",
        ));
    }
    Ok(value)
}

fn validate_reason(value: &str, required: bool) -> UnifiedResult<Option<String>> {
    let value = value.trim();
    if value.is_empty() {
        if required {
            return Err(UnifiedError::bad_request("reason is required"));
        }
        return Ok(None);
    }
    if value.chars().count() > MAX_REASON_LEN {
        return Err(UnifiedError::bad_request(
            "reason must be at most 1000 characters",
        ));
    }
    Ok(Some(value.to_string()))
}

fn validate_module_id_list(values: &[String]) -> UnifiedResult<Vec<String>> {
    if values.len() > MAX_MODULES_PER_CHANGE {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_MODULES_PER_CHANGE} module ids are allowed in one change"
        )));
    }
    let mut cleaned: Vec<String> = Vec::with_capacity(values.len());
    let mut seen: HashSet<String> = HashSet::new();
    for value in values {
        let module_id = validate_module_id(value)?;
        if seen.insert(module_id.clone()) {
            cleaned.push(module_id);
        }
    }
    Ok(cleaned)
}

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

/// One entitlement row: one module granted to one tenant from one
/// source. The effective entitlement for a module is the union of
/// its active rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entitlement {
    pub id: Uuid,
    /// Tenant namespace id (uuidv7) the row belongs to. Rows are
    /// keyed by `company_id` for lookups, but the tenant id is
    /// stored on every row so a tenant-schema move never has to
    /// guess which tenant a row belongs to.
    pub tenant_id: String,
    pub company_id: String,
    pub source_kind: String,
    pub source_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    pub module_id: String,
    pub status: String,
    pub granted_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_ends_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lapsed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lapse_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One source contributing to an effective entitlement, as shown to
/// callers. A license-derived source has no stored entitlement id:
/// the licensing verification API exposes no license id, so the
/// source is identified by kind only and `source_id` is null.
#[derive(Debug, Clone, Serialize)]
pub struct EntitlementSourceView {
    pub source_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entitlement_id: Option<Uuid>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

/// One entry of the append-only entitlement history. History is
/// never rewritten: a correction is a new entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitlementHistoryEntry {
    pub id: Uuid,
    pub tenant_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entitlement_id: Option<Uuid>,
    pub company_id: String,
    pub module_id: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    pub detail: Value,
    pub created_at: DateTime<Utc>,
}

/// One denied enable attempt, recorded for audit and for the AI
/// suggestion (which modules the tenant tried to use most).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeniedAttempt {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub module_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    pub reason: String,
    pub error_code: String,
    pub created_at: DateTime<Utc>,
}

/// One package/product-set change: the computed module diff plus
/// the decision. `requires_confirmation` modules are never
/// disabled until the confirm call names them explicitly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageChange {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub new_module_ids: Vec<String>,
    pub newly_entitled: Vec<String>,
    pub kept: Vec<String>,
    pub kept_via_other_source: Vec<String>,
    pub requires_confirmation: Vec<String>,
    #[serde(default)]
    pub disabled_module_ids: Vec<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Whether this module enabled a module for a tenant, so the
/// dashboard can tell an enabled module from an entitled-but-
/// never-activated one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleActivation {
    pub tenant_id: String,
    pub company_id: String,
    pub module_id: String,
    pub enabled: bool,
    pub enabled_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// Admin-editable entitlement settings. The values below are
/// CONFIGURABLE DEFAULTS: `Default` is what a fresh install uses,
/// and `PUT /settings` replaces them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitlementSettings {
    /// Read-only grace period in days after an entitlement lapses.
    /// 0 means no grace: a lapsed module is disabled at once.
    pub grace_period_days: i64,
    /// When false, a lapsed module skips grace and is disabled at
    /// once even when `grace_period_days` is positive.
    pub read_only_grace_enabled: bool,
    /// Recorded default-deny policy. Fail-closed is an invariant
    /// and is enforced regardless of this value: no entitlement
    /// record always means not entitled. This setting only governs
    /// the reported policy for unregistered-module lookups.
    pub default_deny: bool,
}

impl Default for EntitlementSettings {
    fn default() -> Self {
        Self {
            grace_period_days: DEFAULT_GRACE_PERIOD_DAYS,
            read_only_grace_enabled: true,
            default_deny: true,
        }
    }
}

impl EntitlementSettings {
    fn validate(&self) -> UnifiedResult<()> {
        if self.grace_period_days < 0 || self.grace_period_days > MAX_GRACE_PERIOD_DAYS {
            return Err(UnifiedError::bad_request(
                "grace_period_days must be between 0 and 365",
            ));
        }
        Ok(())
    }
}

/// Effective access state of one module for one tenant, computed
/// from the union of active sources plus the licensing check.
#[derive(Debug, Clone, Serialize)]
pub struct EffectiveEntitlement {
    pub module_id: String,
    pub company_id: String,
    /// `entitled`, `grace_read_only`, or `locked`.
    pub state: String,
    pub entitled: bool,
    /// True while the module is in read-only grace: reads and data
    /// export are allowed, writes are rejected.
    pub read_only: bool,
    pub write_allowed: bool,
    pub sources: Vec<EntitlementSourceView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_ends_at: Option<DateTime<Utc>>,
    pub enabled: bool,
}

/// One module as listed by `GET /my-modules`, with its
/// per-tenant state and, when locked, its upgrade path.
#[derive(Debug, Clone, Serialize)]
pub struct MyModuleView {
    pub module_id: String,
    pub name: String,
    pub description: String,
    pub available_as_product: bool,
    pub state: String,
    pub entitled: bool,
    pub read_only: bool,
    pub write_allowed: bool,
    pub enabled: bool,
    pub sources: Vec<EntitlementSourceView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_ends_at: Option<DateTime<Utc>>,
    pub upgrade_path: UpgradePath,
}

/// The upgrade path for a locked module: the sellable products or
/// packages that unlock it, resolved from product/licensing data
/// through the hook bus — never a hard-coded map. When no provider
/// answers, the path is null with a clear reason, never a
/// fabricated product.
#[derive(Debug, Clone, Serialize)]
pub struct UpgradePath {
    pub available: bool,
    pub products: Vec<UpgradeProduct>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One product or package that unlocks a module, as reported by a
/// hook handler. `kind` is `package`, `product`, or `license`, and
/// is data from the answering module, not a list kept here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpgradeProduct {
    pub kind: String,
    pub id: String,
    pub name: String,
}

/// One AI package suggestion for a tenant, grounded in the denied
/// attempt log. A suggestion only: it never purchases anything.
#[derive(Debug, Clone, Serialize)]
pub struct PackageSuggestion {
    pub module_id: String,
    pub denied_attempts: u64,
    pub upgrade_path: UpgradePath,
    pub note: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ViewerQuery {
    #[serde(default)]
    pub viewer_role: Option<String>,
    #[serde(default)]
    pub viewer_id: Option<String>,
    #[serde(default)]
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateGrantRequest {
    pub company_id: String,
    #[serde(default)]
    pub tenant_id: Option<String>,
    pub module_id: String,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub granted_by: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateGrantRequest {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EnableModuleRequest {
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApplyPackageChangeRequest {
    pub company_id: String,
    #[serde(default)]
    pub source_kind: Option<String>,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub source_ref: Option<String>,
    pub new_module_ids: Vec<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub requested_by: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfirmPackageChangeRequest {
    #[serde(default)]
    pub confirm: Option<bool>,
    #[serde(default)]
    pub module_ids: Option<Vec<String>>,
    #[serde(default)]
    pub decided_by: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AiSuggestRequest {
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SweepSummary {
    pub scanned: usize,
    pub entered_grace: usize,
    pub disabled: usize,
    pub already_settled: usize,
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

pub struct EntitlementsState {
    pub entitlements: DashMap<Uuid, Entitlement>,
    pub history: DashMap<Uuid, EntitlementHistoryEntry>,
    pub denied_attempts: DashMap<Uuid, DeniedAttempt>,
    pub package_changes: DashMap<Uuid, PackageChange>,
    pub activations: DashMap<String, ModuleActivation>,
    pub settings: RwLock<EntitlementSettings>,
    pub hooks: RwLock<Option<Arc<HookBus>>>,
}

impl Default for EntitlementsState {
    fn default() -> Self {
        Self {
            entitlements: DashMap::new(),
            history: DashMap::new(),
            denied_attempts: DashMap::new(),
            package_changes: DashMap::new(),
            activations: DashMap::new(),
            settings: RwLock::new(EntitlementSettings::default()),
            hooks: RwLock::new(None),
        }
    }
}

impl EntitlementsState {
    pub fn new() -> Self {
        Self::default()
    }

    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn emit_with_results(&self, event: &str, data: Value) -> Option<HookContext> {
        self.bus().map(|bus| bus.emit(event, data))
    }

    pub fn settings(&self) -> EntitlementSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    fn activation_key(company_id: &str, module_id: &str) -> String {
        format!("{company_id}|{module_id}")
    }

    fn is_enabled(&self, company_id: &str, module_id: &str) -> bool {
        self.activations
            .get(&Self::activation_key(company_id, module_id))
            .map(|entry| entry.enabled)
            .unwrap_or(false)
    }

    fn record_history(
        &self,
        tenant_id: &str,
        entitlement_id: Option<Uuid>,
        company_id: &str,
        module_id: &str,
        action: &str,
        actor_id: Option<&str>,
        detail: Value,
    ) {
        self.history.insert(
            Uuid::now_v7(),
            EntitlementHistoryEntry {
                id: Uuid::now_v7(),
                tenant_id: tenant_id.to_string(),
                entitlement_id,
                company_id: company_id.to_string(),
                module_id: module_id.to_string(),
                action: action.to_string(),
                actor_id: actor_id.map(str::to_string),
                detail,
                created_at: Utc::now(),
            },
        );
    }

    fn record_denied(
        &self,
        tenant_id: &str,
        company_id: &str,
        module_id: &str,
        actor_id: Option<&str>,
        reason: &str,
        error_code: &str,
    ) -> DeniedAttempt {
        let attempt = DeniedAttempt {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.to_string(),
            company_id: company_id.to_string(),
            module_id: module_id.to_string(),
            actor_id: actor_id.map(str::to_string),
            reason: reason.to_string(),
            error_code: error_code.to_string(),
            created_at: Utc::now(),
        };
        self.denied_attempts
            .insert(attempt.id, attempt.clone());
        attempt
    }

    /// Active rows for one tenant and module: status `active` and
    /// not past `expires_at`. Expired rows are not returned here;
    /// the grace computation in `effective` handles them.
    fn active_rows(&self, company_id: &str, module_id: &str) -> Vec<Entitlement> {
        let now = Utc::now();
        let mut rows: Vec<Entitlement> = self
            .entitlements
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                row.company_id == company_id
                    && row.module_id == module_id
                    && row.status == STATUS_ACTIVE
                    && row.expires_at.is_none_or(|expires| expires > now)
            })
            .collect();
        rows.sort_by(|a, b| a.granted_at.cmp(&b.granted_at));
        rows
    }

    /// Grace rows for one tenant and module: status `grace` with a
    /// grace window that has not ended yet.
    fn grace_rows(&self, company_id: &str, module_id: &str) -> Vec<Entitlement> {
        let now = Utc::now();
        let mut rows: Vec<Entitlement> = self
            .entitlements
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                row.company_id == company_id
                    && row.module_id == module_id
                    && row.status == STATUS_GRACE
                    && row.grace_ends_at.is_none_or(|ends| ends > now)
            })
            .collect();
        rows.sort_by(|a, b| a.granted_at.cmp(&b.granted_at));
        rows
    }

    /// Rows whose expiry has passed but whose grace window (expiry
    /// + configured grace period) has not: these read as grace even
    /// before the sweep materializes the transition.
    fn lapsed_in_grace_window(&self, company_id: &str, module_id: &str) -> Vec<Entitlement> {
        let settings = self.settings();
        if !settings.read_only_grace_enabled || settings.grace_period_days <= 0 {
            return Vec::new();
        }
        let now = Utc::now();
        let mut rows: Vec<Entitlement> = self
            .entitlements
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                row.company_id == company_id
                    && row.module_id == module_id
                    && row.status == STATUS_ACTIVE
                    && match row.expires_at {
                        Some(expires) => {
                            expires <= now
                                && expires + Duration::days(settings.grace_period_days) > now
                        }
                        None => false,
                    }
            })
            .collect();
        rows.sort_by(|a, b| a.granted_at.cmp(&b.granted_at));
        rows
    }

    /// The effective entitlement for one module and tenant: the
    /// union of active stored sources plus the licensing check.
    /// `ctx` is `None` inside hook handlers (no AppContext there),
    /// in which case only stored rows are consulted — the licensing
    /// leg is skipped, never assumed. Fail closed throughout: no
    /// active source means locked.
    pub fn effective(
        &self,
        ctx: Option<&AppContext>,
        company_id: &str,
        module_id: &str,
    ) -> EffectiveEntitlement {
        let settings = self.settings();
        let mut sources: Vec<EntitlementSourceView> = Vec::new();
        let mut expires_at: Option<DateTime<Utc>> = None;
        for row in self.active_rows(company_id, module_id) {
            if let Some(expires) = row.expires_at {
                expires_at = Some(match expires_at {
                    Some(current) if current < expires => current,
                    Some(current) => current,
                    None => expires,
                });
            }
            sources.push(EntitlementSourceView {
                source_kind: row.source_kind.clone(),
                source_id: Some(row.source_id.clone()),
                entitlement_id: Some(row.id),
                status: STATUS_ACTIVE.to_string(),
                expires_at: row.expires_at,
            });
        }
        // Licensing leg, mirroring `check_module_entitlement`
        // exactly (composed, never re-implemented): a sellable CORE
        // module is covered by the implicit owner entitlement, and
        // a sellable non-core module by an active license whose
        // product names it. The licensing API exposes no license
        // id, so a derived source is identified by kind only.
        if let Some(ctx) = ctx {
            if let Some(module) = ctx.modules.get(module_id) {
                if module.available_as_product() && module.is_core() {
                    sources.push(EntitlementSourceView {
                        source_kind: SOURCE_OWNER_ENTITLEMENT.to_string(),
                        source_id: None,
                        entitlement_id: None,
                        status: STATUS_ACTIVE.to_string(),
                        expires_at: None,
                    });
                } else if has_module_entitlement(ctx, company_id, module_id)
                    && !sources.iter().any(|source| {
                        source.source_kind == SOURCE_LICENSE
                            && source.entitlement_id.is_none()
                    })
                {
                    sources.push(EntitlementSourceView {
                        source_kind: SOURCE_LICENSE.to_string(),
                        source_id: None,
                        entitlement_id: None,
                        status: STATUS_ACTIVE.to_string(),
                        expires_at: None,
                    });
                }
            }
        }
        if !sources.is_empty() {
            return EffectiveEntitlement {
                module_id: module_id.to_string(),
                company_id: company_id.to_string(),
                state: ACCESS_ENTITLED.to_string(),
                entitled: true,
                read_only: false,
                write_allowed: true,
                sources,
                expires_at,
                grace_ends_at: None,
                enabled: self.is_enabled(company_id, module_id),
            };
        }
        // No active source: grace if a grace row is live, or if an
        // expired row is still inside its grace window.
        let mut grace_ends_at: Option<DateTime<Utc>> = None;
        let mut grace_sources: Vec<EntitlementSourceView> = Vec::new();
        for row in self.grace_rows(company_id, module_id) {
            grace_ends_at = match (grace_ends_at, row.grace_ends_at) {
                (Some(current), Some(ends)) if ends > current => Some(ends),
                (None, ends) => ends,
                (current, _) => current,
            };
            grace_sources.push(EntitlementSourceView {
                source_kind: row.source_kind.clone(),
                source_id: Some(row.source_id.clone()),
                entitlement_id: Some(row.id),
                status: STATUS_GRACE.to_string(),
                expires_at: row.expires_at,
            });
        }
        for row in self.lapsed_in_grace_window(company_id, module_id) {
            let ends = row
                .expires_at
                .map(|expires| expires + Duration::days(settings.grace_period_days));
            grace_ends_at = match (grace_ends_at, ends) {
                (Some(current), Some(ends)) if ends > current => Some(ends),
                (None, ends) => ends,
                (current, _) => current,
            };
            grace_sources.push(EntitlementSourceView {
                source_kind: row.source_kind.clone(),
                source_id: Some(row.source_id.clone()),
                entitlement_id: Some(row.id),
                status: STATUS_GRACE.to_string(),
                expires_at: row.expires_at,
            });
        }
        if !grace_sources.is_empty() {
            return EffectiveEntitlement {
                module_id: module_id.to_string(),
                company_id: company_id.to_string(),
                state: ACCESS_GRACE_READ_ONLY.to_string(),
                entitled: false,
                read_only: true,
                write_allowed: false,
                sources: grace_sources,
                expires_at,
                grace_ends_at,
                enabled: self.is_enabled(company_id, module_id),
            };
        }
        EffectiveEntitlement {
            module_id: module_id.to_string(),
            company_id: company_id.to_string(),
            state: ACCESS_LOCKED.to_string(),
            entitled: false,
            read_only: false,
            write_allowed: false,
            sources: Vec::new(),
            expires_at: None,
            grace_ends_at: None,
            enabled: self.is_enabled(company_id, module_id),
        }
    }

    /// Modules the tenant is effectively entitled to right now
    /// (stored active rows plus the licensing leg, when a context
    /// is available), as a sorted, de-duplicated list.
    fn entitled_module_ids(
        &self,
        ctx: Option<&AppContext>,
        company_id: &str,
    ) -> Vec<String> {
        let now = Utc::now();
        let mut ids: HashSet<String> = HashSet::new();
        for entry in self.entitlements.iter() {
            let row = entry.value();
            if row.company_id == company_id
                && row.status == STATUS_ACTIVE
                && row.expires_at.is_none_or(|expires| expires > now)
            {
                ids.insert(row.module_id.clone());
            }
        }
        if let Some(ctx) = ctx {
            for module in ctx.modules.list() {
                let module_id = module.id();
                if module.available_as_product()
                    && has_module_entitlement(ctx, company_id, module_id)
                {
                    ids.insert(module_id.to_string());
                }
            }
        }
        let mut sorted: Vec<String> = ids.into_iter().collect();
        sorted.sort();
        sorted
    }

    /// Ask the hook bus which sellable products or packages unlock
    /// a module. Never fabricates: when no handler answers, the
    /// path is null with reason `no_upgrade_path_provider`; when
    /// handlers answer with an empty list, the reason is
    /// `no_product_unlocks_module`.
    fn upgrade_path(&self, company_id: &str, module_id: &str) -> UpgradePath {
        let outcome = self.emit_with_results(
            EVENT_UPGRADE_PATH_REQUESTED,
            json!({
                "company_id": company_id,
                "module_id": module_id,
                "upgrade_paths": [],
            }),
        );
        if let Some(outcome) = outcome {
            if let Some(paths) = outcome.get("upgrade_paths") {
                if let Some(items) = paths.as_array() {
                    let mut products: Vec<UpgradeProduct> = Vec::new();
                    for item in items {
                        let kind = item
                            .get("kind")
                            .and_then(|value| value.as_str())
                            .unwrap_or("package")
                            .to_string();
                        let id = item
                            .get("id")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_string();
                        if !id.is_empty() && !name.is_empty() {
                            products.push(UpgradeProduct { kind, id, name });
                        }
                    }
                    if !products.is_empty() {
                        return UpgradePath {
                            available: true,
                            products,
                            reason: None,
                        };
                    }
                    // A handler answered explicitly with no products.
                    return UpgradePath {
                        available: false,
                        products: Vec::new(),
                        reason: Some("no_product_unlocks_module".to_string()),
                    };
                }
            }
            for result in &outcome.results {
                if let Some(items) = result.get("upgrade_paths").and_then(|value| value.as_array())
                {
                    let mut products: Vec<UpgradeProduct> = Vec::new();
                    for item in items {
                        if let Ok(product) =
                            serde_json::from_value::<UpgradeProduct>(item.clone())
                        {
                            products.push(product);
                        }
                    }
                    if !products.is_empty() {
                        return UpgradePath {
                            available: true,
                            products,
                            reason: None,
                        };
                    }
                }
            }
        }
        UpgradePath {
            available: false,
            products: Vec::new(),
            reason: Some("no_upgrade_path_provider".to_string()),
        }
    }

    /// Ask the hook bus whether the tenant uses a module outside
    /// the package being replaced. Fail-safe: when nothing answers,
    /// the module is treated as used, so the diff lists it under
    /// `requires_confirmation` instead of silently disabling it.
    fn used_outside_package(&self, company_id: &str, module_id: &str) -> bool {
        let outcome = self.emit_with_results(
            EVENT_MODULE_USAGE_REQUESTED,
            json!({
                "company_id": company_id,
                "module_id": module_id,
                "used_outside_package": false,
            }),
        );
        match outcome {
            Some(outcome) => outcome
                .get("used_outside_package")
                .and_then(|value| value.as_bool())
                .unwrap_or(true),
            None => true,
        }
    }

    /// Create one entitlement row (stored sources only; the
    /// licensing leg is read-only and never written here).
    fn create_entitlement(
        &self,
        tenant_id: &str,
        company_id: &str,
        source_kind: &str,
        source_id: &str,
        source_ref: Option<String>,
        module_id: &str,
        expires_at: Option<DateTime<Utc>>,
        granted_by: Option<&str>,
        reason: Option<String>,
    ) -> Entitlement {
        let now = Utc::now();
        let entitlement = Entitlement {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.to_string(),
            company_id: company_id.to_string(),
            source_kind: source_kind.to_string(),
            source_id: source_id.to_string(),
            source_ref,
            module_id: module_id.to_string(),
            status: STATUS_ACTIVE.to_string(),
            granted_at: now,
            expires_at,
            granted_by: granted_by.map(str::to_string),
            reason,
            grace_started_at: None,
            grace_ends_at: None,
            lapsed_at: None,
            lapse_reason: None,
            created_at: now,
            updated_at: now,
        };
        self.entitlements
            .insert(entitlement.id, entitlement.clone());
        self.record_history(
            tenant_id,
            Some(entitlement.id),
            company_id,
            module_id,
            "granted",
            granted_by,
            json!({
                "source_kind": source_kind,
                "source_id": source_id,
                "expires_at": expires_at,
            }),
        );
        self.emit(
            EVENT_ENTITLEMENT_GRANTED,
            json!({
                "company_id": company_id,
                "module_id": module_id,
                "source_kind": source_kind,
                "source_id": source_id,
                "entitlement_id": entitlement.id,
                "expires_at": expires_at,
            }),
        );
        entitlement
    }

    /// Move one stored row into read-only grace (when the settings
    /// allow grace) or straight to disabled. Emits the matching
    /// event and history entry. Never deletes anything.
    fn lapse_row(&self, row: &mut Entitlement, reason: &str) {
        let settings = self.settings();
        let now = Utc::now();
        row.lapsed_at = Some(now);
        row.lapse_reason = Some(reason.to_string());
        row.updated_at = now;
        if settings.read_only_grace_enabled && settings.grace_period_days > 0 {
            row.status = STATUS_GRACE.to_string();
            row.grace_started_at = Some(now);
            row.grace_ends_at = Some(now + Duration::days(settings.grace_period_days));
            let grace_ends_at = row.grace_ends_at;
            let tenant_id = row.tenant_id.clone();
            let company_id = row.company_id.clone();
            let module_id = row.module_id.clone();
            let entitlement_id = row.id;
            self.record_history(
                &tenant_id,
                Some(entitlement_id),
                &company_id,
                &module_id,
                "grace_started",
                None,
                json!({ "reason": reason, "grace_ends_at": grace_ends_at }),
            );
            self.emit(
                EVENT_ENTITLEMENT_GRACE_STARTED,
                json!({
                    "company_id": company_id,
                    "module_id": module_id,
                    "entitlement_id": entitlement_id,
                    "grace_ends_at": grace_ends_at,
                    "reason": reason,
                }),
            );
        } else {
            row.status = STATUS_DISABLED.to_string();
            row.grace_started_at = Some(now);
            row.grace_ends_at = Some(now);
            let tenant_id = row.tenant_id.clone();
            let company_id = row.company_id.clone();
            let module_id = row.module_id.clone();
            let entitlement_id = row.id;
            self.record_history(
                &tenant_id,
                Some(entitlement_id),
                &company_id,
                &module_id,
                "disabled",
                None,
                json!({ "reason": reason }),
            );
            self.emit(
                EVENT_ENTITLEMENT_DISABLED,
                json!({
                    "company_id": company_id,
                    "module_id": module_id,
                    "entitlement_id": entitlement_id,
                    "reason": reason,
                }),
            );
            self.emit(
                events::MODULE_DISABLED,
                json!({ "id": module_id, "company_id": company_id, "reason": reason }),
            );
        }
    }

    /// Re-evaluate modules for a tenant after a source lapsed.
    /// The lapsed rows themselves are always settled (marked
    /// revoked with a history entry), but a module that another
    /// active source still covers never enters grace: only a
    /// module with no remaining active source lapses into grace
    /// (or disable). The licensing leg cannot be evaluated from a
    /// hook handler (no AppContext), so a lapse that only affects
    /// a license-backed module is re-checked by the sweep and by
    /// the next effective read.
    fn apply_source_lapse(
        &self,
        source_kind: &str,
        source_id: &str,
        company_id: Option<&str>,
        reason: &str,
    ) {
        let now = Utc::now();
        let mut candidates: Vec<Uuid> = Vec::new();
        for entry in self.entitlements.iter() {
            let row = entry.value();
            if row.source_kind != source_kind {
                continue;
            }
            if row.source_id != source_id && row.source_ref.as_deref() != Some(source_id) {
                continue;
            }
            if let Some(company_id) = company_id {
                if row.company_id != company_id {
                    continue;
                }
            }
            if row.status != STATUS_ACTIVE {
                continue;
            }
            candidates.push(row.id);
        }
        for id in candidates {
            // Compute coverage BEFORE taking the write guard: the
            // DashMap shard lock is not re-entrant, so no map
            // iteration may run while a row guard is held.
            let (tenant_id, company_id, module_id, row_source_kind, row_source_id) =
                match self.entitlements.get(&id) {
                    Some(row) => (
                        row.tenant_id.clone(),
                        row.company_id.clone(),
                        row.module_id.clone(),
                        row.source_kind.clone(),
                        row.source_id.clone(),
                    ),
                    None => continue,
                };
            // Another stored source still covers this module: settle
            // this row as revoked only, with no grace transition.
            let other_active = self.entitlements.iter().any(|other| {
                let other = other.value();
                other.id != id
                    && other.company_id == company_id
                    && other.module_id == module_id
                    && other.status == STATUS_ACTIVE
                    && other.expires_at.is_none_or(|expires| expires > now)
                    && !(other.source_kind == row_source_kind
                        && other.source_id == row_source_id)
            });
            let Some(mut entry) = self.entitlements.get_mut(&id) else {
                continue;
            };
            if other_active {
                let row = entry.value_mut();
                row.status = STATUS_REVOKED.to_string();
                row.lapsed_at = Some(now);
                row.lapse_reason = Some(reason.to_string());
                row.updated_at = now;
                self.record_history(
                    &tenant_id,
                    Some(id),
                    &company_id,
                    &module_id,
                    "revoked",
                    None,
                    json!({ "reason": reason, "covered_by_other_source": true }),
                );
                self.emit(
                    EVENT_ENTITLEMENT_REVOKED,
                    json!({
                        "company_id": company_id,
                        "module_id": module_id,
                        "source_kind": row_source_kind,
                        "source_id": row_source_id,
                        "entitlement_id": id,
                        "reason": reason,
                    }),
                );
            } else {
                let row = entry.value_mut();
                self.lapse_row(row, reason);
            }
        }
    }

    /// Revoke every active stored row for one tenant and module
    /// except explicit manual grants. Called when the tenants
    /// module revokes its grant for the module: the grant is gone,
    /// so the module lapses into grace (or disable) — but a manual
    /// grant made here is a separate entitlement the tenants
    /// revoke did not address, so it survives and only an admin
    /// revoke here removes it. Tenant data is never touched.
    fn revoke_module_rows(&self, company_id: &str, module_id: &str, reason: &str) {
        let ids: Vec<Uuid> = self
            .entitlements
            .iter()
            .filter(|entry| {
                let row = entry.value();
                row.company_id == company_id
                    && row.module_id == module_id
                    && row.status == STATUS_ACTIVE
                    && row.source_kind != SOURCE_MANUAL_GRANT
            })
            .map(|entry| *entry.key())
            .collect();
        for id in ids {
            if let Some(mut entry) = self.entitlements.get_mut(&id) {
                self.lapse_row(entry.value_mut(), reason);
            }
        }
    }

    /// Run the lapse/grace sweep: expired active rows enter grace
    /// (or disable), and grace rows past their window are disabled.
    /// Tenant data is never touched: this only changes entitlement
    /// rows and emits events.
    fn sweep(&self) -> SweepSummary {
        let now = Utc::now();
        let mut scanned = 0usize;
        let mut entered_grace = 0usize;
        let mut disabled = 0usize;
        let mut already_settled = 0usize;
        let ids: Vec<Uuid> = self
            .entitlements
            .iter()
            .map(|entry| *entry.key())
            .collect();
        for id in ids {
            let Some(mut entry) = self.entitlements.get_mut(&id) else {
                continue;
            };
            scanned += 1;
            let row = entry.value_mut();
            match row.status.as_str() {
                STATUS_ACTIVE => {
                    let expired = row.expires_at.is_some_and(|expires| expires <= now);
                    if expired {
                        let had_grace = self.settings().read_only_grace_enabled
                            && self.settings().grace_period_days > 0;
                        self.lapse_row(row, "expired");
                        if had_grace {
                            entered_grace += 1;
                        } else {
                            disabled += 1;
                        }
                    } else {
                        already_settled += 1;
                    }
                }
                STATUS_GRACE => {
                    let past_grace = row.grace_ends_at.is_some_and(|ends| ends <= now);
                    if past_grace {
                        let tenant_id = row.tenant_id.clone();
                        let company_id = row.company_id.clone();
                        let module_id = row.module_id.clone();
                        let entitlement_id = row.id;
                        row.status = STATUS_DISABLED.to_string();
                        row.updated_at = now;
                        self.record_history(
                            &tenant_id,
                            Some(entitlement_id),
                            &company_id,
                            &module_id,
                            "disabled",
                            None,
                            json!({ "reason": "grace_period_ended" }),
                        );
                        self.emit(
                            EVENT_ENTITLEMENT_DISABLED,
                            json!({
                                "company_id": company_id,
                                "module_id": module_id,
                                "entitlement_id": entitlement_id,
                                "reason": "grace_period_ended",
                            }),
                        );
                        self.emit(
                            events::MODULE_DISABLED,
                            json!({ "id": module_id, "company_id": company_id, "reason": "grace_period_ended" }),
                        );
                        disabled += 1;
                    } else {
                        already_settled += 1;
                    }
                }
                _ => {
                    already_settled += 1;
                }
            }
        }
        SweepSummary {
            scanned,
            entered_grace,
            disabled,
            already_settled,
        }
    }

    /// Top denied modules for a tenant in the lookback window, most
    /// denied first. Only counts are returned — never buyer
    /// personal data.
    fn top_denied_modules(&self, company_id: &str, limit: usize) -> Vec<(String, u64)> {
        let since = Utc::now() - Duration::days(DENIED_LOOKBACK_DAYS);
        let mut counts: Vec<(String, u64)> = Vec::new();
        for entry in self.denied_attempts.iter() {
            let attempt = entry.value();
            if attempt.company_id != company_id || attempt.created_at < since {
                continue;
            }
            match counts.iter_mut().find(|(module_id, _)| *module_id == attempt.module_id) {
                Some((_, count)) => *count += 1,
                None => counts.push((attempt.module_id.clone(), 1)),
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        counts.truncate(limit.max(1).min(MAX_SUGGESTIONS));
        counts
    }
}

// ---------------------------------------------------------------------------
// Capability checks and caller identity.
// ---------------------------------------------------------------------------

/// Fail when no role was supplied or the roles module does not grant
/// `permission` for it (the licensing module's
/// `require_licensing_capability` pattern: ask `role.check` on the
/// hook bus, fail closed on silence).
fn require_capability(
    ctx: &AppContext,
    role: Option<&str>,
    permission: &str,
) -> UnifiedResult<()> {
    let denied = || {
        UnifiedError::forbidden(format!(
            "Role lacks required capability: {permission}"
        ))
    };
    let Some(role) = role.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(denied());
    };
    let outcome = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    if outcome
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(denied())
    }
}

/// Caller identity for the capability check only: `X-User-Id`
/// (actor) and `X-Role` (role slug), with `?viewer_id=` /
/// `?viewer_role=` query fallbacks. Headers win when both are
/// present. Tenant identity is deliberately NOT part of this
/// struct: it comes only from `TenantScope::resolve` (the
/// middleware-attached tenant context), never from a
/// caller-supplied `X-Company-Id` header. See the module header
/// for the known gap: role headers arrive as trusted input until
/// the auth extractor lands.
#[derive(Debug, Clone, Default)]
struct CallerInfo {
    actor_id: Option<String>,
    role: Option<String>,
}

fn caller_of(req: &HttpRequest, query: &ViewerQuery) -> CallerInfo {
    let header = |name: &str| {
        req.headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    CallerInfo {
        actor_id: header("x-user-id").or_else(|| {
            query
                .viewer_id
                .clone()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        }),
        role: header("x-role").or_else(|| {
            query
                .viewer_role
                .clone()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        }),
    }
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

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Tenant scope (standing rule: tenant-scoped data access).
// ---------------------------------------------------------------------------

/// The tenant a request resolved to, from the canonical tenant
/// context (`crate::core::tenancy::TenantContext`, built by the
/// `TenantResolutionMiddleware` from the Host header through the
/// `tenant.resolve_requested` hook). This is the ONLY trusted
/// tenant identity for tenant-scoped endpoints: `tenant_id` is
/// the uuidv7 namespace, `schema_name` is derived from it ONLY by
/// `tenancy::schema_name()` (never built inline here),
/// `company_id` is the tenant's company key the entitlement rows
/// are keyed by (the tenants module uses the tenant id itself as
/// the company id), and `site_id` names the tenant site. No
/// tenant id, schema name, or package id is ever hard-coded here.
#[derive(Debug, Clone)]
pub struct TenantScope {
    pub tenant_id: String,
    pub schema_name: String,
    pub company_id: String,
    pub site_id: String,
}

impl TenantScope {
    /// Resolve the tenant scope for a tenant-scoped call through
    /// the canonical tenancy extractor. Fail closed: when no
    /// tenant context is present (a control-plane request, an
    /// unresolved host, or middleware not running) the call is
    /// refused with 403 `tenant.required` and no data is returned
    /// — never a silent fall back to the `public` schema and
    /// never another tenant's modules. A `requested` company that
    /// differs from the context's company is also a 403: a tenant
    /// may only ever see its own modules. A resolved context with
    /// no company id is also refused: entitlement rows are keyed
    /// by company, and there is no company to key them by.
    pub fn resolve(req: &HttpRequest, requested: Option<String>) -> UnifiedResult<Self> {
        let denied = || {
            UnifiedError::forbidden(
                "Tenant context is required: this endpoint serves tenant data only",
            )
            .with_metadata("error_code", json!("tenant.required"))
        };
        let context = TenantContext::from_http_request(req).map_err(|_| denied())?;
        let Some(company_id) = context.company_id.clone().filter(|value| !value.trim().is_empty()) else {
            return Err(denied());
        };
        if let Some(requested) = requested {
            let requested = requested.trim().to_string();
            if !requested.is_empty() && requested != company_id {
                return Err(UnifiedError::forbidden(
                    "A tenant may only act on its own company_id",
                )
                .with_metadata("error_code", json!("tenant.required")));
            }
        }
        Ok(Self {
            tenant_id: context.tenant_id.to_string(),
            schema_name: context.schema_name,
            company_id,
            site_id: context.site_id.unwrap_or_default(),
        })
    }
}

// ---------------------------------------------------------------------------
// Handlers: tenant dashboard.
// ---------------------------------------------------------------------------

async fn list_my_modules(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_of(&req, &ViewerQuery {
            viewer_role: params.filters.get("viewer_role").cloned(),
            viewer_id: params.filters.get("viewer_id").cloned(),
            company_id: params.filters.get("company_id").cloned(),
        });
        require_capability(&ctx, caller.role.as_deref(), PERM_ENTITLEMENTS_READ)?;
        let scope = TenantScope::resolve(
            &req,
            params.filters.get("company_id").cloned(),
        )?;
        let mut items: Vec<MyModuleView> = Vec::new();
        for module in ctx.modules.list() {
            let module_id = module.id();
            let effective = state.effective(Some(&ctx), &scope.company_id, module_id);
            let upgrade_path = if effective.entitled || !module.available_as_product() {
                UpgradePath {
                    available: false,
                    products: Vec::new(),
                    reason: Some("already_entitled".to_string()),
                }
            } else {
                state.upgrade_path(&scope.company_id, module_id)
            };
            items.push(MyModuleView {
                module_id: module_id.to_string(),
                name: module.name().to_string(),
                description: module.description().to_string(),
                available_as_product: module.available_as_product(),
                state: effective.state,
                entitled: effective.entitled,
                read_only: effective.read_only,
                write_allowed: effective.write_allowed,
                enabled: effective.enabled,
                sources: effective.sources,
                expires_at: effective.expires_at,
                grace_ends_at: effective.grace_ends_at,
                upgrade_path,
            });
        }
        items.retain(|item| {
            matches_search(&format!("{} {} {}", item.module_id, item.name, item.description), &params)
                && params
                    .filters
                    .get("state")
                    .is_none_or(|state| item.state == *state)
                && params
                    .filters
                    .get("entitled")
                    .is_none_or(|entitled| item.entitled == (*entitled == "true" || *entitled == "1"))
                && params
                    .filters
                    .get("available_as_product")
                    .is_none_or(|value| {
                        item.available_as_product == (*value == "true" || *value == "1")
                    })
        });
        let sort = params.allowed_sort(&["module_id", "name", "state"], "module_id");
        match sort.as_str() {
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
            "state" => items.sort_by(|a, b| {
                a.state.cmp(&b.state).then(a.module_id.cmp(&b.module_id))
            }),
            _ => items.sort_by(|a, b| a.module_id.cmp(&b.module_id)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(json!({
            "tenant_id": scope.tenant_id,
            "schema_name": scope.schema_name,
            "company_id": scope.company_id,
            "site_id": scope.site_id,
            "modules": paginate(items, &params).to_json(),
        }))
    })();
    json::respond(result, debug)
}

async fn get_module_access(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    path: web::Path<String>,
    query: web::Query<ViewerQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let module_id = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_of(&req, &query);
        require_capability(&ctx, caller.role.as_deref(), PERM_ENTITLEMENTS_READ)?;
        let scope = TenantScope::resolve(&req, query.company_id.clone())?;
        let module_id = validate_module_id(&module_id)?;
        let module = ctx
            .modules
            .get(&module_id)
            .ok_or_else(|| UnifiedError::not_found(format!("Module \"{module_id}\" is not registered")))?;
        let effective = state.effective(Some(&ctx), &scope.company_id, &module_id);
        let upgrade_path = if effective.entitled || !module.available_as_product() {
            UpgradePath {
                available: false,
                products: Vec::new(),
                reason: Some("already_entitled".to_string()),
            }
        } else {
            state.upgrade_path(&scope.company_id, &module_id)
        };
        Ok(json!({
            "tenant_id": scope.tenant_id,
            "schema_name": scope.schema_name,
            "company_id": scope.company_id,
            "site_id": scope.site_id,
            "access": effective,
            "upgrade_path": upgrade_path,
        }))
    })();
    json::respond(result, debug)
}

async fn enable_module(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    path: web::Path<String>,
    body: web::Json<EnableModuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let module_id = path.into_inner();
    let body = body.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_of(&req, &ViewerQuery {
            viewer_role: None,
            viewer_id: body.actor_id.clone(),
            company_id: body.company_id.clone(),
        });
        // The role still arrives on `X-Role` (or `?viewer_role=`);
        // the body carries only the actor and the tenant.
        let role = req
            .headers()
            .get("x-role")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or(caller.role.clone());
        require_capability(&ctx, role.as_deref(), PERM_ENTITLEMENTS_READ)?;
        let scope = TenantScope::resolve(&req, body.company_id.clone())?;
        let module_id = validate_module_id(&module_id)?;
        let module = ctx
            .modules
            .get(&module_id)
            .ok_or_else(|| UnifiedError::not_found(format!("Module \"{module_id}\" is not registered")))?;
        let actor = caller.actor_id.as_deref();
        // Non-sellable modules need no entitlement (the licensing
        // gate's rule, composed here rather than duplicated).
        if !module.available_as_product() {
            set_activation(&state, &scope, &module_id, true, actor, None, None);
            state.emit(
                events::MODULE_ENABLED,
                json!({ "id": module_id, "company_id": scope.company_id, "reason": "not_sellable" }),
            );
            let effective = state.effective(Some(&ctx), &scope.company_id, &module_id);
            return Ok(json!({ "tenant_id": scope.tenant_id, "access": effective }));
        }
        let effective = state.effective(Some(&ctx), &scope.company_id, &module_id);
        match effective.state.as_str() {
            ACCESS_ENTITLED | ACCESS_GRACE_READ_ONLY => {
                set_activation(&state, &scope, &module_id, true, actor, None, None);
                state.emit(
                    events::MODULE_ENABLED,
                    json!({ "id": module_id, "company_id": scope.company_id, "reason": effective.state }),
                );
                let effective = state.effective(Some(&ctx), &scope.company_id, &module_id);
                Ok(json!({ "tenant_id": scope.tenant_id, "access": effective }))
            }
            _ => {
                let reason = format!("Module \"{module_id}\" requires an active entitlement for this company");
                state.record_denied(
                    &scope.tenant_id,
                    &scope.company_id,
                    &module_id,
                    actor,
                    &reason,
                    ERROR_ENTITLEMENT_REQUIRED,
                );
                state.emit(
                    EVENT_ENTITLEMENT_ENABLE_DENIED,
                    json!({
                        "company_id": scope.company_id,
                        "module_id": module_id,
                        "actor_id": actor,
                        "reason": reason,
                        "error_code": ERROR_ENTITLEMENT_REQUIRED,
                    }),
                );
                state.emit(
                    MODULE_ENTITLEMENT_DENIED_EVENT,
                    json!({
                        "id": module_id,
                        "company_id": scope.company_id,
                        "reason": "no_active_entitlement",
                    }),
                );
                Err(UnifiedError::forbidden(reason)
                    .with_metadata("error_code", json!(ERROR_ENTITLEMENT_REQUIRED))
                    .with_metadata("module_id", json!(module_id))
                    .with_metadata("company_id", json!(scope.company_id)))
            }
        }
    })();
    json::respond(result, debug)
}

async fn disable_module(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    path: web::Path<String>,
    body: web::Json<EnableModuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let module_id = path.into_inner();
    let body = body.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_of(&req, &ViewerQuery {
            viewer_role: None,
            viewer_id: body.actor_id.clone(),
            company_id: body.company_id.clone(),
        });
        let role = req
            .headers()
            .get("x-role")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or(caller.role.clone());
        require_capability(&ctx, role.as_deref(), PERM_ENTITLEMENTS_READ)?;
        let scope = TenantScope::resolve(&req, body.company_id.clone())?;
        let module_id = validate_module_id(&module_id)?;
        if ctx.modules.get(&module_id).is_none() {
            return Err(UnifiedError::not_found(format!(
                "Module \"{module_id}\" is not registered"
            )));
        }
        set_activation(&state, &scope, &module_id, false, caller.actor_id.as_deref(), None, None);
        state.record_history(
            &scope.tenant_id,
            None,
            &scope.company_id,
            &module_id,
            "disabled",
            caller.actor_id.as_deref(),
            json!({ "reason": "disabled_by_tenant" }),
        );
        state.emit(
            events::MODULE_DISABLED,
            json!({ "id": module_id, "company_id": scope.company_id, "reason": "disabled_by_tenant" }),
        );
        let effective = state.effective(Some(&ctx), &scope.company_id, &module_id);
        Ok(json!({ "tenant_id": scope.tenant_id, "access": effective }))
    })();
    json::respond(result, debug)
}

/// Record (or update) the per-tenant activation state for a
/// module. Activation state IS tenant business data: it is written
/// only under a resolved tenant scope, keyed by the tenant's
/// company, and never under a control-plane identity.
fn set_activation(
    state: &EntitlementsState,
    scope: &TenantScope,
    module_id: &str,
    enabled: bool,
    enabled_by: Option<&str>,
    source_kind: Option<&str>,
    source_id: Option<&str>,
) {
    let now = Utc::now();
    let key = EntitlementsState::activation_key(&scope.company_id, module_id);
    state.activations.insert(
        key,
        ModuleActivation {
            tenant_id: scope.tenant_id.clone(),
            company_id: scope.company_id.clone(),
            module_id: module_id.to_string(),
            enabled,
            enabled_at: now,
            enabled_by: enabled_by.map(str::to_string),
            source_kind: source_kind.map(str::to_string),
            source_id: source_id.map(str::to_string),
            updated_at: now,
        },
    );
}

// ---------------------------------------------------------------------------
// Handlers: package / product-set changes.
// ---------------------------------------------------------------------------

async fn apply_package_change(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    body: web::Json<ApplyPackageChangeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let body = body.into_inner();
    let result: UnifiedResult<PackageChange> = (|| {
        let role = req
            .headers()
            .get("x-role")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        require_capability(&ctx, role.as_deref(), PERM_ENTITLEMENTS_READ)?;
        let scope = TenantScope::resolve(&req, Some(body.company_id.clone()))?;
        let source_kind = match body.source_kind {
            Some(kind) => validate_source_kind(&kind)?,
            None => SOURCE_SAAS_PACKAGE.to_string(),
        };
        let source_id = match body.source_id {
            Some(id) => validate_source_id(&id)?,
            None => {
                return Err(UnifiedError::bad_request(
                    "source_id is required (package, product, subscription, or license id)",
                ));
            }
        };
        let new_module_ids = validate_module_id_list(&body.new_module_ids)?;
        for module_id in &new_module_ids {
            if ctx.modules.get(module_id).is_none() {
                return Err(UnifiedError::not_found(format!(
                    "Module \"{module_id}\" is not registered"
                )));
            }
        }
        if let Some(expires_at) = body.expires_at {
            if expires_at <= Utc::now() {
                return Err(UnifiedError::bad_request(
                    "expires_at must be in the future",
                ));
            }
        }
        // Only sellable modules take part in the diff: a module
        // that is not sellable needs no entitlement and is kept
        // unconditionally.
        let (sellable_new, free_new): (Vec<String>, Vec<String>) = new_module_ids
            .into_iter()
            .partition(|module_id| {
                ctx.modules
                    .get(module_id)
                    .map(|module| module.available_as_product())
                    .unwrap_or(false)
            });
        let current = state.entitled_module_ids(Some(&ctx), &scope.company_id);
        let current_set: HashSet<String> = current.iter().cloned().collect();
        let new_set: HashSet<String> = sellable_new.iter().cloned().collect();
        let mut newly_entitled: Vec<String> = new_set
            .difference(&current_set)
            .cloned()
            .collect();
        newly_entitled.sort();
        let mut kept: Vec<String> = new_set
            .intersection(&current_set)
            .cloned()
            .collect();
        kept.extend(free_new);
        kept.sort();
        let mut lost: Vec<String> = current_set
            .difference(&new_set)
            .cloned()
            .collect();
        lost.sort();
        let now = Utc::now();
        let mut kept_via_other_source: Vec<String> = Vec::new();
        let mut requires_confirmation: Vec<String> = Vec::new();
        let mut disabled_module_ids: Vec<String> = Vec::new();
        for module_id in lost {
            // Still covered by another active source (a different
            // license, subscription, package, or grant) or by the
            // licensing leg: the module stays entitled and is never
            // listed for confirmation.
            let other_stored = state.entitlements.iter().any(|entry| {
                let row = entry.value();
                row.company_id == scope.company_id
                    && row.module_id == module_id
                    && row.status == STATUS_ACTIVE
                    && row.expires_at.is_none_or(|expires| expires > now)
                    && !(row.source_kind == source_kind && row.source_id == source_id)
            });
            let other_licensed =
                has_module_entitlement(&ctx, &scope.company_id, &module_id);
            if other_stored || other_licensed {
                kept_via_other_source.push(module_id);
                continue;
            }
            // No other source: never silently disable when the
            // tenant uses the module outside the package. When
            // nothing answers the usage query the module is
            // treated as used (fail-safe).
            if state.used_outside_package(&scope.company_id, &module_id) {
                requires_confirmation.push(module_id);
            } else {
                set_activation(&state, &scope, &module_id, false, body.requested_by.as_deref(), Some(&source_kind), Some(&source_id));
                state.record_history(
                    &scope.tenant_id,
                    None,
                    &scope.company_id,
                    &module_id,
                    "disabled",
                    body.requested_by.as_deref(),
                    json!({ "reason": "package_change", "source_kind": source_kind, "source_id": source_id }),
                );
                state.emit(
                    events::MODULE_DISABLED,
                    json!({ "id": module_id, "company_id": scope.company_id, "reason": "package_change" }),
                );
                disabled_module_ids.push(module_id);
            }
        }
        // Newly entitled modules are enabled under the new source.
        for module_id in &newly_entitled {
            state.create_entitlement(
                &scope.tenant_id,
                &scope.company_id,
                &source_kind,
                &source_id,
                body.source_ref.clone(),
                module_id,
                body.expires_at,
                body.requested_by.as_deref(),
                None,
            );
            set_activation(
                &state,
                &scope,
                module_id,
                true,
                body.requested_by.as_deref(),
                Some(&source_kind),
                Some(&source_id),
            );
            state.emit(
                events::MODULE_ENABLED,
                json!({ "id": module_id, "company_id": scope.company_id, "reason": "package_change" }),
            );
        }
        let status = if requires_confirmation.is_empty() {
            "applied"
        } else {
            "pending_confirmation"
        };
        let change = PackageChange {
            id: Uuid::now_v7(),
            tenant_id: scope.tenant_id.clone(),
            company_id: scope.company_id.clone(),
            source_kind,
            source_id,
            new_module_ids: sellable_new,
            newly_entitled,
            kept,
            kept_via_other_source,
            requires_confirmation,
            disabled_module_ids,
            status: status.to_string(),
            requested_by: body.requested_by.clone(),
            decided_by: None,
            decided_at: None,
            created_at: now,
            updated_at: now,
        };
        state
            .package_changes
            .insert(change.id, change.clone());
        Ok(change)
    })();
    match result {
        Ok(change) => json::success_response(change, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_package_changes(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let role = params.filters.get("viewer_role").cloned();
        require_capability(&ctx, role.as_deref(), PERM_ENTITLEMENTS_READ)?;
        let scope = TenantScope::resolve(&req, params.filters.get("company_id").cloned())?;
        let mut items: Vec<PackageChange> = state
            .package_changes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|change| {
                change.company_id == scope.company_id
                    && matches_search(
                        &format!("{} {}", change.source_kind, change.source_id),
                        &params,
                    )
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| change.status == *status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_package_change(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PackageChange> = (|| {
        let role = req
            .headers()
            .get("x-role")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        require_capability(&ctx, role.as_deref(), PERM_ENTITLEMENTS_READ)?;
        let scope = TenantScope::resolve(&req, None)?;
        let change = state
            .package_changes
            .get(&path.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Package change not found"))?;
        if change.company_id != scope.company_id {
            return Err(UnifiedError::not_found("Package change not found"));
        }
        Ok(change)
    })();
    json::respond(result, debug)
}

async fn confirm_package_change(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    path: web::Path<Uuid>,
    body: web::Json<ConfirmPackageChangeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let body = body.into_inner();
    let result: UnifiedResult<PackageChange> = (|| {
        let role = req
            .headers()
            .get("x-role")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        require_capability(&ctx, role.as_deref(), PERM_ENTITLEMENTS_READ)?;
        let scope = TenantScope::resolve(&req, None)?;
        let id = path.into_inner();
        let mut change = state
            .package_changes
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Package change not found"))?;
        if change.company_id != scope.company_id {
            return Err(UnifiedError::not_found("Package change not found"));
        }
        if change.status != "pending_confirmation" {
            return Err(UnifiedError::conflict(format!(
                "Package change is already {}",
                change.status
            )));
        }
        let decided_by = body.decided_by.clone();
        if body.confirm != Some(true) {
            change.status = "discarded".to_string();
            change.decided_by = decided_by.clone();
            change.decided_at = Some(Utc::now());
            change.updated_at = Utc::now();
            state.package_changes.insert(id, change.clone());
            state.record_history(
                &scope.tenant_id,
                None,
                &scope.company_id,
                "",
                "package_change_discarded",
                decided_by.as_deref(),
                json!({ "change_id": id, "reason": body.reason }),
            );
            return Ok(change);
        }
        let wanted: Vec<String> = match body.module_ids {
            Some(ids) => validate_module_id_list(&ids)?,
            None => change.requires_confirmation.clone(),
        };
        for module_id in &wanted {
            if !change.requires_confirmation.contains(module_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Module \"{module_id}\" is not listed as requires_confirmation for this change"
                )));
            }
        }
        for module_id in wanted {
            set_activation(&state, &scope, &module_id, false, decided_by.as_deref(), Some(&change.source_kind), Some(&change.source_id));
            state.record_history(
                &scope.tenant_id,
                None,
                &scope.company_id,
                &module_id,
                "disabled",
                decided_by.as_deref(),
                json!({ "reason": "package_change_confirmed", "change_id": id }),
            );
            state.emit(
                events::MODULE_DISABLED,
                json!({ "id": module_id, "company_id": scope.company_id, "reason": "package_change_confirmed" }),
            );
            change.disabled_module_ids.push(module_id);
        }
        change
            .requires_confirmation
            .retain(|module_id| !change.disabled_module_ids.contains(module_id));
        change.status = if change.requires_confirmation.is_empty() {
            "confirmed".to_string()
        } else {
            "pending_confirmation".to_string()
        };
        change.decided_by = decided_by;
        change.decided_at = Some(Utc::now());
        change.updated_at = Utc::now();
        state.package_changes.insert(id, change.clone());
        Ok(change)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: admin.
// ---------------------------------------------------------------------------

/// Admin identity from the query (`?viewer_role=` /
/// `?viewer_id=`), following the licensing module. Admin
/// endpoints run on the control plane: they never take a tenant
/// scope from a request, and they never fall back to a tenant
/// context — the caller must hold the capability and name the
/// tenant explicitly in the filter or body.
fn admin_role(query: &PaginationParams) -> Option<String> {
    query
        .filters
        .get("viewer_role")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn list_entitlements(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let result: UnifiedResult<Value> = (|| {
        require_capability(&ctx, admin_role(&params).as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        let mut items: Vec<Entitlement> = state
            .entitlements
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                matches_search(
                    &format!("{} {} {} {}", row.company_id, row.module_id, row.source_kind, row.source_id),
                    &params,
                )
                    && params
                        .filters
                        .get("company_id")
                        .is_none_or(|company_id| row.company_id == *company_id)
                    && params
                        .filters
                        .get("module_id")
                        .is_none_or(|module_id| row.module_id == *module_id)
                    && params
                        .filters
                        .get("source_kind")
                        .is_none_or(|source_kind| row.source_kind == *source_kind)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| row.status == *status)
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "module_id", "company_id"], "created_at");
        match sort.as_str() {
            "module_id" => items.sort_by(|a, b| a.module_id.cmp(&b.module_id)),
            "company_id" => items.sort_by(|a, b| a.company_id.cmp(&b.company_id)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn create_grant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    query: web::Query<ViewerQuery>,
    body: web::Json<CreateGrantRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let body = body.into_inner();
    let result: UnifiedResult<Entitlement> = (|| {
        require_capability(&ctx, query.viewer_role.as_deref(), PERM_ENTITLEMENTS_GRANT)?;
        let company_id = validate_company_id(&body.company_id)?;
        let module_id = validate_module_id(&body.module_id)?;
        if ctx.modules.get(&module_id).is_none() {
            return Err(UnifiedError::not_found(format!(
                "Module \"{module_id}\" is not registered"
            )));
        }
        // The tenant namespace id for the row. On the control plane
        // an administrator may name it explicitly; otherwise it
        // defaults to the company id, and the batch-17 tenant
        // lookup (company -> tenant namespace) replaces this
        // default at integration time. It is never invented from
        // another tenant's data.
        let tenant_id = match body.tenant_id {
            Some(tenant_id) => {
                let tenant_id = tenant_id.trim().to_string();
                Uuid::parse_str(&tenant_id).map_err(|_| {
                    UnifiedError::bad_request("tenant_id must be a UUID (uuidv7 namespace id)")
                })?;
                tenant_id
            }
            None => company_id.clone(),
        };
        if let Some(expires_at) = body.expires_at {
            if expires_at <= Utc::now() {
                return Err(UnifiedError::bad_request(
                    "expires_at must be in the future",
                ));
            }
        }
        let reason = validate_reason(body.reason.as_deref().unwrap_or(""), true)?;
        let granted_by = body
            .granted_by
            .clone()
            .or_else(|| query.viewer_id.clone())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty() && value.chars().count() <= MAX_ACTOR_LEN);
        // One active manual grant per (company, module): a second
        // grant replaces the first instead of stacking duplicates.
        let existing: Vec<Uuid> = state
            .entitlements
            .iter()
            .filter(|entry| {
                let row = entry.value();
                row.company_id == company_id
                    && row.module_id == module_id
                    && row.source_kind == SOURCE_MANUAL_GRANT
                    && row.status == STATUS_ACTIVE
            })
            .map(|entry| *entry.key())
            .collect();
        for id in existing {
            if let Some(mut entry) = state.entitlements.get_mut(&id) {
                entry.status = STATUS_REVOKED.to_string();
                entry.lapsed_at = Some(Utc::now());
                entry.lapse_reason = Some("superseded_by_new_grant".to_string());
                entry.updated_at = Utc::now();
            }
        }
        Ok(state.create_entitlement(
            &tenant_id,
            &company_id,
            SOURCE_MANUAL_GRANT,
            "manual",
            None,
            &module_id,
            body.expires_at,
            granted_by.as_deref(),
            reason,
        ))
    })();
    match result {
        Ok(entitlement) => json::success_response(entitlement, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_entitlement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    query: web::Query<ViewerQuery>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Entitlement> = (|| {
        require_capability(&ctx, query.viewer_role.as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        state
            .entitlements
            .get(&path.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Entitlement not found"))
    })();
    json::respond(result, debug)
}

async fn update_grant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    query: web::Query<ViewerQuery>,
    path: web::Path<Uuid>,
    body: web::Json<UpdateGrantRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let body = body.into_inner();
    let result: UnifiedResult<Entitlement> = (|| {
        require_capability(&ctx, query.viewer_role.as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        let id = path.into_inner();
        let mut row = state
            .entitlements
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Entitlement not found"))?;
        if let Some(status) = body.status {
            let status = status.trim().to_string();
            match status.as_str() {
                STATUS_ACTIVE | STATUS_REVOKED | STATUS_EXPIRED | STATUS_DISABLED
                | STATUS_GRACE => {
                    if status == STATUS_REVOKED && row.status != STATUS_REVOKED {
                        row.lapsed_at = Some(Utc::now());
                        row.lapse_reason = Some("revoked_by_admin".to_string());
                        state.record_history(
                            &row.tenant_id,
                            Some(id),
                            &row.company_id,
                            &row.module_id,
                            "revoked",
                            query.viewer_id.as_deref(),
                            json!({ "reason": body.reason }),
                        );
                        state.emit(
                            EVENT_ENTITLEMENT_REVOKED,
                            json!({
                                "company_id": row.company_id,
                                "module_id": row.module_id,
                                "source_kind": row.source_kind,
                                "source_id": row.source_id,
                                "entitlement_id": id,
                                "reason": "revoked_by_admin",
                            }),
                        );
                    }
                    if status == STATUS_ACTIVE && row.status != STATUS_ACTIVE {
                        row.grace_started_at = None;
                        row.grace_ends_at = None;
                        row.lapsed_at = None;
                        row.lapse_reason = None;
                        state.record_history(
                            &row.tenant_id,
                            Some(id),
                            &row.company_id,
                            &row.module_id,
                            "granted",
                            query.viewer_id.as_deref(),
                            json!({ "reason": body.reason }),
                        );
                        state.emit(
                            EVENT_ENTITLEMENT_GRANTED,
                            json!({
                                "company_id": row.company_id,
                                "module_id": row.module_id,
                                "source_kind": row.source_kind,
                                "source_id": row.source_id,
                                "entitlement_id": id,
                                "expires_at": row.expires_at,
                            }),
                        );
                    }
                    row.status = status;
                }
                _ => {
                    return Err(UnifiedError::bad_request(
                        "status must be one of: active, grace, revoked, expired, disabled",
                    ));
                }
            }
        }
        if let Some(expires_at) = body.expires_at {
            if expires_at <= Utc::now() && row.status == STATUS_ACTIVE {
                return Err(UnifiedError::bad_request(
                    "expires_at must be in the future for an active entitlement",
                ));
            }
            row.expires_at = Some(expires_at);
        }
        if let Some(reason) = body.reason {
            row.reason = validate_reason(&reason, false)?;
        }
        row.updated_at = Utc::now();
        state.entitlements.insert(id, row.clone());
        Ok(row)
    })();
    json::respond(result, debug)
}

async fn delete_grant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    query: web::Query<ViewerQuery>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_capability(&ctx, query.viewer_role.as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        let id = path.into_inner();
        let row = state
            .entitlements
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Entitlement not found"))?;
        if row.source_kind != SOURCE_MANUAL_GRANT {
            return Err(UnifiedError::conflict(
                "Only manual grants may be deleted; license, subscription, and package entitlements lapse through their own modules",
            ));
        }
        state.entitlements.remove(&id);
        // History is kept: the delete itself is recorded, and the
        // earlier history rows for this entitlement are not removed.
        // Tenant data is never deleted by this call — only the
        // grant row itself.
        state.record_history(
            &row.tenant_id,
            Some(id),
            &row.company_id,
            &row.module_id,
            "grant_deleted",
            query.viewer_id.as_deref(),
            json!({ "source_kind": row.source_kind, "source_id": row.source_id }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

async fn list_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let result: UnifiedResult<Value> = (|| {
        require_capability(&ctx, admin_role(&params).as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        let mut items: Vec<EntitlementHistoryEntry> = state
            .history
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                matches_search(&format!("{} {} {}", entry.company_id, entry.module_id, entry.action), &params)
                    && params
                        .filters
                        .get("company_id")
                        .is_none_or(|company_id| entry.company_id == *company_id)
                    && params
                        .filters
                        .get("module_id")
                        .is_none_or(|module_id| entry.module_id == *module_id)
                    && params
                        .filters
                        .get("action")
                        .is_none_or(|action| entry.action == *action)
                    && params
                        .filters
                        .get("entitlement_id")
                        .is_none_or(|entitlement_id| {
                            entry
                                .entitlement_id
                                .map(|id| id.to_string())
                                .as_deref()
                                == Some(entitlement_id.as_str())
                        })
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn list_denied_attempts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let result: UnifiedResult<Value> = (|| {
        require_capability(&ctx, admin_role(&params).as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        let mut items: Vec<DeniedAttempt> = state
            .denied_attempts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|attempt| {
                matches_search(&format!("{} {}", attempt.company_id, attempt.module_id), &params)
                    && params
                        .filters
                        .get("company_id")
                        .is_none_or(|company_id| attempt.company_id == *company_id)
                    && params
                        .filters
                        .get("module_id")
                        .is_none_or(|module_id| attempt.module_id == *module_id)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    query: web::Query<ViewerQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EntitlementSettings> = (|| {
        require_capability(&ctx, query.viewer_role.as_deref(), PERM_ENTITLEMENTS_READ)?;
        Ok(state.settings())
    })();
    json::respond(result, debug)
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    query: web::Query<ViewerQuery>,
    body: web::Json<EntitlementSettings>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let body = body.into_inner();
    let result: UnifiedResult<EntitlementSettings> = (|| {
        require_capability(&ctx, query.viewer_role.as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        body.validate()?;
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = body.clone();
        Ok(body)
    })();
    json::respond(result, debug)
}

async fn run_sweep(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    query: web::Query<ViewerQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SweepSummary> = (|| {
        require_capability(&ctx, query.viewer_role.as_deref(), PERM_ENTITLEMENTS_MANAGE)?;
        Ok(state.sweep())
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: AI (suggestion only — never purchases anything).
// ---------------------------------------------------------------------------

async fn ai_suggest(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EntitlementsState>>,
    body: web::Json<AiSuggestRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let body = body.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let role = req
            .headers()
            .get("x-role")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        require_capability(&ctx, role.as_deref(), PERM_ENTITLEMENTS_READ)?;
        let scope = TenantScope::resolve(&req, body.company_id.clone())?;
        let limit = body.limit.unwrap_or(MAX_SUGGESTIONS).max(1).min(MAX_SUGGESTIONS);
        let denied = state.top_denied_modules(&scope.company_id, limit);
        let mut suggestions: Vec<PackageSuggestion> = Vec::new();
        for (module_id, denied_attempts) in &denied {
            suggestions.push(PackageSuggestion {
                module_id: module_id.clone(),
                denied_attempts: *denied_attempts,
                upgrade_path: state.upgrade_path(&scope.company_id, module_id),
                note: format!(
                    "The tenant tried to enable \"{module_id}\" {denied_attempts} time(s) in the last {DENIED_LOOKBACK_DAYS} days without an entitlement."
                ),
            });
        }
        // Let the AI module ground or enrich the suggestion. When
        // nothing answers, the suggestion stands on the denied
        // log alone and `ai_grounded` is false — the response says
        // so instead of pretending an AI weighed in.
        let denied_modules: Vec<Value> = denied
            .iter()
            .map(|(module_id, denied_attempts)| {
                json!({ "module_id": module_id, "denied_attempts": denied_attempts })
            })
            .collect();
        let outcome = state.emit_with_results(
            EVENT_AI_SUGGEST_REQUESTED,
            json!({
                "company_id": scope.company_id,
                "tenant_id": scope.tenant_id,
                "denied_modules": denied_modules,
            }),
        );
        let mut ai_grounded = false;
        let mut ai_note: Option<String> = None;
        if let Some(outcome) = outcome {
            if let Some(note) = outcome.get("ai_note").and_then(|value| value.as_str()) {
                ai_grounded = true;
                ai_note = Some(note.to_string());
            }
            for result in &outcome.results {
                if let Some(note) = result.get("ai_note").and_then(|value| value.as_str()) {
                    ai_grounded = true;
                    ai_note = Some(note.to_string());
                }
            }
        }
        Ok(json!({
            "tenant_id": scope.tenant_id,
            "company_id": scope.company_id,
            "suggestions": suggestions,
            "ai_grounded": ai_grounded,
            "ai_note": ai_note,
            "purchases_nothing": true,
        }))
    })();
    json::respond(result, debug)
}

async fn ai_capabilities(
    ctx: web::Data<Arc<AppContext>>,
    _state: web::Data<Arc<EntitlementsState>>,
    query: web::Query<ViewerQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_capability(&ctx, query.viewer_role.as_deref(), PERM_ENTITLEMENTS_READ)?;
        Ok(json!({
            "capabilities": [{
                "id": "suggest_requested",
                "module_id": "entitlements",
                "name": "Suggest package from denied attempts",
                "description": "Suggest which package unlocks the modules a tenant has tried to use most, from the denied-attempt audit log. A suggestion only: it never purchases or provisions anything.",
                "input_fields": ["company_id"],
                "allows_pii": false,
                "use_case": "entitlements.ai.suggest_requested",
            }],
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Hook handlers.
// ---------------------------------------------------------------------------

/// Read a string field from a hook payload, trying several key
/// spellings. Emitters disagree on field names, so a lapse that
/// cannot name its source is skipped instead of erroring.
fn hook_string(ctx: &HookContext, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = ctx.get(key).and_then(|value| value.as_str()) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn hook_string_from(ctx: &HookContext, object_key: &str, keys: &[&str]) -> Option<String> {
    ctx.get(object_key)
        .and_then(|object| {
            keys.iter().find_map(|key| {
                object
                    .get(key)
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
        })
        .filter(|value| !value.trim().is_empty())
}

/// Answer an entitlement check query from stored rows only (hook
/// handlers have no AppContext, so the licensing leg is skipped,
/// never assumed). Sets `allowed`, `state`, and `reason`.
fn on_check_requested(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let company_id = hook_string(ctx, &["company_id"]);
    let module_id = hook_string(ctx, &["module_id"]);
    let Some(company_id) = company_id else { return };
    let Some(module_id) = module_id else { return };
    let effective = state.effective(None, &company_id, &module_id);
    ctx.set("allowed", json!(effective.entitled));
    ctx.set("state", json!(effective.state));
    ctx.set(
        "reason",
        json!(if effective.entitled {
            "entitled"
        } else if effective.read_only {
            "grace_read_only"
        } else {
            "no_active_entitlement"
        }),
    );
    ctx.add_result(json!({
        "company_id": company_id,
        "module_id": module_id,
        "allowed": effective.entitled,
        "state": effective.state,
    }));
}

/// Answer a write check query: writes are allowed only while the
/// module is entitled — locked and read-only-grace both refuse.
fn on_write_check_requested(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let company_id = hook_string(ctx, &["company_id"]);
    let module_id = hook_string(ctx, &["module_id"]);
    let Some(company_id) = company_id else { return };
    let Some(module_id) = module_id else { return };
    let effective = state.effective(None, &company_id, &module_id);
    ctx.set("write_allowed", json!(effective.write_allowed));
    ctx.set("state", json!(effective.state));
    ctx.set(
        "error_code",
        json!(if effective.write_allowed {
            Value::Null
        } else if effective.read_only {
            json!(ERROR_ENTITLEMENT_GRACE_READ_ONLY)
        } else {
            json!(ERROR_ENTITLEMENT_REQUIRED)
        }),
    );
    ctx.add_result(json!({
        "company_id": company_id,
        "module_id": module_id,
        "write_allowed": effective.write_allowed,
        "state": effective.state,
    }));
}

/// Record a grant requested by another module (SaaS packages
/// provisioning, store checkout). Control-plane sources are read
/// only through this command and the owning modules' events —
/// never by querying their tables directly.
fn on_grant_requested(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let company_id = hook_string(ctx, &["company_id"]);
    let module_id = hook_string(ctx, &["module_id"]);
    let source_kind = hook_string(ctx, &["source_kind"]).unwrap_or_else(|| SOURCE_SAAS_PACKAGE.to_string());
    let source_id = hook_string(ctx, &["source_id"]);
    let Some(company_id) = company_id else {
        ctx.set("granted", json!(false));
        ctx.set("reason", json!("company_id is required"));
        return;
    };
    let Some(module_id) = module_id else {
        ctx.set("granted", json!(false));
        ctx.set("reason", json!("module_id is required"));
        return;
    };
    let Some(source_id) = source_id else {
        ctx.set("granted", json!(false));
        ctx.set("reason", json!("source_id is required"));
        return;
    };
    if !SOURCE_KINDS.contains(&source_kind.as_str()) {
        ctx.set("granted", json!(false));
        ctx.set("reason", json!("unknown source_kind"));
        return;
    }
    if validate_module_id(&module_id).is_err() {
        ctx.set("granted", json!(false));
        ctx.set("reason", json!("invalid module_id"));
        return;
    }
    // The tenant namespace id when the emitter knows it; the
    // company id otherwise (same documented default as the admin
    // grant endpoint, replaced by the batch-17 tenant lookup at
    // integration time).
    let tenant_id = hook_string(ctx, &["tenant_id"]).unwrap_or_else(|| company_id.clone());
    let source_ref = hook_string(ctx, &["source_ref"]);
    let expires_at = ctx
        .get("expires_at")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<DateTime<Utc>>().ok());
    let actor_id = hook_string(ctx, &["actor_id"]);
    let entitlement = state.create_entitlement(
        &tenant_id,
        &company_id,
        &source_kind,
        &source_id,
        source_ref,
        &module_id,
        expires_at,
        actor_id.as_deref(),
        None,
    );
    ctx.set("granted", json!(true));
    ctx.set("entitlement_id", json!(entitlement.id));
    ctx.add_result(json!({ "granted": true, "entitlement_id": entitlement.id }));
}

/// Answer the AI suggestion query from the denied-attempt log.
/// Only counts are shared — never buyer personal data.
fn on_ai_suggest_requested(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let company_id = hook_string(ctx, &["company_id"]);
    let Some(company_id) = company_id else { return };
    let denied = state.top_denied_modules(&company_id, MAX_SUGGESTIONS);
    let modules: Vec<Value> = denied
        .iter()
        .map(|(module_id, denied_attempts)| {
            json!({ "module_id": module_id, "denied_attempts": denied_attempts })
        })
        .collect();
    ctx.set("suggestion", json!({ "company_id": company_id, "denied_modules": modules }));
    ctx.add_result(json!({ "company_id": company_id, "denied_modules": modules }));
}

/// A license lapsed (`license.revoked`, or the expiry scan's
/// `license.expiring` — which only notes, never lapses). The
/// licensing payload carries `license_id` and no company id; the
/// licensing leg of the effective check already reflects the
/// revocation on the next read, and any stored license-sourced row
/// lapses here.
fn on_license_revoked(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let Some(license_id) = hook_string(ctx, &["license_id", "id"]) else {
        return;
    };
    let company_id = hook_string(ctx, &["company_id"]);
    state.apply_source_lapse(
        SOURCE_LICENSE,
        &license_id,
        company_id.as_deref(),
        "license_revoked",
    );
}

fn on_license_expiring(_state: Arc<EntitlementsState>, _ctx: &mut HookContext) {
    // Expiring is a warning, not a lapse: it names no module and no
    // tenant, so there is nothing tenant-scoped to record here. The
    // sweep lapses the entitlement when the expiry actually passes,
    // and the licensing module owns the renewal reminder itself.
}

/// A subscription lapsed. The store payload carries
/// `subscription_id` plus a nested `subscription` object that may
/// name the company.
fn on_subscription_cancelled(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let Some(subscription_id) = hook_string(ctx, &["subscription_id", "id"]) else {
        return;
    };
    let company_id = hook_string(ctx, &["company_id"])
        .or_else(|| hook_string_from(ctx, "subscription", &["company_id"]));
    state.apply_source_lapse(
        SOURCE_SUBSCRIPTION,
        &subscription_id,
        company_id.as_deref(),
        "subscription_cancelled",
    );
}

fn on_subscription_expired(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let Some(subscription_id) = hook_string(ctx, &["subscription_id", "id"]) else {
        return;
    };
    let company_id = hook_string(ctx, &["company_id"])
        .or_else(|| hook_string_from(ctx, "subscription", &["company_id"]));
    state.apply_source_lapse(
        SOURCE_SUBSCRIPTION,
        &subscription_id,
        company_id.as_deref(),
        "subscription_expired",
    );
}

/// A SaaS package purchase was cancelled. The payload names both
/// the purchase and the package; entitlements may be sourced from
/// either id, so both are lapsed.
fn on_package_cancelled(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let company_id = hook_string(ctx, &["company_id"]);
    if let Some(purchase_id) = hook_string(ctx, &["purchase_id"]) {
        state.apply_source_lapse(
            SOURCE_SAAS_PACKAGE,
            &purchase_id,
            company_id.as_deref(),
            "package_cancelled",
        );
    }
    if let Some(package_id) = hook_string(ctx, &["package_id"]) {
        state.apply_source_lapse(
            SOURCE_SAAS_PACKAGE,
            &package_id,
            company_id.as_deref(),
            "package_cancelled",
        );
    }
}

/// Mirror a tenants-module grant into a stored entitlement
/// row, so the runtime gate and the control-plane grant record
/// agree. `granted_via` maps onto this module's source kinds;
/// `preset` and `builtin` map onto nothing: a preset is a settings
/// bundle, not a purchase source, and a builtin (non-sellable)
/// module needs no entitlement at all — the gate already allows
/// it. The grant event carries no grant id, so the row's
/// `source_id` is the tenant id: the grant record itself lives in
/// `tenant_module_grants`, keyed by tenant and module.
fn on_tenant_module_granted(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let company_id = hook_string(ctx, &["company_id"]);
    let module_id = hook_string(ctx, &["module_id"]);
    let granted_via = hook_string(ctx, &["granted_via"]).unwrap_or_default();
    let Some(company_id) = company_id else { return };
    let Some(module_id) = module_id else { return };
    if validate_module_id(&module_id).is_err() {
        return;
    }
    let source_kind = match granted_via.as_str() {
        "license" => SOURCE_LICENSE,
        "subscription" => SOURCE_SUBSCRIPTION,
        "package" => SOURCE_SAAS_PACKAGE,
        "manual_grant" => SOURCE_MANUAL_GRANT,
        "owner_entitlement" => SOURCE_OWNER_ENTITLEMENT,
        _ => return,
    };
    let tenant_id = hook_string(ctx, &["tenant_id"]).unwrap_or_else(|| company_id.clone());
    let actor_id = hook_string(ctx, &["actor_id"]);
    let already = state.entitlements.iter().any(|entry| {
        let row = entry.value();
        row.company_id == company_id
            && row.module_id == module_id
            && row.source_kind == source_kind
            && row.source_id == tenant_id
            && row.status == STATUS_ACTIVE
    });
    if already {
        return;
    }
    state.create_entitlement(
        &tenant_id,
        &company_id,
        source_kind,
        &tenant_id,
        None,
        &module_id,
        None,
        actor_id.as_deref(),
        Some(format!("tenant.module.granted via {granted_via}")),
    );
}

/// Mirror a tenants-module grant revocation: the module's
/// non-manual rows lapse into grace (or disable). Manual grants
/// survive; see `revoke_module_rows`.
fn on_tenant_module_revoked(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let company_id = hook_string(ctx, &["company_id"]);
    let module_id = hook_string(ctx, &["module_id"]);
    let reason = hook_string(ctx, &["reason"]).unwrap_or_else(|| "tenant_module_revoked".to_string());
    let Some(company_id) = company_id else { return };
    let Some(module_id) = module_id else { return };
    state.revoke_module_rows(&company_id, &module_id, &reason);
}

// ---------------------------------------------------------------------------
// Module.
// ---------------------------------------------------------------------------

pub struct EntitlementsModule {
    state: Arc<EntitlementsState>,
}

impl EntitlementsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(EntitlementsState::new()),
        }
    }
}

impl Default for EntitlementsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for EntitlementsModule {
    fn id(&self) -> &'static str {
        "entitlements"
    }

    fn name(&self) -> &'static str {
        "Per-Product Entitlements"
    }

    fn description(&self) -> &'static str {
        "Per-product feature limiting: entitlement-scoped module activation per tenant (license, subscription, SaaS package, or manual grant), with read-only grace on lapse, package-change diffs that never silently disable a used module, and an AI package suggestion grounded in denied attempts."
    }

    fn requires(&self) -> &[&'static str] {
        &["licensing", "extensions", "roles"]
    }

    fn available_as_product(&self) -> bool {
        false
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        // Hand the hook bus to the state so hook handlers (which
        // receive no AppContext) can still emit follow-up events.
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
            EVENT_CHECK_REQUESTED,
            "entitlements",
            Arc::new(move |ctx: &mut HookContext| {
                on_check_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_SAAS_ENTITLEMENT_CHECK_REQUESTED,
            "entitlements",
            Arc::new(move |ctx: &mut HookContext| {
                on_check_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_WRITE_CHECK_REQUESTED,
            "entitlements",
            Arc::new(move |ctx: &mut HookContext| {
                on_write_check_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_GRANT_REQUESTED,
            "entitlements",
            Arc::new(move |ctx: &mut HookContext| {
                on_grant_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_AI_SUGGEST_REQUESTED,
            "entitlements",
            Arc::new(move |ctx: &mut HookContext| {
                on_ai_suggest_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            LICENSE_REVOKED_EVENT,
            "entitlements",
            Arc::new(move |ctx: &mut HookContext| {
                on_license_revoked(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            LICENSE_EXPIRING,
            "entitlements",
            Arc::new(move |ctx: &mut HookContext| {
                on_license_expiring(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            SUBSCRIPTION_CANCELLED_EVENT,
            "entitlements",
            Arc::new(move |ctx: &mut HookContext| {
                on_subscription_cancelled(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            SUBSCRIPTION_EXPIRED_EVENT,
            "entitlements",
            Arc::new(move |ctx: &mut HookContext| {
                on_subscription_expired(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            SAAS_PACKAGE_CANCELLED_EVENT,
            "entitlements",
            Arc::new(move |ctx: &mut HookContext| {
                on_package_cancelled(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            TENANT_MODULE_GRANTED_EVENT,
            "entitlements",
            Arc::new(move |ctx: &mut HookContext| {
                on_tenant_module_granted(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            TENANT_MODULE_REVOKED_EVENT,
            "entitlements",
            Arc::new(move |ctx: &mut HookContext| {
                on_tenant_module_revoked(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/entitlements")
                // Specific paths first: `/{id}` below must not
                // capture them.
                .route("/my-modules", web::get().to(list_my_modules))
                .route("/modules/{module_id}/access", web::get().to(get_module_access))
                .route("/modules/{module_id}/enable", web::post().to(enable_module))
                .route("/modules/{module_id}/disable", web::post().to(disable_module))
                .route("/package-changes/apply", web::post().to(apply_package_change))
                .route("/package-changes", web::get().to(list_package_changes))
                .route("/package-changes/{id}", web::get().to(get_package_change))
                .route("/package-changes/{id}/confirm", web::post().to(confirm_package_change))
                .route("/ai/suggest", web::post().to(ai_suggest))
                .route("/ai/capabilities", web::get().to(ai_capabilities))
                .route("/history", web::get().to(list_history))
                .route("/denied-attempts", web::get().to(list_denied_attempts))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/sweep", web::post().to(run_sweep))
                .route("/grants", web::post().to(create_grant))
                .route("", web::get().to(list_entitlements))
                .route("/{id}", web::get().to(get_entitlement))
                .route("/{id}", web::patch().to(update_grant))
                .route("/{id}", web::delete().to(delete_grant)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/entitlements/my-modules", owner: "entitlements", description: "List every module with the calling tenant's per-module entitlement state and upgrade path (tenant context required, fail closed)." },
            RouteInfo { method: "GET", path: "/api/v1/entitlements/modules/{module_id}/access", owner: "entitlements", description: "One module's access state for the calling tenant: entitled, grace_read_only, or locked." },
            RouteInfo { method: "POST", path: "/api/v1/entitlements/modules/{module_id}/enable", owner: "entitlements", description: "Enable a module after the server-side entitlement check; 403 entitlement.required and an audited attempt when not entitled." },
            RouteInfo { method: "POST", path: "/api/v1/entitlements/modules/{module_id}/disable", owner: "entitlements", description: "Disable a module the tenant enabled." },
            RouteInfo { method: "POST", path: "/api/v1/entitlements/package-changes/apply", owner: "entitlements", description: "Compute and apply the module diff for a new package/product set; lost modules used outside the package require explicit confirmation." },
            RouteInfo { method: "GET", path: "/api/v1/entitlements/package-changes", owner: "entitlements", description: "List package-change records for the calling tenant, paginated." },
            RouteInfo { method: "GET", path: "/api/v1/entitlements/package-changes/{id}", owner: "entitlements", description: "One package-change record for the calling tenant." },
            RouteInfo { method: "POST", path: "/api/v1/entitlements/package-changes/{id}/confirm", owner: "entitlements", description: "Confirm disabling the modules a package change listed as requires_confirmation, or discard the change." },
            RouteInfo { method: "POST", path: "/api/v1/entitlements/ai/suggest", owner: "entitlements", description: "Suggest which package unlocks the modules the tenant tried to use most (suggestion only, never purchases)." },
            RouteInfo { method: "GET", path: "/api/v1/entitlements/ai/capabilities", owner: "entitlements", description: "The AI capabilities this module offers." },
            RouteInfo { method: "GET", path: "/api/v1/entitlements/history", owner: "entitlements", description: "Entitlement history per tenant/module (admin, paginated, filtered)." },
            RouteInfo { method: "GET", path: "/api/v1/entitlements/denied-attempts", owner: "entitlements", description: "Denied enable attempts (admin, paginated, filtered)." },
            RouteInfo { method: "GET", path: "/api/v1/entitlements/settings", owner: "entitlements", description: "Entitlement settings (grace period, read-only grace, default deny)." },
            RouteInfo { method: "PUT", path: "/api/v1/entitlements/settings", owner: "entitlements", description: "Update entitlement settings (admin)." },
            RouteInfo { method: "POST", path: "/api/v1/entitlements/sweep", owner: "entitlements", description: "Run the lapse/grace sweep now (admin)." },
            RouteInfo { method: "POST", path: "/api/v1/entitlements/grants", owner: "entitlements", description: "Create a manual grant (admin, reason required)." },
            RouteInfo { method: "GET", path: "/api/v1/entitlements", owner: "entitlements", description: "List entitlements (admin, paginated, filtered by company/module/source/status)." },
            RouteInfo { method: "GET", path: "/api/v1/entitlements/{id}", owner: "entitlements", description: "One entitlement (admin)." },
            RouteInfo { method: "PATCH", path: "/api/v1/entitlements/{id}", owner: "entitlements", description: "Revoke, extend, or change a grant (admin)." },
            RouteInfo { method: "DELETE", path: "/api/v1/entitlements/{id}", owner: "entitlements", description: "Delete a manual-grant row (admin; history is kept, never tenant data)." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(EntitlementsModule::new()));
}
