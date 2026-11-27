//! Saas Packages - Module definition: routes, hooks, and registration.

// SaaS Packages module -- sellable product bundles of modules.
//
// A package is DATA, never code: an admin or vendor defines a named
// bundle (module list, preset settings payload, wizard steps, and a
// link to a product/plan in the products module), publishes it, and
// customers buy it through the store / billing / subscription flow.
// Buying never provisions anything by itself: on `order.paid` (or a
// subscription activation) this module runs PROVISIONING for the
// purchase -- validate entitlements, provision a tenant subdomain,
// enable the package modules in dependency order, hand the preset
// settings off to the owning modules, pre-fill the setup wizard,
// and send the welcome message. Every step is recorded with its own
// outcome, failed steps can be retried, and the full history is kept.
//
// Routes live under `/api/v1/saas-packages`:
//
//   Packages:
//     GET    /packages -- list (paginated, filter by status, search).
//     POST   /packages -- create a package in draft (manage).
//     GET    /packages/{id} -- one package (id or slug).
//     PATCH  /packages/{id} -- edit; editing a published package
//            creates a new version, existing purchases keep theirs.
//     DELETE /packages/{id} -- delete a package with no purchases.
//     POST   /packages/{id}/publish -- draft -> published.
//     POST   /packages/{id}/archive -- published/draft -> archived.
//     POST   /packages/{id}/clone -- clone as a new draft package.
//     GET    /packages/{id}/versions -- version snapshots.
//     GET    /packages/{id}/versions/{version} -- one snapshot.
//     POST   /packages/{id}/validate -- module/entitlement preview.
//     GET    /packages/{id}/modules -- current module list.
//     GET    /packages/{id}/diff/{other_id} -- module diff of two packages.
//   Purchases:
//     POST   /packages/{id}/purchase -- buy (emits a purchase request
//            for the store; checkout itself lives in the store).
//     GET    /purchases -- list purchases (paginated, filter by
//            package_id, company_id, buyer_id, status).
//     GET    /purchases/{id} -- one purchase.
//     POST   /purchases/{id}/cancel -- cancel, with a grace period.
//     POST   /purchases/{id}/provision -- start provisioning.
//     POST   /purchases/{id}/provision/retry -- retry failed/blocked steps.
//     GET    /purchases/{id}/provision -- provisioning status + steps.
//     GET    /purchases/{id}/provision/steps -- paginated step list.
//     GET    /provision-steps/{id} -- one provisioning step.
//     POST   /purchases/{id}/upgrade -- move to another package.
//     POST   /purchases/{id}/downgrade -- move to another package.
//     GET    /purchases/{id}/history -- purchase history.
//     GET    /tenants/{company_id}/purchases -- purchases of one tenant.
//     POST   /purchases/{id}/renew -- request renewal via billing.
//   AI (smart module):
//     POST   /ai/recommend-package -- suggest a package (AI gated).
//     POST   /ai/describe-package -- describe a package (AI gated).
//     GET    /ai/capabilities -- the AI capabilities this module offers.
//   Settings:
//     GET    /settings -- configurable defaults (view).
//     PUT    /settings -- update the defaults (manage).
//
// HONESTY -- READ THIS FIRST. What is real in this module:
// in-memory package / version / purchase / provisioning-step /
// history management; package versioning (published edits create a
// new version, purchases pin theirs); purchase requests handed to
// the store through events (checkout, payment capture, and
// subscription billing live in the store/billing modules); per-step
// provisioning with per-step outcomes, retry, and a full audit
// trail; entitlement blocking (a package module the tenant is not
// entitled to BLOCKS provisioning with a 403-style error naming
// the module -- it is never silently skipped); upgrade/downgrade
// with a module diff that never disables a module the tenant uses
// outside the package without explicit confirmation; cancel with
// a grace period tied to the subscription lifecycle events. What
// is NOT built here: durable storage (the DashMap stores are
// authoritative; migration `2026-11-22-000096_saas_packages` is
// the persistence target); checkout or payment capture (the store
// owns it -- this module only requests a purchase and reacts to
// `order.paid`); the subdomain itself (the domains flow owns
// allocation -- this module only emits the provision request and
// records the handoff); the setup wizard itself (the presets
// module owns it -- this module only emits the pre-fill request);
// settings application (owning modules apply the settings this
// module hands off through events); email/push delivery (the
// notifications module owns delivery -- this module only emits
// `notification.requested`); and the AI model call itself (the AI
// module owns the gate and the provider call -- this module only
// declares capabilities and delegates through
// `run_ai_capability`).
//
// CONFIG-DRIVEN -- READ THIS FIRST. Nothing behavioural is
// hard-coded: packages, their module lists, settings payloads,
// wizard steps, prices (held only as a product id reference --
// money never lives here), the grace period, the module cap, and
// the wizard step vocabulary all live in data or in
// `SaasPackagesSettings`, editable by an administrator through
// `PUT /settings`. The only hard-coded vocabulary is disclosed in
// the OPEN notes at the end of this header: the closed package
// statuses, the closed purchase/provisioning statuses, the six
// provisioning pipeline step names, the four capability names,
// and the seed example package (which is DATA -- the code works
// with zero packages defined, and an admin can edit or delete
// the seed like any other package).
//
// TENANT SCOPING -- READ THIS FIRST (standing rule). Package
// definitions are CONTROL-PLANE data (they live in the `public`
// schema with the other control-plane tables). Package
// administration (create / edit / delete / publish / archive /
// clone, settings) is control-plane access and enforces
// `require_control_plane` (mirroring
// `tenancy::ControlPlaneGuard`): a request that resolved to a
// tenant host is rejected outright (403). The storefront reads
// (listing / getting published packages) are the exception:
// tenants must be able to browse the catalog.
// Everything else this module stores or reads is TENANT data and
// is always resolved through the tenant context, using the batch
// 17 tenancy plumbing in crates/unified-core/src/tenancy.rs` (which LANDED
// while this module was being written, with migration
// `000094_tenant_isolation`):
//   - Every purchase, provisioning step, history entry, and
//     enabled-module record is keyed by the tenant's company id
//     (a uuidv7 tenant id). There is no unscoped read anywhere:
//     list endpoints filter by an explicit company id, and a
//     single-record read is always checked against the caller's
//     buyer ownership or the manage capability.
//   - When the tenant-resolution middleware attached a
//     `TenantContext` to the request (a request on a tenant host),
//     `require_tenant_scope` enforces that the requested company
//     matches the resolved tenant -- validated through
//     `tenancy::TenantContext::from_resolved`, so a tenant id that
//     is not a uuidv7 UUID is rejected outright -- and a mismatch
//     is a 403, fail closed. A tenant-host request can never read
//     another tenant's purchase, steps, or history.
//   - Every provisioning step runs for exactly one tenant: the
//     purchase's company id is carried into every step, every
//     emitted event, and every history entry; the tenant schema
//     name travels with it, built only through
//     `tenancy::schema_name` (see `tenant_schema()`), never
//     inline; and the enabled-module record is keyed
//     `"<company_id>|<module_id>"`.
//   - This module executes no SQL and never sets a search_path:
//     the DashMap stores are authoritative and the migration is
//     the persistence target, so `with_tenant_connection` /
//     `set_tenant_search_path` have nothing to wrap here. When the
//     persistence target is wired to Postgres, the integrator
//     applies the database half: the `saas_packages` tables stay
//     in `public`, while the tenant's enabled-module state moves
//     into the tenant schema (derived with `schema_name()`).
//     No tenant id, schema name, or package id is hard-coded
//     anywhere in this file.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the e-learning module. Capability checks
// use the `role.check` hook and fail closed (no answer means not
// allowed).
//
// Events emitted on the hook bus:
//   - `saas_packages.package.created`, `.updated`, `.published`,
//     `.archived`, `.deleted`, `.cloned`
//   - `saas_packages.purchase.requested` (for the store flow:
//     `{"purchase_id", "package_id", "package_version",
//     "product_id", "buyer_id", "company_id", "module":
//     "saas_packages", "item_type": "saas_package"}`)
//   - `saas_packages.purchase.completed` (provisioning finished)
//   - `saas_packages.provision.started`,
//     `saas_packages.provision.step_completed`,
//     `saas_packages.provision.completed`,
//     `saas_packages.provision.failed`
//   - `saas_packages.module.enabled`, `saas_packages.module.disabled`
//   - `saas_packages.upgraded`, `saas_packages.downgraded`,
//     `saas_packages.cancelled`, `saas_packages.renewal.requested`
//   - `site.provision_requested` (tenant subdomain request for the
//     domains flow: `{"purchase_id", "company_id",
//     "tenant_schema", "subdomain", "domain", "package_id",
//     "source": "saas_packages"}` -- the schema name is built only
//     through the tenancy helper, never inline)
//   - `preset.settings.applied` (settings handoff, in the presets
//     module's payload shape -- owning modules apply the settings)
//   - `saas_packages.wizard.prefill_requested` (wizard pre-fill for
//     the presets module: `{"purchase_id", "company_id",
//     "package_id", "steps", "settings"}`)
//   - `saas_packages.entitlement.check_requested` (hook-path
//     entitlement query, same fail-closed shape as `role.check`:
//     `{"company_id", "module_id", "allowed": false}` -- see OPEN)
//   - `notification.requested` (provisioning welcome message, in
//     the notifications module's payload shape; no credential is
//     ever carried in the payload -- credentials are issued by the
//     auth/mail flow and referenced only by a tokenized link)
//   - `module.enabled` / `module.disabled` (core events, when a
//     package module is enabled/disabled for a tenant)
//   - `billing.subscription.cancel_requested`,
//     `billing.subscription.renew_requested` (lifecycle handoffs
//     for the billing module)
// Events this module listens to (all read defensively -- a missing
// field or an unknown purchase is skipped, never an error):
//   - `order.paid`, `store.order.completed` -- a paid order whose
//     payload names a `saas_packages` purchase starts provisioning
//     for the matching pending purchase (`purchase_id`, or an
//     `items` entry carrying `purchase_id`).
//     Double-trigger guard: one payment emits several of these
//     events (`order.paid`, then `store.order.completed`, plus
//     `subscription.activated`), so the first paid trigger claims
//     the payment (purchase + order + subscription identifiers, see
//     `claim_paid_provision` in events.rs) and later deliveries of
//     the same payment are skipped instead of re-running full
//     provisioning and consuming another attempt. A genuinely new
//     payment (new order id) still provisions. Recovery from a
//     failed or blocked run is the manual HTTP retry endpoint.
//   - `subscription.activated` -- activates provisioning the same way
//     for a purchase carrying the subscription id.
//   - `store.order.refunded`, `store.order.cancelled` -- suspends
//     the matching active purchase (status back to pending payment
//     is never automatic; the purchase is marked `suspended` and
//     an admin re-provisions or refunds through the store).
//   - `subscription.cancelled` -- moves the matching purchase into
//     its grace period; `subscription.expired` -- expires it.
//   - `saas_packages.ai.recommend_package_requested`,
//     `saas_packages.ai.describe_package_requested` -- AI grounding
//     hooks answered with published-package context (never buyer
//     personal data).
//
// OPEN -- known gaps, recorded honestly instead of hidden:
//   - The AI contract (`AiCapability`, `register_ai_capabilities`,
//     `run_ai_capability` in `crate::modules::ai`) is owned by the
//     parallel AI-contract builder and did not exist in `ai.rs` at
//     the time this file was written (verified: no match for
//     `AiCapability` or `run_ai_capability` in `ai.rs`). This file
//     uses the exact names from the batch brief; it compiles only
//     once the AI-contract builder lands those items. The same
//     applies to the `Caller` type the AI helper takes: this file
//     passes its own `Caller` (actor + role, same shape as the
//     e-learning module) and the integrator must confirm the
//     helper accepts it.
//   - Hook-context provisioning (started from `order.paid`) has no
//     `AppContext`, so it cannot call the licensing module's
//     entitlement check directly. Sellable non-core modules are
//     checked through the `saas_packages.entitlement
//     .check_requested` hook query instead; when nothing answers
//     (nothing does today) the step is recorded as `blocked` with
//     a clear reason and the purchase stays in `provisioning`
//     until an admin retries through `POST
//     /purchases/{id}/provision/retry`, which runs the real
//     licensing check. Non-sellable and core modules are never
//     blocked this way. The durable fix is for the licensing
//     module to answer the query event (or for hook handlers to
//     receive an `AppContext`).
//   - The domains flow has no provision-request listener today:
//     `saas_domains` answers `tenant.resolve_requested`,
//     `saas_domains.alias.lookup`, and reservation endpoints, and
//     emits `site.provisioned` as an outcome. This module emits
//     `site.provision_requested` and records the step as
//     `completed` with outcome `requested` when no handler claims
//     it (`subdomain_pending: true` on the purchase) -- the
//     subdomain is pending until the domains flow claims the
//     request. The durable fix is a domains listener for
//     `site.provision_requested`.
//   - The presets module has no wizard pre-fill listener today:
//     it owns `presets.wizard.started` / `.step.saved` /
//     `.completed` but no `prefill_requested` event. This module
//     emits `saas_packages.wizard.prefill_requested` and records
//     the step as completed with outcome `requested`; the durable
//     fix is a presets listener that starts a wizard pre-filled
//     from the payload.
//   - The seed package names the `exams` module, whose file and
//     migration (`000090_exams`) now exist (parallel batch 14
//     builder) but which is still not registered in
//     `src/modules/mod.rs` (verified at write time), and
//     `elearning` / `academic`, whose files exist but are not
//     registered in `src/modules/mod.rs` either. The seed is data,
//     inserted directly at startup like the presets seeds;
//     provisioning reports such modules as unknown until their
//     modules register. `notifications` and `payments` are
//     registered.
//   - `saas_domains` and `presets` are event-only integrations:
//     both files exist on disk but neither is registered in
//     `src/modules/mod.rs`, so they cannot be hard `requires`
//     (the registry would report them missing). `requires` lists
//     only registered modules; the domains/presets wiring is
//     events only.
//   - Billing has no documented renew/cancel-request listener
//     found in `billing.rs` / `store.rs` at write time, so
//     `billing.subscription.renew_requested` and
//     `billing.subscription.cancel_requested` are handoffs that
//     may go unanswered until billing adds listeners.
//   - CONVERGENCE (batch 15, per-product selling + smart pricing,
//     running in parallel): this module is the BUNDLE layer -- it
//     bundles modules into sellable packages and provisions them.
//     Per-product entitlement and per-product pricing belong to
//     batch 15, which has already landed `entitlements.rs` and
//     migration `000092_entitlements`. This module still checks
//     entitlement through the licensing module
//     (`check_module_entitlement`), which was the only
//     entitlement API available at write time. The integrator must
//     confirm which entitlement source is canonical (licensing vs
//     batch 15 entitlements) and point the entitlement checks --
//     including the hook-path `saas_packages.entitlement
//     .check_requested` query -- at it. Package pricing stays a
//     product-id reference here; batch 15 owns price math,
//     discounts, and proration, and this module must never
//     duplicate that math.
//   - Grace-period expiry has no scheduler sweep: the purchase
//     moves to `cancelled` on `subscription.expired`, or an admin
//     cancels it; the GET views report `grace_expired` as a
//     computed flag so the UI can show an expired grace period.
//   - TENANT-SCOPED DATA ACCESS convergence (batch 17): the
//     tenancy plumbing LANDED while this module was being written
//     (crates/unified-core/src/tenancy.rs`: `schema_name`,
//     `TenantContext` + fail-closed extractor, `ControlPlaneGuard`,
//     `with_tenant_connection`, `set_tenant_search_path`; migration
//     `000094_tenant_isolation`: `tenants`,
//     `tenant_provisioning_steps`, `tenant_module_grants`). This
//     module uses it where it applies: `require_tenant_scope`
//     validates through `tenancy::TenantContext::from_resolved`,
//     `require_control_plane` mirrors `ControlPlaneGuard` on the
//     package-admin and settings endpoints, and `tenant_schema()`
//     builds schema names only through `tenancy::schema_name`.
//     What is still missing: the `tenants.rs` module file itself
//     (the migration references `src/modules/tenants.rs`, which
//     does not exist on disk yet), so there is no listener yet for
//     creating the tenant schema (`create_schema` /
//     `run_migrations` in the tenants pipeline) -- package
//     provisioning here provisions the subdomain and the modules,
//     but tenant schema creation belongs to the tenants module,
//     and the integrator must sequence them (tenant record +
//     schema first, then package provisioning) or have the tenants
//     module answer `site.provision_requested` / a package
//     provisioning handoff. Also still open: this module runs no
//     SQL, so `with_tenant_connection` has nothing to wrap here;
//     the search_path enforcement applies when the migration
//     persistence target is wired to Postgres.
//   - `cargo check` has never run in this environment (no Rust
//     toolchain, crates.io blocked), so compilation is NOT
//     verified; see the builder report for the verification that
//     was done instead.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::middleware::TenantContext;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::tenancy as tenancy;
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai::{register_ai_capabilities, run_ai_capability, AiCapability};
use crate::modules::licensing::check_module_entitlement;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{constants::*, types::*, state::*, seed::*, auth::*, helpers::*, provisioning::*, handlers_packages::*, handlers_purchases::*, handlers_ai::*, events::*};

