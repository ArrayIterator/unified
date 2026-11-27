// Per-product feature limiting (entitlements) module.
//
// The selling model: a tenant (company) may buy one product at a
// time - one module, or one SaaS package bundling several modules -
// and the tenant dashboard may activate only what was purchased.
// An entitlement is DATA, never code: one row grants one module to
// one tenant from one admin-defined source. The effective
// entitlement for a module is the UNION of its active sources: any
// active source grants the module. No entitlement record means not
// entitled (fail closed).
//
// Sources (`source_kind`, a closed taxonomy - the sources
// themselves are admin-defined records):
//   - `owner_entitlement` - the implicit owner entitlement for a
//                       sellable CORE module (the same rule as
//                       `licensing::check_module_entitlement`).
//   - `license`       - a licensing-module license whose product
//                       names the module (composed through
//                       `licensing::has_module_entitlement`, never
//                       re-implemented here).
//   - `subscription`  - a billing/store subscription backing the
//                       module.
//   - `saas_package`  - a SaaS package purchase (see
//                       `crate::modules::saas_packages`).
//   - `manual_grant`  - an administrator grant, with a reason.
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
//     GET    /my-modules - every registered module with its
//            per-tenant state (entitled with source + expiry, or
//            locked with an upgrade path), paginated.
//     GET    /modules/{module_id}/access - one module's access
//            state: entitled / grace_read_only / locked, and
//            whether writes are allowed.
//     POST   /modules/{module_id}/enable - enable after the
//            server-side entitlement check (403
//            `entitlement.required` + audited attempt when not
//            entitled).
//     POST   /modules/{module_id}/disable - disable a module the
//            tenant enabled.
//   Package / product-set changes:
//     POST   /package-changes/apply - compute the module diff for
//            a new package/product set (newly entitled, kept,
//            kept via another source, requires_confirmation).
//            Newly entitled modules are enabled; modules that
//            would be lost are NEVER silently disabled when the
//            tenant uses them outside the package. A lost module
//            with no other source has the replaced source's rows
//            lapsed (grace, then disabled) - the activation flag
//            alone is never the revocation, because `effective()`
//            reads any active row as entitled.
//     GET    /package-changes - list change records (paginated).
//     GET    /package-changes/{id} - one change record.
//     POST   /package-changes/{id}/confirm - explicitly confirm
//            disabling the modules listed as
//            `requires_confirmation` (or discard the change).
//   Admin:
//     GET    / - list entitlements (paginated, filter by
//            company_id / module_id / source_kind / status).
//     POST   /grants - create a manual grant (reason required).
//     GET    /{id} - one entitlement.
//     PATCH  /{id} - revoke / extend / change a grant.
//     DELETE /{id} - delete a manual-grant row (admin only;
//            history is kept; never tenant data).
//     GET    /history - entitlement history per tenant/module
//            (paginated, filter by company_id / module_id /
//            action).
//     GET    /denied-attempts - denied enable attempts (paginated,
//            filter by company_id / module_id).
//     GET    /settings - entitlement settings (view).
//     PUT    /settings - update settings (manage).
//     POST   /sweep - run the lapse/grace sweep now (manage).
//   AI (smart module, suggestion only - never purchases):
//     POST   /ai/suggest - suggest which package unlocks the
//            modules this tenant tried to use most, from the
//            denied-attempt audit log.
//     GET    /ai/capabilities - the AI capabilities this module
//            offers.
//
// Events emitted on the hook bus:
//   - `entitlement.granted` - `{"company_id", "module_id",
//     "source_kind", "source_id", "entitlement_id", "expires_at"}`
//   - `entitlement.revoked` - `{"company_id", "module_id",
//     "source_kind", "source_id", "entitlement_id", "reason"}`
//   - `entitlement.grace_started` - `{"company_id", "module_id",
//     "entitlement_id", "grace_ends_at", "reason"}`
//   - `entitlement.disabled` - `{"company_id", "module_id",
//     "entitlement_id", "reason"}`
//   - `entitlement.enable_denied` - `{"company_id", "module_id",
//     "actor_id", "reason", "error_code"}`
//   - `module.enabled` / `module.disabled` (core events) when a
//     module is enabled/disabled for a tenant through this module.
//   - `module.entitlement.granted` / `module.entitlement.denied`
//     (core constants) are emitted by the licensing check this
//     module composes with; this module does not re-emit them.
//   - `entitlement.upgrade_path_requested` - hook QUERY emitted
//     when a locked module needs an upgrade path:
//     `{"company_id", "module_id", "upgrade_paths": []}`.
//     A handler (SaaS packages / products) may set
//     `upgrade_paths` to `[{"kind", "id", "name"}]`; when nothing
//     answers, the path is null with reason
//     `no_upgrade_path_provider` - never a fabricated product.
//   - `entitlement.module_usage_requested` - hook QUERY emitted
//     by the package-change diff: `{"company_id", "module_id",
//     "used_outside_package": false}`. A handler that knows the
//     tenant uses the module outside the package sets the flag;
//     when nothing answers, the diff treats the module as used
//     (fail-safe: it lands in `requires_confirmation`).
//   - `entitlements.ai.suggest_requested` - hook QUERY the AI
//     endpoint emits so the AI module can ground or enrich the
//     suggestion: `{"company_id", "denied_modules": [...]}`.
//     This module answers it from the denied-attempt log.
// Events this module listens to (all read defensively - a missing
// field or an unknown record is skipped, never an error):
//   - `license.revoked`, `license.expiring` (licensing) -
//     entitlements sourced from that license lapse.
//   - `subscription.cancelled`, `subscription.expired` (store /
//     billing) - entitlements sourced from that subscription lapse.
//   - `saas_packages.cancelled` - entitlements sourced from that
//     package purchase lapse.
//   - `tenant.module.granted` (tenants) - mirror the grant into a
//     stored entitlement row (`granted_via` maps onto the source
//     kinds; `preset` and `builtin` map onto nothing, see the
//     handler). `tenant.module.revoked` - the module's non-manual
//     rows lapse into grace; manual grants survive it.
//   - `entitlements.check_requested` and
//     `saas_packages.entitlement.check_requested` - hook QUERY:
//     `{"company_id", "module_id", "allowed": false}`; answered
//     with the effective state (this is the answer the SaaS
//     packages provisioning step waits for). The licensing leg is
//     evaluated through the `AppContext` stashed at `init`, so
//     the hook answer agrees with the HTTP path.
//   - `entitlements.write.check_requested` - hook QUERY:
//     `{"company_id", "module_id", "write_allowed": false}`;
//     answered false while the module is locked or in read-only
//     grace.
//   - `entitlements.grant_requested` - hook COMMAND: another
//     module (SaaS packages provisioning, store checkout) asks
//     this module to record a grant:
//     `{"company_id", "module_id", "source_kind", "source_id",
//     "source_ref", "expires_at", "actor_id", "emitter"}`.
//     Answered with `granted: true` and the entitlement id, or
//     `granted: false` with a reason. The emitter must name itself
//     (`emitter`) and be on the `GRANT_EMITTERS` allowlist
//     (`saas_packages`, `store`); anything else is rejected and
//     mints nothing.
//   - `entitlements.ai.suggest_requested` - answered as above.
//
// Persistence: the live data lives in in-memory DashMap stores
// (the same approach the licensing and SaaS-packages modules use).
// The Diesel tables for durable persistence are the migration
// `2026-11-22-000092_entitlements`; the schema and model entries
// are supplied to the integrator in
// docs/integration/INTEGRATION_NOTES_BATCH15_B1.md. Nothing in this file references
// a table that does not exist yet, so it compiles against the
// current schema.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (actor) and
// `X-Role` (role slug the capability check runs against), read
// from the headers and nowhere else. `?viewer_id=` /
// `?viewer_role=` query parameters are never identity: a role slug
// named in a URL authorizes nothing. Admin endpoints read headers
// only, and an anonymous caller (no headers) gets 401. Capability checks use the
// `role.check` hook and fail closed (no answer means not
// allowed). This is the known gap the licensing module documents:
// the headers arrive as trusted input until the auth extractor
// lands.
//
// TENANT-SCOPED DATA ACCESS (standing rule). Every tenant-scoped
// endpoint in this module resolves its tenant ONLY through the
// tenant context (`TenantScope::resolve`, backed by
// `TenantContext::from_request` - the context the
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
//     - this module never queries the licenses, subscriptions,
//     or packages tables directly (it references no control-plane
//     table at all; its only Diesel surface is its own migration
//     tables, supplied as snippets for the integrator).
//   - The per-tenant activation state (`entitlement_module_
//     activations`) and every entitlement/history/denied row IS
//     tenant business data: each row carries the tenant namespace
//     id (`tenant_id`, uuidv7) alongside the company key, is
//     written only under a resolved tenant scope, and its durable
//     tables (`2026-11-22-000092_entitlements`) are TENANT-SCHEMA
//     tables - created per tenant schema by the migration runner
//     with the search_path set to that tenant's schema - never
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
// at the time of writing - the plumbing EXISTS now:
//   - `unified_core::tenancy` (crates/unified-core/src/tenancy.rs`): the single
//     source of truth for schema-per-tenant isolation, with
//     `TenantContext { tenant_id: Uuid, schema_name,
//     company_id: Option<String>, site_id: Option<String> }`,
//     `TenantContext::from_http_request(req)` (fail-closed
//     extractor: 401 when no tenant context, 403 when the tenant
//     id is not a UUID), `schema_name(Uuid)` - the ONLY place a
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
//     a schema name itself - it reports the derived
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
//     provisioning - no per-module registration is needed for it.
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
//     it registers - same status as `saas_packages`.
//   - REMAINING GAP: the in-memory DashMap stores used this
//     round are process-global keyed by company id, not
//     per-schema Diesel reads/writes through
//     `with_tenant_connection`; the durable per-schema wiring
//     (schema/models entries in the notes) replaces them at
//     integration time. Until then a restart loses grants made
//     since the last durable write - the same limitation the
//     licensing and SaaS-packages modules carry.
//   - ALSO OPEN: the admin grant endpoint and the grant hook
//     resolve the tenant namespace id through
//     `resolve_grant_tenant_id`: an explicitly named tenant id,
//     else the company id itself when it is a tenant UUID (the
//     tenants module uses the tenant id as the company id). A
//     non-UUID company with no explicit tenant fails closed with
//     400 (admin) or `granted: false` (hook) - the old default
//     of `tenant_id = company_id` is gone. There is no separate
//     company -> tenant lookup in the tree to consult.
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
//     modules as entitled only through the licensing check -
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
// OPEN - known gaps, recorded honestly instead of hidden:
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