pub struct SaasPackagesModule {
    pub(crate) state: SaasPackagesState,
}

impl SaasPackagesModule {
    pub fn new() -> Self {
        Self {
            state: SaasPackagesState::new(),
        }
    }
}

impl Default for SaasPackagesModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SaasPackagesModule::new()));
}

// Module implementation.

impl Module for SaasPackagesModule {
    fn id(&self) -> &'static str {
        "saas_packages"
    }

    fn name(&self) -> &'static str {
        "SaaS Packages"
    }

    fn description(&self) -> &'static str {
        "SaaS packages: sellable product bundles of modules with package versioning, purchase through the store flow, per-step provisioning (entitlement validation, tenant subdomain, module enablement in dependency order, preset settings handoff, setup wizard pre-fill, welcome message), upgrade/downgrade with a module diff, cancel with a grace period, and AI package recommendation and description. Packages are data, never code."
    }

    fn is_core(&self) -> bool {
        false
    }

    /// Hard requirements: only modules registered in
    /// `src/modules/mod.rs`. `saas_domains` and `presets` are
    /// event-only integrations (their files exist but neither is
    /// registered in `mod.rs`, so the registry would report them
    /// missing if listed here).
    fn requires(&self) -> &[&'static str] {
        &[
            "products",
            "store",
            "billing",
            "extensions",
            "licensing",
            "notifications",
            "roles",
        ]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec![
            "products",
            "store",
            "billing",
            "extensions",
            "licensing",
            "notifications",
            "roles",
        ]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("commerce")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        *self
            .state
            .modules
            .write()
            .expect("modules lock poisoned") = Some(Arc::clone(&ctx.modules));
        // Seed the example packages as data, once each. All runtime
        // behaviour reads the store from here on; the seeds are never
        // consulted again. Each seed is inserted by id, so an
        // existing install picks up a new seed without disturbing
        // the packages already stored.
        for seed in [
            seed_online_exam_package(),
            seed_site_builder_package(),
            seed_cloud_package(),
        ] {
            if !self.state.packages.contains_key(&seed.id) {
                self.state.store_version(&seed);
                self.state.packages.insert(seed.id, seed.clone());
                log::info!("SaaS Packages module seeded example package \"{}\"", seed.slug);
            }
        }
        // Warn when a seeded package names a module the registry
        // does not know, so a registration gap is visible at
        // startup instead of failing the first provisioning.
        for package in self.state.packages.iter() {
            for entry in &package.value().modules {
                if ctx.modules.get(&entry.module_id).is_none() {
                    log::warn!(
                        "SaaS package \"{}\" names unregistered module \"{}\"; provisioning will report it as unknown until the module registers",
                        package.value().slug,
                        entry.module_id
                    );
                }
            }
        }
        let report = ctx.modules.validate_dependencies();
        if !report.valid {
            for issue in &report.issues {
                log::warn!("Module dependency problem: {}", issue.message);
            }
        }
        // Register the AI capabilities this module declares. The AI
        // helper runs the AI gate for the recommendation and
        // description endpoints; nothing here calls a provider.
        register_ai_capabilities("saas_packages", AI_CAPABILITIES);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            ORDER_PAID,
            "saas_packages",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_paid(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STORE_ORDER_COMPLETED,
            "saas_packages",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_paid(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STORE_ORDER_REFUNDED,
            "saas_packages",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_refunded_or_cancelled(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STORE_ORDER_CANCELLED,
            "saas_packages",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_refunded_or_cancelled(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            SUBSCRIPTION_ACTIVATED,
            "saas_packages",
            Arc::new(move |ctx: &mut HookContext| {
                on_subscription_activated(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            SUBSCRIPTION_CANCELLED,
            "saas_packages",
            Arc::new(move |ctx: &mut HookContext| {
                on_subscription_cancelled(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            SUBSCRIPTION_EXPIRED,
            "saas_packages",
            Arc::new(move |ctx: &mut HookContext| {
                on_subscription_expired(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_AI_RECOMMEND_REQUESTED,
            "saas_packages",
            Arc::new(move |ctx: &mut HookContext| {
                on_ai_recommend_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_AI_DESCRIBE_REQUESTED,
            "saas_packages",
            Arc::new(move |ctx: &mut HookContext| {
                on_ai_describe_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/saas-packages")
                .app_data(state)
                .route("/ai/capabilities", web::get().to(list_ai_capabilities))
                .route("/ai/recommend-package", web::post().to(ai_recommend_package))
                .route("/ai/describe-package", web::post().to(ai_describe_package))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/packages", web::get().to(list_packages))
                .route("/packages", web::post().to(create_package))
                .route("/packages/{id}/versions", web::get().to(list_package_versions))
                .route("/packages/{id}/versions/{version}", web::get().to(get_package_version))
                .route("/packages/{id}/validate", web::post().to(validate_package))
                .route("/packages/{id}/modules", web::get().to(list_package_modules))
                .route("/packages/{id}/diff/{other_id}", web::get().to(diff_packages))
                .route("/packages/{id}/publish", web::post().to(publish_package))
                .route("/packages/{id}/archive", web::post().to(archive_package))
                .route("/packages/{id}/clone", web::post().to(clone_package))
                .route("/packages/{id}/purchase", web::post().to(purchase_package))
                .route("/packages/{id}", web::get().to(get_package))
                .route("/packages/{id}", web::patch().to(update_package))
                .route("/packages/{id}", web::delete().to(delete_package))
                .route("/purchases", web::get().to(list_purchases))
                .route("/purchases/{id}", web::get().to(get_purchase))
                .route("/purchases/{id}/cancel", web::post().to(cancel_purchase))
                .route("/purchases/{id}/provision", web::post().to(start_provisioning))
                .route("/purchases/{id}/provision", web::get().to(get_provisioning))
                .route("/purchases/{id}/provision/retry", web::post().to(retry_provisioning))
                .route("/purchases/{id}/provision/steps", web::get().to(list_provision_steps))
                .route("/purchases/{id}/upgrade", web::post().to(upgrade_purchase))
                .route("/purchases/{id}/downgrade", web::post().to(downgrade_purchase))
                .route("/purchases/{id}/history", web::get().to(get_purchase_history))
                .route("/purchases/{id}/renew", web::post().to(renew_purchase))
                .route("/provision-steps/{id}", web::get().to(get_provision_step))
                .route("/tenants/{company_id}/purchases", web::get().to(list_tenant_purchases)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/ai/capabilities", owner: "saas_packages", description: "List the AI capabilities this module offers (view).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/ai/recommend-package", owner: "saas_packages", description: "Ask the AI to recommend a package for a business type and needs (view; suggestions only, never purchases or provisions).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/ai/describe-package", owner: "saas_packages", description: "Ask the AI to describe one package (view).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/settings", owner: "saas_packages", description: "Get the configurable SaaS Packages settings (view).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/saas-packages/settings", owner: "saas_packages", description: "Update the configurable SaaS Packages settings (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/packages", owner: "saas_packages", description: "List packages (paginated, filter by status or vendor_id, search; non-managers see published only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/packages", owner: "saas_packages", description: "Create a package in draft (manage; module ids validated against the registry).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/packages/{id}/versions", owner: "saas_packages", description: "List version snapshots of a package (paginated, newest first).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/packages/{id}/versions/{version}", owner: "saas_packages", description: "Get one frozen version snapshot of a package.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/packages/{id}/validate", owner: "saas_packages", description: "Validate a package for a tenant: module registration, entitlement, and dependency order preview.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/packages/{id}/modules", owner: "saas_packages", description: "List the modules of a package's current version with registry info (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/packages/{id}/diff/{other_id}", owner: "saas_packages", description: "Module diff between two packages: to enable, to disable, shared.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/packages/{id}/publish", owner: "saas_packages", description: "Publish a draft package (manage; requires at least one module).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/packages/{id}/archive", owner: "saas_packages", description: "Archive a published or draft package (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/packages/{id}/clone", owner: "saas_packages", description: "Clone a package as a new draft (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/packages/{id}/purchase", owner: "saas_packages", description: "Buy a published package: creates a pending purchase and emits a purchase request for the store (checkout lives in the store).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/packages/{id}", owner: "saas_packages", description: "Get one package by id or slug (drafts and archived only for managers).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/saas-packages/packages/{id}", owner: "saas_packages", description: "Edit a package (manage; editing a published package creates a new version, purchases keep theirs).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/saas-packages/packages/{id}", owner: "saas_packages", description: "Delete a package with no purchases (manage; published packages must be archived first).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/purchases", owner: "saas_packages", description: "List purchases (paginated, filter by package_id, company_id, buyer_id, status; non-managers see only their own).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/purchases/{id}", owner: "saas_packages", description: "Get one purchase with its computed grace-expired flag (own or manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/purchases/{id}/cancel", owner: "saas_packages", description: "Cancel a purchase with a grace period (manage; hands the subscription cancellation to billing).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/purchases/{id}/provision", owner: "saas_packages", description: "Start provisioning a purchase (provision; full run of the six-step pipeline; only a purchase that has already been paid -- provisioning or failed -- since unpaid purchases provision on order.paid).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/purchases/{id}/provision", owner: "saas_packages", description: "Get the provisioning status and steps of a purchase (own or manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/purchases/{id}/provision/retry", owner: "saas_packages", description: "Retry the failed, blocked, or skipped provisioning steps of a purchase (provision).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/purchases/{id}/provision/steps", owner: "saas_packages", description: "List the provisioning steps of a purchase (paginated, filter by status or step).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/purchases/{id}/upgrade", owner: "saas_packages", description: "Move a purchase to another published package (manage; enables new modules, never disables shared modules without explicit confirmation).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/purchases/{id}/downgrade", owner: "saas_packages", description: "Move a purchase to another published package (manage; same diff rules as upgrade).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/purchases/{id}/history", owner: "saas_packages", description: "List the history of a purchase (paginated, filter by action).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/purchases/{id}/renew", owner: "saas_packages", description: "Request renewal of a purchase through billing (purchase; hands the renewal to billing).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/provision-steps/{id}", owner: "saas_packages", description: "Get one provisioning step (own purchase or manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/tenants/{company_id}/purchases", owner: "saas_packages", description: "List the purchases of one tenant (paginated, filter by status; non-managers see only their own).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}