// Module registration: struct, Module impl and register().

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::licensing::LICENSE_EXPIRING;
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::*;

// Module.

pub struct EntitlementsModule {
    pub(crate) state: Arc<EntitlementsState>,
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

        ai::register_ai_capabilities("entitlements", AI_CAPABILITIES);
        // Hand the hook bus to the state so hook handlers (which
        // receive no AppContext) can still emit follow-up events.
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        // Stash the shared context so the synchronous hook handlers
        // can run the licensing leg of the effective check (see
        // `state::stash_ctx`): without it the hook gate and the HTTP
        // path disagree on license-only entitlements.
        let _ = super::state::ENTITLEMENTS_CTX.set(super::state::stash_ctx(ctx));
        // Stash the state itself so the licensing gate
        // (`check_module_entitlement`, which only ever sees an
        // `AppContext`) can consult the same stored rows
        // `effective()` reads through the one shared
        // `entitled_via_live_sources` decision.
        super::state::stash_entitlements_state(self.state.clone());
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
            RouteInfo { method: "GET", path: "/api/v1/entitlements/my-modules", owner: "entitlements", description: "List every module with the calling tenant's per-module entitlement state and upgrade path (tenant context required, fail closed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/entitlements/modules/{module_id}/access", owner: "entitlements", description: "One module's access state for the calling tenant: entitled, grace_read_only, or locked.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/entitlements/modules/{module_id}/enable", owner: "entitlements", description: "Enable a module after the server-side entitlement check; 403 entitlement.required and an audited attempt when not entitled.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/entitlements/modules/{module_id}/disable", owner: "entitlements", description: "Disable a module the tenant enabled.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/entitlements/package-changes/apply", owner: "entitlements", description: "Compute and apply the module diff for a new package/product set; lost modules used outside the package require explicit confirmation.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/entitlements/package-changes", owner: "entitlements", description: "List package-change records for the calling tenant, paginated.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/entitlements/package-changes/{id}", owner: "entitlements", description: "One package-change record for the calling tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/entitlements/package-changes/{id}/confirm", owner: "entitlements", description: "Confirm disabling the modules a package change listed as requires_confirmation, or discard the change.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/entitlements/ai/suggest", owner: "entitlements", description: "Suggest which package unlocks the modules the tenant tried to use most (suggestion only, never purchases).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/entitlements/ai/capabilities", owner: "entitlements", description: "The AI capabilities this module offers.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/entitlements/history", owner: "entitlements", description: "Entitlement history per tenant/module (admin, paginated, filtered).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/entitlements/denied-attempts", owner: "entitlements", description: "Denied enable attempts (admin, paginated, filtered).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/entitlements/settings", owner: "entitlements", description: "Entitlement settings (grace period, read-only grace, default deny).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/entitlements/settings", owner: "entitlements", description: "Update entitlement settings (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/entitlements/sweep", owner: "entitlements", description: "Run the lapse/grace sweep now (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/entitlements/grants", owner: "entitlements", description: "Create a manual grant (admin, reason required).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/entitlements", owner: "entitlements", description: "List entitlements (admin, paginated, filtered by company/module/source/status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/entitlements/{id}", owner: "entitlements", description: "One entitlement (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/entitlements/{id}", owner: "entitlements", description: "Revoke, extend, or change a grant (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/entitlements/{id}", owner: "entitlements", description: "Delete a manual-grant row (admin; history is kept, never tenant data).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(EntitlementsModule::new()));
}
