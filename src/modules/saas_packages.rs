// SaaS Packages module — sellable product bundles of modules.
//
// A package is DATA, never code: an admin or vendor defines a named
// bundle (module list, preset settings payload, wizard steps, and a
// link to a product/plan in the products module), publishes it, and
// customers buy it through the store / billing / subscription flow.
// Buying never provisions anything by itself: on `order.paid` (or a
// subscription activation) this module runs PROVISIONING for the
// purchase — validate entitlements, provision a tenant subdomain,
// enable the package modules in dependency order, hand the preset
// settings off to the owning modules, pre-fill the setup wizard,
// and send the welcome message. Every step is recorded with its own
// outcome, failed steps can be retried, and the full history is kept.
//
// Routes live under `/api/v1/saas-packages`:
//
//   Packages:
//     GET    /packages — list (paginated, filter by status, search).
//     POST   /packages — create a package in draft (manage).
//     GET    /packages/{id} — one package (id or slug).
//     PATCH  /packages/{id} — edit; editing a published package
//            creates a new version, existing purchases keep theirs.
//     DELETE /packages/{id} — delete a package with no purchases.
//     POST   /packages/{id}/publish — draft -> published.
//     POST   /packages/{id}/archive — published/draft -> archived.
//     POST   /packages/{id}/clone — clone as a new draft package.
//     GET    /packages/{id}/versions — version snapshots.
//     GET    /packages/{id}/versions/{version} — one snapshot.
//     POST   /packages/{id}/validate — module/entitlement preview.
//     GET    /packages/{id}/modules — current module list.
//     GET    /packages/{id}/diff/{other_id} — module diff of two packages.
//   Purchases:
//     POST   /packages/{id}/purchase — buy (emits a purchase request
//            for the store; checkout itself lives in the store).
//     GET    /purchases — list purchases (paginated, filter by
//            package_id, company_id, buyer_id, status).
//     GET    /purchases/{id} — one purchase.
//     POST   /purchases/{id}/cancel — cancel, with a grace period.
//     POST   /purchases/{id}/provision — start provisioning.
//     POST   /purchases/{id}/provision/retry — retry failed/blocked steps.
//     GET    /purchases/{id}/provision — provisioning status + steps.
//     GET    /purchases/{id}/provision/steps — paginated step list.
//     GET    /provision-steps/{id} — one provisioning step.
//     POST   /purchases/{id}/upgrade — move to another package.
//     POST   /purchases/{id}/downgrade — move to another package.
//     GET    /purchases/{id}/history — purchase history.
//     GET    /tenants/{company_id}/purchases — purchases of one tenant.
//     POST   /purchases/{id}/renew — request renewal via billing.
//   AI (smart module):
//     POST   /ai/recommend-package — suggest a package (AI gated).
//     POST   /ai/describe-package — describe a package (AI gated).
//     GET    /ai/capabilities — the AI capabilities this module offers.
//   Settings:
//     GET    /settings — configurable defaults (view).
//     PUT    /settings — update the defaults (manage).
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory package / version / purchase / provisioning-step /
// history management; package versioning (published edits create a
// new version, purchases pin theirs); purchase requests handed to
// the store through events (checkout, payment capture, and
// subscription billing live in the store/billing modules); per-step
// provisioning with per-step outcomes, retry, and a full audit
// trail; entitlement blocking (a package module the tenant is not
// entitled to BLOCKS provisioning with a 403-style error naming
// the module — it is never silently skipped); upgrade/downgrade
// with a module diff that never disables a module the tenant uses
// outside the package without explicit confirmation; cancel with
// a grace period tied to the subscription lifecycle events. What
// is NOT built here: durable storage (the DashMap stores are
// authoritative; migration `2026-11-22-000096_saas_packages` is
// the persistence target); checkout or payment capture (the store
// owns it — this module only requests a purchase and reacts to
// `order.paid`); the subdomain itself (the domains flow owns
// allocation — this module only emits the provision request and
// records the handoff); the setup wizard itself (the presets
// module owns it — this module only emits the pre-fill request);
// settings application (owning modules apply the settings this
// module hands off through events); email/push delivery (the
// notifications module owns delivery — this module only emits
// `notification.requested`); and the AI model call itself (the AI
// module owns the gate and the provider call — this module only
// declares capabilities and delegates through
// `run_ai_capability`).
//
// CONFIG-DRIVEN — READ THIS FIRST. Nothing behavioural is
// hard-coded: packages, their module lists, settings payloads,
// wizard steps, prices (held only as a product id reference —
// money never lives here), the grace period, the module cap, and
// the wizard step vocabulary all live in data or in
// `SaasPackagesSettings`, editable by an administrator through
// `PUT /settings`. The only hard-coded vocabulary is disclosed in
// the OPEN notes at the end of this header: the closed package
// statuses, the closed purchase/provisioning statuses, the six
// provisioning pipeline step names, the four capability names,
// and the seed example package (which is DATA — the code works
// with zero packages defined, and an admin can edit or delete
// the seed like any other package).
//
// TENANT SCOPING — READ THIS FIRST (standing rule). Package
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
// 17 tenancy plumbing in `src/core/tenancy.rs` (which LANDED
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
//     matches the resolved tenant — validated through
//     `tenancy::TenantContext::from_resolved`, so a tenant id that
//     is not a uuidv7 UUID is rejected outright — and a mismatch
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
//     "source": "saas_packages"}` — the schema name is built only
//     through the tenancy helper, never inline)
//   - `preset.settings.applied` (settings handoff, in the presets
//     module's payload shape — owning modules apply the settings)
//   - `saas_packages.wizard.prefill_requested` (wizard pre-fill for
//     the presets module: `{"purchase_id", "company_id",
//     "package_id", "steps", "settings"}`)
//   - `saas_packages.entitlement.check_requested` (hook-path
//     entitlement query, same fail-closed shape as `role.check`:
//     `{"company_id", "module_id", "allowed": false}` — see OPEN)
//   - `notification.requested` (provisioning welcome message, in
//     the notifications module's payload shape; no credential is
//     ever carried in the payload — credentials are issued by the
//     auth/mail flow and referenced only by a tokenized link)
//   - `module.enabled` / `module.disabled` (core events, when a
//     package module is enabled/disabled for a tenant)
//   - `billing.subscription.cancel_requested`,
//     `billing.subscription.renew_requested` (lifecycle handoffs
//     for the billing module)
// Events this module listens to (all read defensively — a missing
// field or an unknown purchase is skipped, never an error):
//   - `order.paid`, `store.order.completed` — a paid order whose
//     payload names a `saas_packages` purchase starts provisioning
//     for the matching pending purchase (`purchase_id`, or an
//     `items` entry carrying `purchase_id`).
//   - `subscription.activated` — activates provisioning the same way
//     for a purchase carrying the subscription id.
//   - `store.order.refunded`, `store.order.cancelled` — suspends
//     the matching active purchase (status back to pending payment
//     is never automatic; the purchase is marked `suspended` and
//     an admin re-provisions or refunds through the store).
//   - `subscription.cancelled` — moves the matching purchase into
//     its grace period; `subscription.expired` — expires it.
//   - `saas_packages.ai.recommend_package_requested`,
//     `saas_packages.ai.describe_package_requested` — AI grounding
//     hooks answered with published-package context (never buyer
//     personal data).
//
// OPEN — known gaps, recorded honestly instead of hidden:
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
//     it (`subdomain_pending: true` on the purchase) — the
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
//     running in parallel): this module is the BUNDLE layer — it
//     bundles modules into sellable packages and provisions them.
//     Per-product entitlement and per-product pricing belong to
//     batch 15, which has already landed `entitlements.rs` and
//     migration `000092_entitlements`. This module still checks
//     entitlement through the licensing module
//     (`check_module_entitlement`), which was the only
//     entitlement API available at write time. The integrator must
//     confirm which entitlement source is canonical (licensing vs
//     batch 15 entitlements) and point the entitlement checks —
//     including the hook-path `saas_packages.entitlement
//     .check_requested` query — at it. Package pricing stays a
//     product-id reference here; batch 15 owns price math,
//     discounts, and proration, and this module must never
//     duplicate that math.
//   - Grace-period expiry has no scheduler sweep: the purchase
//     moves to `cancelled` on `subscription.expired`, or an admin
//     cancels it; the GET views report `grace_expired` as a
//     computed flag so the UI can show an expired grace period.
//   - TENANT-SCOPED DATA ACCESS convergence (batch 17): the
//     tenancy plumbing LANDED while this module was being written
//     (`src/core/tenancy.rs`: `schema_name`,
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
//     `run_migrations` in the tenants pipeline) — package
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

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::middleware::TenantContext;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::tenancy as tenancy;
use crate::core::pagination::{PageResult, PaginationParams};
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

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_SLUG_LEN: usize = 100;
const MAX_REF_LEN: usize = 500;
const MAX_TEXT_LEN: usize = 20000;
const MAX_SUBDOMAIN_LEN: usize = 63;

/// Capability names checked through the `role.check` hook.
pub const PERM_SAAS_PACKAGES_VIEW: &str = "saas_packages.view";
pub const PERM_SAAS_PACKAGES_MANAGE: &str = "saas_packages.manage";
pub const PERM_SAAS_PACKAGES_PURCHASE: &str = "saas_packages.purchase";
pub const PERM_SAAS_PACKAGES_PROVISION: &str = "saas_packages.provision";

/// Event names this module emits.
pub const EVENT_PACKAGE_CREATED: &str = "saas_packages.package.created";
pub const EVENT_PACKAGE_UPDATED: &str = "saas_packages.package.updated";
pub const EVENT_PACKAGE_PUBLISHED: &str = "saas_packages.package.published";
pub const EVENT_PACKAGE_ARCHIVED: &str = "saas_packages.package.archived";
pub const EVENT_PACKAGE_DELETED: &str = "saas_packages.package.deleted";
pub const EVENT_PACKAGE_CLONED: &str = "saas_packages.package.cloned";
pub const EVENT_PURCHASE_REQUESTED: &str = "saas_packages.purchase.requested";
pub const EVENT_PURCHASE_COMPLETED: &str = "saas_packages.purchase.completed";
pub const EVENT_PROVISION_STARTED: &str = "saas_packages.provision.started";
pub const EVENT_PROVISION_STEP_COMPLETED: &str = "saas_packages.provision.step_completed";
pub const EVENT_PROVISION_COMPLETED: &str = "saas_packages.provision.completed";
pub const EVENT_PROVISION_FAILED: &str = "saas_packages.provision.failed";
pub const EVENT_MODULE_ENABLED: &str = "saas_packages.module.enabled";
pub const EVENT_MODULE_DISABLED: &str = "saas_packages.module.disabled";
pub const EVENT_UPGRADED: &str = "saas_packages.upgraded";
pub const EVENT_DOWNGRADED: &str = "saas_packages.downgraded";
pub const EVENT_CANCELLED: &str = "saas_packages.cancelled";
pub const EVENT_RENEWAL_REQUESTED: &str = "saas_packages.renewal.requested";
pub const EVENT_SITE_PROVISION_REQUESTED: &str = "site.provision_requested";
pub const EVENT_SETTINGS_APPLIED: &str = "preset.settings.applied";
pub const EVENT_WIZARD_PREFILL_REQUESTED: &str = "saas_packages.wizard.prefill_requested";
pub const EVENT_ENTITLEMENT_CHECK_REQUESTED: &str = "saas_packages.entitlement.check_requested";
pub const EVENT_AI_RECOMMEND_REQUESTED: &str = "saas_packages.ai.recommend_package_requested";
pub const EVENT_AI_DESCRIBE_REQUESTED: &str = "saas_packages.ai.describe_package_requested";

/// Event names this module listens to.
const ORDER_PAID: &str = "order.paid";
const STORE_ORDER_COMPLETED: &str = "store.order.completed";
const STORE_ORDER_REFUNDED: &str = "store.order.refunded";
const STORE_ORDER_CANCELLED: &str = "store.order.cancelled";
const SUBSCRIPTION_ACTIVATED: &str = "subscription.activated";
const SUBSCRIPTION_CANCELLED: &str = "subscription.cancelled";
const SUBSCRIPTION_EXPIRED: &str = "subscription.expired";

/// Package lifecycle statuses. Closed vocabulary: a package is a
/// draft until published, and archived packages are never sold.
const STATUS_DRAFT: &str = "draft";
const STATUS_PUBLISHED: &str = "published";
const STATUS_ARCHIVED: &str = "archived";

/// Purchase lifecycle statuses.
const PURCHASE_PENDING_PAYMENT: &str = "pending_payment";
const PURCHASE_PROVISIONING: &str = "provisioning";
const PURCHASE_ACTIVE: &str = "active";
const PURCHASE_FAILED: &str = "failed";
const PURCHASE_SUSPENDED: &str = "suspended";
const PURCHASE_GRACE_PERIOD: &str = "grace_period";
const PURCHASE_CANCELLED: &str = "cancelled";
const PURCHASE_EXPIRED: &str = "expired";

/// Provisioning pipeline step names, in execution order. These are
/// the fixed pipeline of this module (a standard constant, not a
/// business list): every provisioning run executes these six steps
/// in this order, each recorded with its own outcome.
const STEP_VALIDATE_ENTITLEMENTS: &str = "validate_entitlements";
const STEP_PROVISION_SUBDOMAIN: &str = "provision_subdomain";
const STEP_ENABLE_MODULES: &str = "enable_modules";
const STEP_APPLY_PRESET_SETTINGS: &str = "apply_preset_settings";
const STEP_RUN_SETUP_WIZARD: &str = "run_setup_wizard";
const STEP_SEND_WELCOME: &str = "send_welcome";
const PROVISION_STEPS: &[&str] = &[
    STEP_VALIDATE_ENTITLEMENTS,
    STEP_PROVISION_SUBDOMAIN,
    STEP_ENABLE_MODULES,
    STEP_APPLY_PRESET_SETTINGS,
    STEP_RUN_SETUP_WIZARD,
    STEP_SEND_WELCOME,
];

/// Provisioning step statuses.
const STEP_PENDING: &str = "pending";
const STEP_RUNNING: &str = "running";
const STEP_COMPLETED: &str = "completed";
const STEP_FAILED: &str = "failed";
const STEP_SKIPPED: &str = "skipped";
const STEP_BLOCKED: &str = "blocked";

/// Per-module enable outcomes, mirroring the presets module so the
/// two modules report the same words for the same situations.
const OUTCOME_ENABLED: &str = "enabled";
const OUTCOME_ALREADY_ENABLED: &str = "already_enabled";
const OUTCOME_SKIPPED_NOT_ENTITLED: &str = "skipped_not_entitled";
const OUTCOME_SKIPPED_DEPENDENCY_MISSING: &str = "skipped_dependency_missing";
const OUTCOME_FAILED: &str = "failed";
/// The step finished as a handoff: the request was emitted and no
/// owning module has claimed it yet (see the OPEN notes).
const OUTCOME_REQUESTED: &str = "requested";

// ---------------------------------------------------------------------------
// AI capabilities (smart module).
// ---------------------------------------------------------------------------

/// The AI capabilities this module declares. Recommendations are
/// suggestions only: they never purchase or provision anything.
pub const AI_CAPABILITIES: &[AiCapability] = &[
    AiCapability {
        id: "recommend_package",
        module_id: "saas_packages",
        name: "Recommend package",
        description: "Suggest the best fitting SaaS package for a business type and a set of needs, from the published packages.",
        input_fields: &["business_type", "needs"],
        allows_pii: false,
        use_case: "saas_packages.recommend_package",
    },
    AiCapability {
        id: "describe_package",
        module_id: "saas_packages",
        name: "Describe package",
        description: "Describe one SaaS package: its modules, settings summary, and what a buyer gets.",
        input_fields: &["package_id"],
        allows_pii: false,
        use_case: "saas_packages.describe_package",
    },
];

// ---------------------------------------------------------------------------
// Configurable settings (admin-editable; defaults are defaults only)
// ---------------------------------------------------------------------------

/// All behaviour-shaping defaults of this module. An administrator
/// edits these through `PUT /settings`; every validation and
/// default in this file reads the live value, so changing a value
/// here changes what the module accepts without any code change.
/// The values below are CONFIGURABLE DEFAULTS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaasPackagesSettings {
    /// Grace period in days after a cancellation before the
    /// purchase is fully cancelled.
    pub grace_period_days: i64,
    /// Maximum modules one package may list.
    pub max_modules_per_package: usize,
    /// Maximum wizard steps one package may store.
    pub max_wizard_steps: usize,
    /// Maximum provisioning attempts per purchase before an admin
    /// must intervene (retries beyond this are rejected).
    pub provisioning_max_attempts: i32,
    /// Base domain appended to a requested tenant subdomain
    /// (`"<subdomain>.<base_domain>"`); empty means the subdomain
    /// is used exactly as requested.
    pub subdomain_base_domain: String,
    /// Whether vendors (non-admin sellers) may create packages.
    pub vendor_packages_enabled: bool,
    /// Wizard step names a package may use. Mirrors the presets
    /// module's step vocabulary; the list is data, so an admin can
    /// extend it when the wizard gains a step.
    pub wizard_step_names: Vec<String>,
}

impl Default for SaasPackagesSettings {
    fn default() -> Self {
        Self {
            grace_period_days: 14,
            max_modules_per_package: 50,
            max_wizard_steps: 10,
            provisioning_max_attempts: 3,
            subdomain_base_domain: String::new(),
            vendor_packages_enabled: true,
            wizard_step_names: vec![
                "business_type".to_string(),
                "review_modules".to_string(),
                "apply".to_string(),
                "settings".to_string(),
                "done".to_string(),
            ],
        }
    }
}

impl SaasPackagesSettings {
    fn validate(&self) -> UnifiedResult<()> {
        if self.grace_period_days < 0 || self.grace_period_days > 365 {
            return Err(UnifiedError::bad_request(
                "grace_period_days must be between 0 and 365",
            ));
        }
        if self.max_modules_per_package == 0 || self.max_modules_per_package > 500 {
            return Err(UnifiedError::bad_request(
                "max_modules_per_package must be between 1 and 500",
            ));
        }
        if self.max_wizard_steps == 0 || self.max_wizard_steps > 20 {
            return Err(UnifiedError::bad_request(
                "max_wizard_steps must be between 1 and 20",
            ));
        }
        if self.provisioning_max_attempts < 1 || self.provisioning_max_attempts > 10 {
            return Err(UnifiedError::bad_request(
                "provisioning_max_attempts must be between 1 and 10",
            ));
        }
        if self.subdomain_base_domain.len() > MAX_REF_LEN {
            return Err(UnifiedError::bad_request(
                "subdomain_base_domain must be at most 500 characters",
            ));
        }
        if self.wizard_step_names.is_empty() {
            return Err(UnifiedError::bad_request(
                "wizard_step_names must contain at least one step",
            ));
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for step in &self.wizard_step_names {
            let step = step.trim();
            if step.is_empty() || step.len() > MAX_SLUG_LEN {
                return Err(UnifiedError::bad_request(
                    "wizard step names must be non-empty and at most 100 characters",
                ));
            }
            if !seen.insert(step) {
                return Err(UnifiedError::bad_request(format!(
                    "Duplicate wizard step \"{step}\" in wizard_step_names"
                )));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Stored records.
// ---------------------------------------------------------------------------

/// One module inside a package, with its required flag and order.
/// Mirrors the presets module entry shape so the two modules agree
/// on what a bundled module list looks like.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageModuleEntry {
    pub module_id: String,
    pub required: bool,
    pub sort_order: i32,
}

/// A sellable package definition. Packages are DATA: the module
/// list, settings payload, and wizard steps live here, never in
/// code. Money never lives here — `product_id` is only a reference
/// to a product/plan in the products module, which owns pricing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaasPackage {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: String,
    /// Draft, published, or archived (closed vocabulary).
    pub status: String,
    /// Current version number. Editing a published package bumps
    /// this and snapshots the old version; purchases pin the
    /// version they bought.
    pub current_version: i32,
    /// Reference to a product/plan in the products module (price
    /// and billing live there). None means no product linked yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    /// Settings payload applied at provisioning. Handed off through
    /// events, never applied here.
    pub preset_settings: Value,
    /// Setup wizard step sequence (names from the settings
    /// vocabulary), pre-filled at provisioning.
    pub wizard_steps: Vec<String>,
    /// Modules bundled by the current version, in order.
    pub modules: Vec<PackageModuleEntry>,
    /// Id of the admin/vendor who created the package.
    pub created_by: String,
    /// Optional vendor (seller) id; None means a platform package.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One frozen version of a package: the snapshot a purchase pins.
/// Older versions are never edited; a new version is a new row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageVersion {
    pub package_id: Uuid,
    pub version: i32,
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    pub preset_settings: Value,
    pub wizard_steps: Vec<String>,
    pub modules: Vec<PackageModuleEntry>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

/// One purchase of a package by a buyer for a tenant (company).
/// The purchase pins the package version it bought, so later
/// package edits never change what the buyer already owns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackagePurchase {
    pub id: Uuid,
    pub package_id: Uuid,
    pub package_version: i32,
    pub buyer_id: String,
    pub company_id: Uuid,
    /// pending_payment, provisioning, active, failed, suspended,
    /// grace_period, cancelled, expired.
    pub status: String,
    /// Requested tenant subdomain (bare label, without base domain).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdomain: Option<String>,
    /// True while the subdomain request has been emitted but no
    /// domains handler has claimed it yet.
    pub subdomain_pending: bool,
    /// Store order that paid for this purchase, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// Billing subscription backing this purchase, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<String>,
    /// Product/plan reference copied from the package at purchase
    /// time, so a later package edit cannot rewrite the sale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    /// Modules this purchase enabled for the tenant.
    pub enabled_modules: Vec<String>,
    /// End of the cancellation grace period, if cancelling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grace_ends_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_reason: Option<String>,
    /// How many provisioning attempts have run (cap from settings).
    pub provision_attempts: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One step of one provisioning run. Every step of every attempt is
/// kept, so the full provisioning history is auditable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionStep {
    pub id: Uuid,
    pub purchase_id: Uuid,
    pub step: String,
    /// pending, running, completed, failed, skipped, blocked.
    pub status: String,
    /// Outcome word for a finished step (enabled, requested,
    /// already_enabled, skipped_not_entitled, failed, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub attempt: i32,
    pub started_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

/// One history entry for a purchase: every lifecycle change is
/// recorded with its actor, so the purchase story is complete.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurchaseHistoryEntry {
    pub id: Uuid,
    pub purchase_id: Uuid,
    pub action: String,
    pub actor_id: String,
    pub detail: Value,
    pub created_at: DateTime<Utc>,
}

/// The outcome of one module inside a provisioning run, mirroring
/// the presets module's per-module outcome shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleProvisionOutcome {
    pub module_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub auto_activated: Vec<String>,
}

/// The provisioning summary returned by the provision endpoints
/// and stored on the purchase view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionSummary {
    pub purchase_id: Uuid,
    pub package_id: Uuid,
    pub package_version: i32,
    pub company_id: Uuid,
    /// Tenant schema name for the purchase's company, built only
    /// through `tenancy::schema_name` (never inline, never stored
    /// from user input).
    pub tenant_schema: String,
    pub status: String,
    pub attempt: i32,
    pub steps: Vec<ProvisionStep>,
    pub module_outcomes: Vec<ModuleProvisionOutcome>,
    pub subdomain_pending: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// Module diff between two packages, for upgrade/downgrade preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageDiff {
    pub from_package_id: Uuid,
    pub to_package_id: Uuid,
    pub to_enable: Vec<String>,
    pub to_disable: Vec<String>,
    pub shared: Vec<String>,
    /// Modules that would be disabled but are used outside this
    /// purchase (shared with another purchase), so they are kept.
    pub kept_shared: Vec<String>,
}

// ---------------------------------------------------------------------------
// Request bodies.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PackageModuleInput {
    module_id: String,
    #[serde(default = "default_required")]
    required: bool,
    #[serde(default)]
    sort_order: Option<i32>,
}

fn default_required() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct CreatePackageRequest {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    product_id: Option<String>,
    #[serde(default)]
    preset_settings: Option<Value>,
    #[serde(default)]
    wizard_steps: Option<Vec<String>>,
    modules: Vec<PackageModuleInput>,
    #[serde(default)]
    vendor_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdatePackageRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    product_id: Option<String>,
    #[serde(default)]
    preset_settings: Option<Value>,
    #[serde(default)]
    wizard_steps: Option<Vec<String>>,
    #[serde(default)]
    modules: Option<Vec<PackageModuleInput>>,
}

#[derive(Debug, Deserialize)]
struct PurchaseRequest {
    #[serde(default)]
    buyer_id: Option<String>,
    company_id: String,
    #[serde(default)]
    subdomain: Option<String>,
    #[serde(default)]
    order_id: Option<String>,
    #[serde(default)]
    subscription_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CancelPurchaseRequest {
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    actor_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChangePackageRequest {
    target_package_id: String,
    #[serde(default)]
    confirm_disable: bool,
    #[serde(default)]
    modules_to_disable: Vec<String>,
    #[serde(default)]
    actor_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ValidatePackageRequest {
    #[serde(default)]
    company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AiRecommendRequest {
    #[serde(default)]
    business_type: Option<String>,
    #[serde(default)]
    needs: Option<Vec<String>>,
    #[serde(default)]
    company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AiDescribeRequest {
    package_id: String,
    #[serde(default)]
    company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SettingsUpdateRequest {
    #[serde(default)]
    grace_period_days: Option<i64>,
    #[serde(default)]
    max_modules_per_package: Option<usize>,
    #[serde(default)]
    max_wizard_steps: Option<usize>,
    #[serde(default)]
    provisioning_max_attempts: Option<i32>,
    #[serde(default)]
    subdomain_base_domain: Option<String>,
    #[serde(default)]
    vendor_packages_enabled: Option<bool>,
    #[serde(default)]
    wizard_step_names: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// In-memory stores (authoritative at runtime; see the module header).
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct SaasPackagesState {
    pub packages: Arc<DashMap<Uuid, SaasPackage>>,
    /// Version snapshots, keyed `"<package_id>|<version>"`.
    pub versions: Arc<DashMap<String, PackageVersion>>,
    pub purchases: Arc<DashMap<Uuid, PackagePurchase>>,
    pub steps: Arc<DashMap<Uuid, ProvisionStep>>,
    pub history: Arc<DashMap<Uuid, PurchaseHistoryEntry>>,
    /// Modules a purchase enabled, keyed `"<company_id>|<module_id>"`
    /// to the purchase ids that enabled them. This is this module's
    /// own record of what it enabled; modules enabled through the
    /// Extensions Manager or a preset are not visible here.
    pub tenant_modules: Arc<DashMap<String, Vec<Uuid>>>,
    pub settings: Arc<RwLock<SaasPackagesSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
    pub modules: Arc<RwLock<Option<Arc<ModuleRegistry>>>>,
}

impl Default for SaasPackagesState {
    fn default() -> Self {
        Self {
            packages: Arc::new(DashMap::new()),
            versions: Arc::new(DashMap::new()),
            purchases: Arc::new(DashMap::new()),
            steps: Arc::new(DashMap::new()),
            history: Arc::new(DashMap::new()),
            tenant_modules: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(SaasPackagesSettings::default())),
            hooks: Arc::new(RwLock::new(None)),
            modules: Arc::new(RwLock::new(None)),
        }
    }
}

impl SaasPackagesState {
    pub fn new() -> Self {
        Self::default()
    }

    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn registry(&self) -> Option<Arc<ModuleRegistry>> {
        self.modules.read().expect("modules lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit and return the handler context, so a caller can read
    /// back answers from hook queries (fail-closed shape: no answer
    /// means the queried permission or claim was not granted).
    fn emit_with_results(&self, event: &str, data: Value) -> Option<HookContext> {
        self.bus().map(|bus| bus.emit(event, data))
    }

    fn settings(&self) -> SaasPackagesSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    fn tenant_module_key(company_id: &Uuid, module_id: &str) -> String {
        format!("{company_id}|{module_id}")
    }

    fn tenant_has_module(&self, company_id: &Uuid, module_id: &str) -> bool {
        self.tenant_modules
            .get(&Self::tenant_module_key(company_id, module_id))
            .map(|entry| !entry.value().is_empty())
            .unwrap_or(false)
    }

    fn mark_tenant_module(&self, company_id: &Uuid, module_id: &str, purchase_id: Uuid) {
        let key = Self::tenant_module_key(company_id, module_id);
        self.tenant_modules
            .entry(key)
            .or_insert_with(Vec::new)
            .push(purchase_id);
    }

    fn unmark_tenant_module(&self, company_id: &Uuid, module_id: &str, purchase_id: Uuid) {
        let key = Self::tenant_module_key(company_id, module_id);
        if let Some(mut entry) = self.tenant_modules.get_mut(&key) {
            entry.retain(|id| *id != purchase_id);
        }
    }

    /// Other purchases (not `except`) that also enabled this module
    /// for the tenant. A module with other owners is shared and is
    /// never disabled by an upgrade/downgrade of one purchase.
    fn other_module_owners(&self, company_id: &Uuid, module_id: &str, except: Uuid) -> Vec<Uuid> {
        self.tenant_modules
            .get(&Self::tenant_module_key(company_id, module_id))
            .map(|entry| {
                entry
                    .value()
                    .iter()
                    .copied()
                    .filter(|id| *id != except)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn record_history(&self, purchase_id: Uuid, action: &str, actor_id: &str, detail: Value) {
        self.history.insert(
            Uuid::now_v7(),
            PurchaseHistoryEntry {
                id: Uuid::now_v7(),
                purchase_id,
                action: action.to_string(),
                actor_id: actor_id.to_string(),
                detail,
                created_at: Utc::now(),
            },
        );
    }

    fn snapshot_version(package: &SaasPackage) -> PackageVersion {
        PackageVersion {
            package_id: package.id,
            version: package.current_version,
            name: package.name.clone(),
            description: package.description.clone(),
            product_id: package.product_id.clone(),
            preset_settings: package.preset_settings.clone(),
            wizard_steps: package.wizard_steps.clone(),
            modules: package.modules.clone(),
            created_by: package.created_by.clone(),
            created_at: package.updated_at,
        }
    }

    fn version_key(package_id: &Uuid, version: i32) -> String {
        format!("{package_id}|{version}")
    }

    fn store_version(&self, package: &SaasPackage) {
        let snapshot = Self::snapshot_version(package);
        self.versions
            .insert(Self::version_key(&snapshot.package_id, snapshot.version), snapshot);
    }

    /// Published packages, newest first, for AI grounding and the
    /// public list. No buyer data is ever included here.
    fn published_packages(&self) -> Vec<SaasPackage> {
        let mut items: Vec<SaasPackage> = self
            .packages
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|package| package.status == STATUS_PUBLISHED)
            .collect();
        items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        items
    }
}

pub struct SaasPackagesModule {
    state: SaasPackagesState,
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

// ---------------------------------------------------------------------------
// SEED DATA — the example package as initial rows.
//
// This table is read exactly once, at startup (`init`), to insert
// rows into the package store, and is never read again: every other
// line of this module reads packages from the store only. The same
// row, with the same id, is the `INSERT` statement in migration
// `000096_saas_packages`, so the database and the in-memory store
// agree. The seed is EXAMPLE DATA ONLY: an admin can edit or delete
// it like any other package, and the code works with zero packages
// defined. No package is ever hard-coded anywhere else in this file.
//
// The bundled modules are the ones the brief names for the headline
// example: exams, e-learning, academic, notifications, payments.
// At write time `exams` has a file and migration (`000090_exams`,
// parallel batch 14 builder) but is not registered in
// `src/modules/mod.rs`; `elearning` / `academic` exist as files but
// are not registered in `src/modules/mod.rs` either; `notifications` and
// `payments` are registered. `init` logs a warning per unregistered
// seed module instead of failing startup, and provisioning reports
// such modules as unknown until their modules register.
// ---------------------------------------------------------------------------

/// Fixed id for the seeded example package, shared with the
/// migration `INSERT`s so the database row and the in-memory row
/// match.
pub const SEED_ONLINE_EXAM_PACKAGE_ID: &str = "20000000-0000-4000-8000-000000000001";

fn seed_online_exam_package() -> SaasPackage {
    let now = Utc::now();
    SaasPackage {
        id: Uuid::parse_str(SEED_ONLINE_EXAM_PACKAGE_ID)
            .expect("seed package id is a valid UUID"),
        slug: "online-exam-saas".to_string(),
        name: "Online Exam SaaS".to_string(),
        description: "Everything a school or training provider needs to run online exams: exams, e-learning courses, academic records, notifications, and payments, provisioned on its own tenant subdomain with a guided setup wizard.".to_string(),
        status: STATUS_PUBLISHED.to_string(),
        current_version: 1,
        product_id: None,
        preset_settings: json!({
            "welcome_email_enabled": true,
            "default_language": "en"
        }),
        wizard_steps: vec![
            "business_type".to_string(),
            "review_modules".to_string(),
            "apply".to_string(),
            "settings".to_string(),
            "done".to_string(),
        ],
        modules: vec![
            PackageModuleEntry { module_id: "exams".to_string(), required: true, sort_order: 0 },
            PackageModuleEntry { module_id: "elearning".to_string(), required: true, sort_order: 1 },
            PackageModuleEntry { module_id: "academic".to_string(), required: true, sort_order: 2 },
            PackageModuleEntry { module_id: "notifications".to_string(), required: true, sort_order: 3 },
            PackageModuleEntry { module_id: "payments".to_string(), required: true, sort_order: 4 },
        ],
        created_by: "system".to_string(),
        vendor_id: None,
        created_at: now,
        updated_at: now,
    }
}

// ---------------------------------------------------------------------------
// Viewer identity and capability checks (same trusted-parameter
// pattern as the e-learning module: X-User-Id plus X-Role headers,
// `role.check` hook, fail closed).
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the e-learning module.
pub struct Caller {
    pub actor: String,
    pub role: String,
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
/// the `role.check` hook. Fails closed: no answer means not allowed.
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

/// Tenant scoping (standing rule). When the tenant-resolution
/// middleware attached a `TenantContext` to the request, the
/// requested company must match the resolved tenant — a mismatch
/// is a 403, fail closed. The resolved context is validated
/// through the tenancy contract (`tenancy::TenantContext
/// ::from_resolved`): a tenant id that is not a uuidv7 UUID is
/// rejected outright, and the tenant schema name is always derived
/// through `tenancy::schema_name`, never built inline. With no
/// context resolved (a control-plane request, for example a buyer
/// on the main host or an admin), the explicit company scoping in
/// each handler governs instead. That is not a silent fallback to
/// the public schema: this module executes no SQL at all, never
/// sets a search_path, and never reads a record without its
/// company id. The database-level enforcement (search_path via
/// `with_tenant_connection`) has nothing to wrap here because
/// there is no SQL in this file; it applies when the migration
/// persistence target is wired to Postgres (see the module header
/// OPEN notes).
fn require_tenant_scope(req: &HttpRequest, company_id: &Uuid) -> UnifiedResult<()> {
    let Some(resolved) = TenantContext::from_request(req) else {
        return Ok(());
    };
    let tenant = tenancy::TenantContext::from_resolved(&resolved)?;
    if tenant.tenant_id != *company_id {
        return Err(UnifiedError::forbidden(
            "Request tenant context does not match the requested company: tenant-scoped data is never served across tenants",
        ));
    }
    if let Some(company) = &tenant.company_id {
        if company != &company_id.to_string() {
            return Err(UnifiedError::forbidden(
                "Request tenant context does not match the requested company: tenant-scoped data is never served across tenants",
            ));
        }
    }
    Ok(())
}

/// Control-plane guard (standing rule, mirroring
/// `tenancy::ControlPlaneGuard`): package administration and
/// module settings live in the `public` schema, so a request that
/// resolved to a tenant host is rejected outright (403). The
/// storefront reads (listing / getting published packages) are
/// the exception: tenants must be able to browse the catalog.
fn require_control_plane(req: &HttpRequest) -> UnifiedResult<()> {
    if TenantContext::from_request(req).is_some() {
        return Err(UnifiedError::forbidden(
            "Tenant requests cannot access the control plane",
        )
        .with_metadata("kind", serde_json::json!("tenant_control_plane_blocked")));
    }
    Ok(())
}

/// The tenant schema name for a company id, built only through
/// the tenancy helper — never inline, never hard-coded.
fn tenant_schema(company_id: &Uuid) -> String {
    tenancy::schema_name(*company_id)
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn now() -> DateTime<Utc> {
    Utc::now()
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

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw.trim())
        .map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
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

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

/// Turn a name into a URL-safe slug: lowercase, runs of other
/// characters become one hyphen, leading/trailing hyphens dropped.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_hyphen = true;
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_hyphen = false;
        } else if !last_hyphen {
            slug.push('-');
            last_hyphen = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= MAX_SLUG_LEN
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
        && slug
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

fn valid_subdomain(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SUBDOMAIN_LEN
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

/// Emit one audit-shaped event on the hook bus: the audit module
/// records events by name, so the payload carries the actor, the
/// action, and the target in a fixed shape.
fn emit_audit(
    ctx: &AppContext,
    event: &str,
    actor_id: &str,
    action: &str,
    target_type: &str,
    target_id: &str,
    company_id: Option<Uuid>,
    details: Value,
) {
    let mut payload = serde_json::json!({
        "actor_id": actor_id,
        "action": action,
        "target_type": target_type,
        "target_id": target_id,
        "details": details,
        "recorded_at": Utc::now(),
    });
    if let Some(company_id) = company_id {
        payload["company_id"] = serde_json::json!(company_id);
    }
    ctx.hooks.emit(event, payload);
}

fn emit_audit_state(
    state: &SaasPackagesState,
    event: &str,
    actor_id: &str,
    action: &str,
    target_type: &str,
    target_id: &str,
    company_id: Option<Uuid>,
    details: Value,
) {
    let mut payload = serde_json::json!({
        "actor_id": actor_id,
        "action": action,
        "target_type": target_type,
        "target_id": target_id,
        "details": details,
        "recorded_at": Utc::now(),
    });
    if let Some(company_id) = company_id {
        payload["company_id"] = serde_json::json!(company_id);
    }
    state.emit(event, payload);
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

/// Load a package by id or slug, or 404.
fn load_package(state: &SaasPackagesState, raw_id: &str) -> UnifiedResult<SaasPackage> {
    if let Ok(id) = Uuid::parse_str(raw_id.trim()) {
        if let Some(package) = state.packages.get(&id) {
            return Ok(package.value().clone());
        }
    }
    let slug = raw_id.trim();
    for entry in state.packages.iter() {
        if entry.value().slug == slug {
            return Ok(entry.value().clone());
        }
    }
    Err(UnifiedError::not_found(format!(
        "Package \"{raw_id}\" not found"
    )))
}

fn load_purchase(state: &SaasPackagesState, raw_id: &str) -> UnifiedResult<PackagePurchase> {
    let id = parse_uuid(raw_id, "purchase")?;
    state
        .purchases
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Purchase \"{raw_id}\" not found")))
}

fn load_version(state: &SaasPackagesState, package: &SaasPackage, version: i32) -> UnifiedResult<PackageVersion> {
    state
        .versions
        .get(&SaasPackagesState::version_key(&package.id, version))
        .map(|entry| entry.value().clone())
        .ok_or_else(|| {
            UnifiedError::not_found(format!(
                "Package \"{}\" has no version {version}",
                package.slug
            ))
        })
}

/// The module list of the version a purchase pinned.
fn purchase_version(state: &SaasPackagesState, purchase: &PackagePurchase) -> UnifiedResult<PackageVersion> {
    let package = state
        .packages
        .get(&purchase.package_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| {
            UnifiedError::not_found(format!(
                "Package \"{}\" of purchase \"{}\" not found",
                purchase.package_id, purchase.id
            ))
        })?;
    load_version(state, &package, purchase.package_version)
}

/// Validate a package module list from data against the live
/// registry: at least one entry, at most the configured cap, every
/// module id registered, no duplicate module id. Entries are
/// returned sorted by sort order.
fn validate_package_modules(
    registry: &ModuleRegistry,
    settings: &SaasPackagesSettings,
    entries: &[PackageModuleInput],
) -> UnifiedResult<Vec<PackageModuleEntry>> {
    if entries.is_empty() {
        return Err(UnifiedError::bad_request(
            "A package must list at least one module",
        ));
    }
    if entries.len() > settings.max_modules_per_package {
        return Err(UnifiedError::bad_request(format!(
            "A package may list at most {} modules",
            settings.max_modules_per_package
        )));
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut validated: Vec<PackageModuleEntry> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let module_id = entry.module_id.trim().to_string();
        if module_id.is_empty() {
            return Err(UnifiedError::bad_request(
                "A package module entry must name a module id",
            ));
        }
        if registry.get(&module_id).is_none() {
            return Err(UnifiedError::bad_request(format!(
                "Unknown module \"{module_id}\": no module with that id is registered"
            ))
            .with_metadata("module_id", serde_json::json!(module_id)));
        }
        if !seen.insert(module_id.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate module \"{module_id}\" in the package module list"
            )));
        }
        validated.push(PackageModuleEntry {
            module_id,
            required: entry.required,
            sort_order: entry.sort_order.unwrap_or(index as i32),
        });
    }
    validated.sort_by_key(|entry| entry.sort_order);
    Ok(validated)
}

/// Validate a wizard step list from data against the live settings
/// vocabulary: non-empty, at most the configured cap, every name
/// from the vocabulary, no repeats.
fn validate_wizard_steps(
    settings: &SaasPackagesSettings,
    steps: &[String],
) -> UnifiedResult<Vec<String>> {
    if steps.is_empty() {
        return Err(UnifiedError::bad_request(
            "A wizard step list must contain at least one step",
        ));
    }
    if steps.len() > settings.max_wizard_steps {
        return Err(UnifiedError::bad_request(format!(
            "A wizard step list must contain at most {} steps",
            settings.max_wizard_steps
        )));
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut cleaned: Vec<String> = Vec::new();
    for step in steps {
        let step = step.trim().to_string();
        if !settings.wizard_step_names.iter().any(|name| *name == step) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown wizard step \"{step}\" (expected one of: {})",
                settings.wizard_step_names.join(", ")
            )));
        }
        if !seen.insert(step.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate wizard step \"{step}\""
            )));
        }
        cleaned.push(step);
    }
    Ok(cleaned)
}

/// Resolve the full activation order for a set of module ids:
/// every module's transitive requirements first (deepest first),
/// then the module itself, de-duplicated with order preserved.
/// Fails closed with the registry's error (missing dependency or
/// cycle) when any module's graph is broken.
fn resolve_apply_order(
    registry: &ModuleRegistry,
    module_ids: &[String],
) -> UnifiedResult<Vec<String>> {
    let mut ordered: Vec<String> = Vec::new();
    for module_id in module_ids {
        for required in registry.transitive_requires_ordered(module_id)? {
            if !ordered.contains(&required) {
                ordered.push(required);
            }
        }
        if !ordered.contains(module_id) {
            ordered.push(module_id.clone());
        }
    }
    Ok(ordered)
}

/// Entitlement answer for one module and tenant, using the real
/// licensing check (HTTP path only — an `AppContext` is required).
/// Returns Ok(true) when entitled, Ok(false) when the tenant holds
/// no entitlement, and Err when the module itself is unknown.
fn entitlement_available_http(
    ctx: &AppContext,
    company_id: &Uuid,
    module_id: &str,
) -> UnifiedResult<bool> {
    let Some(module) = ctx.modules.get(module_id) else {
        return Err(UnifiedError::not_found(format!(
            "Module \"{module_id}\" not found"
        )));
    };
    if !module.available_as_product() || module.is_core() {
        return Ok(true);
    }
    match check_module_entitlement(ctx, &company_id.to_string(), module_id) {
        Ok(()) => Ok(true),
        Err(error) if error.code == 403 => Ok(false),
        Err(error) => Err(error),
    }
}

/// Entitlement answer for the hook path, where no `AppContext`
/// exists. Non-sellable and core modules pass from the registry
/// alone. A sellable non-core module is checked through the
/// `saas_packages.entitlement.check_requested` hook query
/// (fail-closed shape, like `role.check`): an explicit `allowed:
/// true` answer passes, anything else blocks. Returns
/// `Ok(Some(true/false))` for a decided answer, `Ok(None)` when no
/// handler answered at all (the caller records the step as
/// `blocked`, never as passed and never as silently skipped).
fn entitlement_available_hook(
    state: &SaasPackagesState,
    registry: &ModuleRegistry,
    company_id: &Uuid,
    module_id: &str,
) -> UnifiedResult<Option<bool>> {
    let Some(module) = registry.get(module_id) else {
        return Err(UnifiedError::not_found(format!(
            "Module \"{module_id}\" not found"
        )));
    };
    if !module.available_as_product() || module.is_core() {
        return Ok(Some(true));
    }
    let answered = state.emit_with_results(
        EVENT_ENTITLEMENT_CHECK_REQUESTED,
        json!({
            "company_id": company_id.to_string(),
            "module_id": module_id,
            "allowed": false,
        }),
    );
    // No handler answered at all (empty results) means the question
    // is undecided — the caller records a blocked step. Any answer
    // is read from the payload first (same shape as `role.check`)
    // and then from the contributed results.
    match answered {
        Some(context) if !context.results.is_empty() => Ok(Some(
            context
                .get("allowed")
                .and_then(|value| value.as_bool())
                .or_else(|| {
                    context
                        .results
                        .iter()
                        .filter_map(|value| value.get("allowed").and_then(|allowed| allowed.as_bool()))
                        .next()
                })
                .unwrap_or(false),
        )),
        _ => Ok(None),
    }
}

/// Compute the module diff between two module lists: what the
/// target adds, what it drops, and what both share.
fn module_diff(from: &[PackageModuleEntry], to: &[PackageModuleEntry]) -> PackageDiff {
    let from_ids: HashSet<&str> = from.iter().map(|entry| entry.module_id.as_str()).collect();
    let to_ids: HashSet<&str> = to.iter().map(|entry| entry.module_id.as_str()).collect();
    let mut to_enable: Vec<String> = to
        .iter()
        .filter(|entry| !from_ids.contains(entry.module_id.as_str()))
        .map(|entry| entry.module_id.clone())
        .collect();
    let mut to_disable: Vec<String> = from
        .iter()
        .filter(|entry| !to_ids.contains(entry.module_id.as_str()))
        .map(|entry| entry.module_id.clone())
        .collect();
    let mut shared: Vec<String> = from
        .iter()
        .filter(|entry| to_ids.contains(entry.module_id.as_str()))
        .map(|entry| entry.module_id.clone())
        .collect();
    to_enable.sort();
    to_disable.sort();
    shared.sort();
    PackageDiff {
        from_package_id: Uuid::nil(),
        to_package_id: Uuid::nil(),
        to_enable,
        to_disable,
        shared,
        kept_shared: Vec::new(),
    }
}

/// Full subdomain for a purchase: the requested label plus the
/// configured base domain, or the label alone when none is set.
fn full_subdomain(settings: &SaasPackagesSettings, subdomain: &str) -> String {
    let base = settings.subdomain_base_domain.trim().trim_start_matches('.');
    if base.is_empty() {
        subdomain.to_string()
    } else {
        format!("{subdomain}.{base}")
    }
}

fn steps_for_purchase(state: &SaasPackagesState, purchase_id: Uuid) -> Vec<ProvisionStep> {
    let mut steps: Vec<ProvisionStep> = state
        .steps
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|step| step.purchase_id == purchase_id)
        .collect();
    steps.sort_by_key(|step| (step.attempt, PROVISION_STEPS.iter().position(|name| *name == step.step).unwrap_or(usize::MAX)));
    steps
}

fn latest_step(state: &SaasPackagesState, purchase_id: Uuid, step_name: &str) -> Option<ProvisionStep> {
    steps_for_purchase(state, purchase_id)
        .into_iter()
        .filter(|step| step.step == step_name)
        .max_by_key(|step| step.attempt)
}

// ---------------------------------------------------------------------------
// Provisioning.
//
// The pipeline runs six steps in order. Two execution contexts
// exist: the HTTP path (a real `AppContext` is available, so the
// licensing module is consulted directly) and the hook path
// (`order.paid` and friends, where only the stored registry and
// hook bus exist, so entitlement is queried through the
// `saas_packages.entitlement.check_requested` hook). Both paths
// share this engine; the only difference is how entitlement is
// answered. A module the tenant is not entitled to BLOCKS the run
// with a 403-style error naming the module — it is never silently
// skipped. A step that cannot be decided on the hook path is
// recorded as `blocked` (not failed) and the purchase stays in
// `provisioning` until an admin retries through the HTTP endpoint.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProvisionMode {
    Full,
    RetryFailed,
}

/// Emit an event and return the handler context, from either the
/// HTTP context or the stored hook bus.
fn emit_event(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    event: &str,
    data: Value,
) -> Option<HookContext> {
    match http_ctx {
        Some(ctx) => Some(ctx.hooks.emit(event, data)),
        None => state.emit_with_results(event, data),
    }
}

fn emit_event_fire(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    event: &str,
    data: Value,
) {
    let _ = emit_event(state, http_ctx, event, data);
}

/// What one provisioning step decided.
struct StepDecision {
    status: &'static str,
    outcome: Option<String>,
    detail: Option<String>,
}

fn step_completed(outcome: &str, detail: String) -> StepDecision {
    StepDecision {
        status: STEP_COMPLETED,
        outcome: Some(outcome.to_string()),
        detail: Some(detail),
    }
}

fn step_failed(detail: String) -> StepDecision {
    StepDecision {
        status: STEP_FAILED,
        outcome: Some(OUTCOME_FAILED.to_string()),
        detail: Some(detail),
    }
}

fn step_blocked(detail: String) -> StepDecision {
    StepDecision {
        status: STEP_BLOCKED,
        outcome: None,
        detail: Some(detail),
    }
}

/// Check one module's entitlement in either execution context.
/// Returns Ok(true) entitled, Ok(false) not entitled, Err(unknown
/// module). `Ok(None)` from the hook query (nobody answered) is
/// reported through the `blocked` out-parameter instead of an
/// error, so the caller can record a blocked step.
fn check_one_entitlement(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    registry: &ModuleRegistry,
    company_id: &Uuid,
    module_id: &str,
) -> UnifiedResult<Option<bool>> {
    match http_ctx {
        Some(ctx) => entitlement_available_http(ctx, company_id, module_id).map(Some),
        None => entitlement_available_hook(state, registry, company_id, module_id),
    }
}

fn build_summary(state: &SaasPackagesState, purchase: &PackagePurchase) -> ProvisionSummary {
    let steps = steps_for_purchase(state, purchase.id);
    let latest_attempt = steps.iter().map(|step| step.attempt).max().unwrap_or(0);
    let current: Vec<ProvisionStep> = steps
        .iter()
        .filter(|step| step.attempt == latest_attempt)
        .cloned()
        .collect();
    let module_outcomes = latest_step(state, purchase.id, STEP_ENABLE_MODULES)
        .and_then(|step| step.detail)
        .and_then(|detail| serde_json::from_str::<Vec<ModuleProvisionOutcome>>(&detail).ok())
        .unwrap_or_default();
    let blocked_reason = current
        .iter()
        .find(|step| step.status == STEP_BLOCKED)
        .and_then(|step| step.detail.clone());
    let status = if purchase.status == PURCHASE_ACTIVE {
        PURCHASE_ACTIVE.to_string()
    } else if current.iter().any(|step| step.status == STEP_FAILED) {
        PURCHASE_FAILED.to_string()
    } else if current.iter().any(|step| step.status == STEP_BLOCKED) {
        PURCHASE_PROVISIONING.to_string()
    } else {
        purchase.status.clone()
    };
    ProvisionSummary {
        purchase_id: purchase.id,
        package_id: purchase.package_id,
        package_version: purchase.package_version,
        company_id: purchase.company_id,
        tenant_schema: tenant_schema(&purchase.company_id),
        status,
        attempt: latest_attempt,
        steps: current,
        module_outcomes,
        subdomain_pending: purchase.subdomain_pending,
        blocked_reason,
        updated_at: purchase.updated_at,
    }
}

/// Run the provisioning pipeline for one purchase. `http_ctx` is
/// Some on the HTTP path and None on the hook path. Returns the
/// fresh summary; a failed or blocked run is a summary, not an
/// error — only a missing purchase, a missing registry, or an
/// exhausted attempt budget returns Err.
fn run_provisioning(
    state: &SaasPackagesState,
    purchase_id: Uuid,
    actor: &str,
    mode: ProvisionMode,
    http_ctx: Option<&AppContext>,
) -> UnifiedResult<ProvisionSummary> {
    let settings = state.settings();
    let mut purchase = state
        .purchases
        .get(&purchase_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Purchase \"{purchase_id}\" not found")))?;
    let version = purchase_version(state, &purchase)?;
    let registry = match http_ctx {
        Some(ctx) => ctx.modules.clone(),
        None => state
            .registry()
            .ok_or_else(|| UnifiedError::conflict("Module registry is not available yet"))?,
    };
    if purchase.provision_attempts >= settings.provisioning_max_attempts
        && (mode == ProvisionMode::Full
            || steps_for_purchase(state, purchase_id)
                .iter()
                .any(|step| step.status == STEP_FAILED || step.status == STEP_BLOCKED))
    {
        return Err(UnifiedError::conflict(format!(
            "Provisioning attempt limit reached for purchase \"{purchase_id}\" (maximum {} attempts); an administrator must reset the purchase",
            settings.provisioning_max_attempts
        )));
    }
    let attempt = purchase.provision_attempts + 1;
    purchase.provision_attempts = attempt;
    purchase.status = PURCHASE_PROVISIONING.to_string();
    purchase.updated_at = now();
    state.purchases.insert(purchase.id, purchase.clone());
    emit_event_fire(
        state,
        http_ctx,
        EVENT_PROVISION_STARTED,
        json!({
            "purchase_id": purchase.id,
            "package_id": purchase.package_id,
            "package_version": purchase.package_version,
            "company_id": purchase.company_id,
            "tenant_schema": tenant_schema(&purchase.company_id),
            "attempt": attempt,
            "actor_id": actor,
        }),
    );
    state.record_history(
        purchase.id,
        "provision.started",
        actor,
        json!({ "attempt": attempt, "mode": if mode == ProvisionMode::Full { "full" } else { "retry_failed" } }),
    );

    // Steps to run this attempt: everything on a full run; only
    // failed, blocked, or never-reached (skipped) steps on retry.
    let rerun: Vec<&str> = match mode {
        ProvisionMode::Full => PROVISION_STEPS.to_vec(),
        ProvisionMode::RetryFailed => PROVISION_STEPS
            .iter()
            .copied()
            .filter(|name| {
                matches!(
                    latest_step(state, purchase_id, name).map(|step| step.status.as_str()),
                    Some(STEP_FAILED) | Some(STEP_BLOCKED) | Some(STEP_SKIPPED) | None
                )
            })
            .collect(),
    };

    let mut module_outcomes: Vec<ModuleProvisionOutcome> = latest_step(state, purchase_id, STEP_ENABLE_MODULES)
        .and_then(|step| step.detail)
        .and_then(|detail| serde_json::from_str::<Vec<ModuleProvisionOutcome>>(&detail).ok())
        .unwrap_or_default();
    let mut failed: Option<String> = None;
    let mut blocked: Option<String> = None;

    for step_name in PROVISION_STEPS {
        if failed.is_some() || blocked.is_some() {
            // Never reached: record as skipped so the attempt shows
            // the whole pipeline, and retry can pick it up later.
            if rerun.contains(step_name) || mode == ProvisionMode::Full {
                record_step(state, purchase_id, step_name, attempt, StepDecision {
                    status: STEP_SKIPPED,
                    outcome: Some("skipped".to_string()),
                    detail: Some("Not reached: an earlier step failed or was blocked".to_string()),
                });
            }
            continue;
        }
        if !rerun.contains(step_name) {
            continue;
        }
        let mut step = ProvisionStep {
            id: Uuid::now_v7(),
            purchase_id,
            step: step_name.to_string(),
            status: STEP_RUNNING.to_string(),
            outcome: None,
            detail: None,
            attempt,
            started_at: now(),
            completed_at: None,
        };
        state.steps.insert(step.id, step.clone());
        let decision = execute_step(
            state,
            http_ctx,
            &registry,
            &mut purchase,
            &version,
            actor,
            step_name,
            &mut module_outcomes,
        );
        step.status = decision.status.to_string();
        step.outcome = decision.outcome;
        step.detail = decision.detail.clone();
        step.completed_at = Some(now());
        state.steps.insert(step.id, step.clone());
        emit_event_fire(
            state,
            http_ctx,
            EVENT_PROVISION_STEP_COMPLETED,
            json!({
                "purchase_id": purchase.id,
                "step": step_name,
                "status": step.status,
                "outcome": step.outcome,
                "attempt": attempt,
            }),
        );
        match decision.status {
            STEP_FAILED => failed = decision.detail,
            STEP_BLOCKED => blocked = decision.detail,
            _ => {}
        }
    }

    // Refresh the purchase row (steps may have changed it: subdomain
    // pending flag, enabled modules).
    purchase = state
        .purchases
        .get(&purchase_id)
        .map(|entry| entry.value().clone())
        .unwrap_or(purchase);
    if let Some(reason) = &failed {
        purchase.status = PURCHASE_FAILED.to_string();
        purchase.updated_at = now();
        state.purchases.insert(purchase.id, purchase.clone());
        emit_event_fire(
            state,
            http_ctx,
            EVENT_PROVISION_FAILED,
            json!({
                "purchase_id": purchase.id,
                "package_id": purchase.package_id,
                "company_id": purchase.company_id,
                "attempt": attempt,
                "reason": reason,
            }),
        );
        state.record_history(
            purchase.id,
            "provision.failed",
            actor,
            json!({ "attempt": attempt, "reason": reason }),
        );
    } else if let Some(reason) = &blocked {
        purchase.status = PURCHASE_PROVISIONING.to_string();
        purchase.updated_at = now();
        state.purchases.insert(purchase.id, purchase.clone());
        state.record_history(
            purchase.id,
            "provision.blocked",
            actor,
            json!({ "attempt": attempt, "reason": reason }),
        );
    } else {
        purchase.status = PURCHASE_ACTIVE.to_string();
        purchase.updated_at = now();
        state.purchases.insert(purchase.id, purchase.clone());
        emit_event_fire(
            state,
            http_ctx,
            EVENT_PROVISION_COMPLETED,
            json!({
                "purchase_id": purchase.id,
                "package_id": purchase.package_id,
                "package_version": purchase.package_version,
                "company_id": purchase.company_id,
                "tenant_schema": tenant_schema(&purchase.company_id),
                "attempt": attempt,
                "enabled_modules": purchase.enabled_modules,
                "subdomain_pending": purchase.subdomain_pending,
            }),
        );
        emit_event_fire(
            state,
            http_ctx,
            EVENT_PURCHASE_COMPLETED,
            json!({
                "purchase_id": purchase.id,
                "package_id": purchase.package_id,
                "company_id": purchase.company_id,
            }),
        );
        state.record_history(
            purchase.id,
            "provision.completed",
            actor,
            json!({ "attempt": attempt, "enabled_modules": purchase.enabled_modules }),
        );
    }
    let summary = build_summary(state, &purchase);
    Ok(summary)
}

fn record_step(
    state: &SaasPackagesState,
    purchase_id: Uuid,
    step_name: &str,
    attempt: i32,
    decision: StepDecision,
) {
    let step = ProvisionStep {
        id: Uuid::now_v7(),
        purchase_id,
        step: step_name.to_string(),
        status: decision.status.to_string(),
        outcome: decision.outcome,
        detail: decision.detail,
        attempt,
        started_at: now(),
        completed_at: Some(now()),
    };
    state.steps.insert(step.id, step);
}

/// Execute one pipeline step. Returns the step decision; a failed
/// or blocked decision stops the pipeline (the runner handles it).
#[allow(clippy::too_many_arguments)]
fn execute_step(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    registry: &ModuleRegistry,
    purchase: &mut PackagePurchase,
    version: &PackageVersion,
    actor: &str,
    step_name: &str,
    module_outcomes: &mut Vec<ModuleProvisionOutcome>,
) -> StepDecision {
    match step_name {
        STEP_VALIDATE_ENTITLEMENTS => validate_entitlements_step(state, http_ctx, registry, purchase, version),
        STEP_PROVISION_SUBDOMAIN => provision_subdomain_step(state, http_ctx, purchase, version),
        STEP_ENABLE_MODULES => enable_modules_step(state, http_ctx, registry, purchase, version, module_outcomes),
        STEP_APPLY_PRESET_SETTINGS => apply_settings_step(state, http_ctx, purchase, version, actor),
        STEP_RUN_SETUP_WIZARD => run_wizard_step(state, http_ctx, purchase, version, actor),
        STEP_SEND_WELCOME => send_welcome_step(state, http_ctx, purchase, version),
        _ => step_failed(format!("Unknown provisioning step \"{step_name}\"")),
    }
}

fn validate_entitlements_step(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    registry: &ModuleRegistry,
    purchase: &PackagePurchase,
    version: &PackageVersion,
) -> StepDecision {
    for entry in &version.modules {
        if registry.get(&entry.module_id).is_none() {
            return step_failed(format!(
                "Unknown module \"{}\": no module with that id is registered",
                entry.module_id
            ));
        }
        match check_one_entitlement(state, http_ctx, registry, &purchase.company_id, &entry.module_id) {
            Ok(Some(true)) => {}
            Ok(Some(false)) => {
                return step_failed(format!(
                    "Tenant \"{}\" is not entitled to module \"{}\": provisioning blocked (the module requires an active license entitlement)",
                    purchase.company_id, entry.module_id
                ));
            }
            Ok(None) => {
                return step_blocked(format!(
                    "No entitlement handler answered for module \"{}\": provisioning is paused until the entitlement can be checked; retry provisioning to re-check",
                    entry.module_id
                ));
            }
            Err(error) => return step_failed(error.message),
        }
    }
    step_completed(
        OUTCOME_ENABLED,
        format!(
            "All {} package modules are registered and entitled for tenant \"{}\"",
            version.modules.len(),
            purchase.company_id
        ),
    )
}

fn provision_subdomain_step(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    purchase: &mut PackagePurchase,
    _version: &PackageVersion,
) -> StepDecision {
    let settings = state.settings();
    let label = match &purchase.subdomain {
        Some(label) => label.clone(),
        None => format!("pkg-{}", purchase.id.to_string().replace('-', "")[..8].to_string()),
    };
    let domain = full_subdomain(&settings, &label);
    let answered = emit_event(
        state,
        http_ctx,
        EVENT_SITE_PROVISION_REQUESTED,
        json!({
            "purchase_id": purchase.id,
            "company_id": purchase.company_id,
            "tenant_schema": tenant_schema(&purchase.company_id),
            "subdomain": label,
            "domain": domain,
            "package_id": purchase.package_id,
            "source": "saas_packages",
        }),
    );
    let claimed = answered
        .as_ref()
        .map(|context| {
            context.results.iter().any(|result| {
                result.get("claimed").and_then(|value| value.as_bool()).unwrap_or(false)
                    || result.get("site_id").is_some()
                    || result.get("domain").is_some()
            })
        })
        .unwrap_or(false);
    if claimed {
        purchase.subdomain = Some(label);
        purchase.subdomain_pending = false;
        purchase.updated_at = now();
        state.purchases.insert(purchase.id, purchase.clone());
        step_completed(
            "provisioned",
            format!("Tenant subdomain \"{domain}\" was claimed by the domains flow"),
        )
    } else {
        purchase.subdomain = Some(label);
        purchase.subdomain_pending = true;
        purchase.updated_at = now();
        state.purchases.insert(purchase.id, purchase.clone());
        step_completed(
            OUTCOME_REQUESTED,
            format!(
                "Subdomain \"{domain}\" requested from the domains flow; no handler has claimed it yet, so the subdomain stays pending until the domains flow provisions it"
            ),
        )
    }
}

fn enable_modules_step(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    registry: &ModuleRegistry,
    purchase: &mut PackagePurchase,
    version: &PackageVersion,
    module_outcomes: &mut Vec<ModuleProvisionOutcome>,
) -> StepDecision {
    let module_ids: Vec<String> = version
        .modules
        .iter()
        .map(|entry| entry.module_id.clone())
        .collect();
    let ordered = match resolve_apply_order(registry, &module_ids) {
        Ok(ordered) => ordered,
        Err(error) => {
            let missing: Vec<String> = error
                .metadata
                .get("missing_required")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            for entry in &version.modules {
                module_outcomes.push(ModuleProvisionOutcome {
                    module_id: entry.module_id.clone(),
                    status: OUTCOME_SKIPPED_DEPENDENCY_MISSING.to_string(),
                    reason: Some(if missing.is_empty() {
                        error.message.clone()
                    } else {
                        format!("Missing dependency module(s): {}", missing.join(", "))
                    }),
                    auto_activated: Vec::new(),
                });
            }
            return step_failed(format!(
                "Cannot resolve the module dependency order for package \"{}\": {}",
                version.package_id, error.message
            ));
        }
    };
    let in_package: HashSet<&str> = module_ids.iter().map(|id| id.as_str()).collect();
    let mut enabled_before: HashSet<String> = ordered
        .iter()
        .filter(|id| state.tenant_has_module(&purchase.company_id, id))
        .cloned()
        .collect();
    for module_id in &ordered {
        if state.tenant_has_module(&purchase.company_id, module_id) {
            if in_package.contains(module_id.as_str()) {
                module_outcomes.push(ModuleProvisionOutcome {
                    module_id: module_id.clone(),
                    status: OUTCOME_ALREADY_ENABLED.to_string(),
                    reason: None,
                    auto_activated: Vec::new(),
                });
            }
            continue;
        }
        // Entitlement gate, re-checked per module (defense in depth):
        // a module the tenant is not entitled to BLOCKS the run with
        // a 403-style error naming the module — never silently skipped.
        match check_one_entitlement(state, http_ctx, registry, &purchase.company_id, module_id) {
            Ok(Some(true)) => {}
            Ok(Some(false)) => {
                return step_failed(format!(
                    "Tenant \"{}\" is not entitled to module \"{}\": provisioning blocked (the module requires an active license entitlement)",
                    purchase.company_id, module_id
                ));
            }
            Ok(None) => {
                return step_blocked(format!(
                    "No entitlement handler answered for module \"{module_id}\": provisioning is paused until the entitlement can be checked; retry provisioning to re-check"
                ));
            }
            Err(error) => return step_failed(error.message),
        }
        let auto_activated: Vec<String> = registry
            .transitive_requires_ordered(module_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|required| !enabled_before.contains(required))
            .collect();
        for required in &auto_activated {
            state.mark_tenant_module(&purchase.company_id, required, purchase.id);
            emit_event_fire(
                state,
                http_ctx,
                events::MODULE_ENABLED,
                json!({
                    "id": required,
                    "company_id": purchase.company_id,
                    "package_id": purchase.package_id,
                    "purchase_id": purchase.id,
                    "auto_activated": true,
                    "auto_activated_by": module_id,
                }),
            );
        }
        state.mark_tenant_module(&purchase.company_id, module_id, purchase.id);
        enabled_before.insert(module_id.clone());
        emit_event_fire(
            state,
            http_ctx,
            events::MODULE_ENABLED,
            json!({
                "id": module_id,
                "company_id": purchase.company_id,
                "package_id": purchase.package_id,
                "purchase_id": purchase.id,
                "actor_id": purchase.buyer_id,
            }),
        );
        emit_event_fire(
            state,
            http_ctx,
            EVENT_MODULE_ENABLED,
            json!({
                "package_id": purchase.package_id,
                "purchase_id": purchase.id,
                "company_id": purchase.company_id,
                "module_id": module_id,
                "actor_id": purchase.buyer_id,
                "auto_activated": auto_activated,
            }),
        );
        if in_package.contains(module_id.as_str()) {
            module_outcomes.push(ModuleProvisionOutcome {
                module_id: module_id.clone(),
                status: OUTCOME_ENABLED.to_string(),
                reason: None,
                auto_activated,
            });
        }
    }
    // Keep outcomes in package order (dependencies report through
    // their dependent's `auto_activated` list).
    module_outcomes.sort_by_key(|outcome| {
        version
            .modules
            .iter()
            .position(|entry| entry.module_id == outcome.module_id)
            .unwrap_or(usize::MAX)
    });
    let enabled: Vec<String> = module_outcomes
        .iter()
        .filter(|outcome| outcome.status == OUTCOME_ENABLED || outcome.status == OUTCOME_ALREADY_ENABLED)
        .map(|outcome| outcome.module_id.clone())
        .collect();
    purchase.enabled_modules = enabled;
    purchase.updated_at = now();
    state.purchases.insert(purchase.id, purchase.clone());
    // The outcomes travel in the step detail so the GET provision
    // endpoints can rebuild them without re-running anything.
    let detail = serde_json::to_string(&module_outcomes)
        .unwrap_or_else(|_| "[]".to_string());
    StepDecision {
        status: STEP_COMPLETED,
        outcome: Some(OUTCOME_ENABLED.to_string()),
        detail: Some(detail),
    }
}

fn apply_settings_step(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    purchase: &PackagePurchase,
    version: &PackageVersion,
    actor: &str,
) -> StepDecision {
    // Settings handoff only: the owning modules apply the package
    // settings. Nothing is applied here, and the settings payload
    // is never interpreted by this module.
    emit_event_fire(
        state,
        http_ctx,
        EVENT_SETTINGS_APPLIED,
        json!({
            "preset_id": version.package_id,
            "package_id": version.package_id,
            "application_id": purchase.id,
            "purchase_id": purchase.id,
            "company_id": purchase.company_id,
            "actor_id": actor,
            "settings": version.preset_settings,
        }),
    );
    step_completed(
        OUTCOME_REQUESTED,
        "Package settings handed off to the owning modules through the settings event".to_string(),
    )
}

fn run_wizard_step(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    purchase: &PackagePurchase,
    version: &PackageVersion,
    actor: &str,
) -> StepDecision {
    emit_event_fire(
        state,
        http_ctx,
        EVENT_WIZARD_PREFILL_REQUESTED,
        json!({
            "purchase_id": purchase.id,
            "company_id": purchase.company_id,
            "package_id": version.package_id,
            "package_version": version.version,
            "actor_id": actor,
            "steps": version.wizard_steps,
            "settings": version.preset_settings,
        }),
    );
    step_completed(
        OUTCOME_REQUESTED,
        "Setup wizard pre-fill requested from the presets flow with the package steps and settings".to_string(),
    )
}

fn send_welcome_step(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    purchase: &PackagePurchase,
    version: &PackageVersion,
) -> StepDecision {
    let settings = state.settings();
    let link = purchase.subdomain.as_ref().map(|label| {
        format!("https://{}", full_subdomain(&settings, label))
    });
    // No credential is ever carried in the payload: credentials are
    // issued by the auth/mail flow and referenced only by the
    // tenant link. Delivery itself belongs to notifications.
    let mut payload = json!({
        "kind": "saas_package_welcome",
        "user_id": purchase.buyer_id,
        "title": format!("Your \"{}\" workspace is ready", version.name),
        "body": format!("Your purchase of \"{}\" is provisioned. Sign in at your tenant workspace to finish setup with the guided wizard.", version.name),
        "module": "saas_packages",
        "category": "saas_packages",
        "priority": "normal",
        "company_id": purchase.company_id,
        "purchase_id": purchase.id,
        "package_id": purchase.package_id,
    });
    if let Some(link) = link {
        payload["link"] = json!(link);
    }
    emit_event_fire(state, http_ctx, "notification.requested", payload);
    step_completed(
        OUTCOME_REQUESTED,
        "Welcome message requested from the notifications flow".to_string(),
    )
}

// ---------------------------------------------------------------------------
// HTTP handlers — packages.
// ---------------------------------------------------------------------------

async fn list_packages(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let vendor_filter = params.filters.get("vendor_id").cloned();
        let is_manager = check_capability(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE);
        let search = params.search.clone().map(|value| value.to_lowercase());
        let mut items: Vec<SaasPackage> = state
            .packages
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|package| {
                // Non-managers only ever see published packages.
                if !is_manager && package.status != STATUS_PUBLISHED {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &package.status != status {
                        return false;
                    }
                }
                if let Some(vendor) = &vendor_filter {
                    if package.vendor_id.as_deref() != Some(vendor.as_str()) {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    let haystack = format!("{} {} {}", package.name, package.slug, package.description).to_lowercase();
                    if !haystack.contains(needle) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    body: web::Json<CreatePackageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasPackage> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
        require_control_plane(&req)?;
        let input = body.into_inner();
        let settings = state.settings();
        if !settings.vendor_packages_enabled && input.vendor_id.is_some() {
            return Err(UnifiedError::forbidden(
                "Vendor packages are disabled by the module settings",
            ));
        }
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?
            .unwrap_or_default();
        let slug = match &input.slug {
            Some(raw) => {
                let slug = require_text(raw, "slug", MAX_SLUG_LEN)?.to_lowercase();
                if !valid_slug(&slug) {
                    return Err(UnifiedError::bad_request(
                        "slug must be lowercase letters, digits, and single hyphens, without leading or trailing hyphens",
                    ));
                }
                slug
            }
            None => slugify(&name),
        };
        if slug.is_empty() {
            return Err(UnifiedError::bad_request(
                "slug must not be empty (provide a slug or a name with letters or digits)",
            ));
        }
        if state.packages.iter().any(|entry| entry.value().slug == slug) {
            return Err(UnifiedError::conflict(format!(
                "A package with slug \"{slug}\" already exists"
            )));
        }
        let product_id = optional_text(&input.product_id, "product_id", MAX_REF_LEN)?;
        let preset_settings = input.preset_settings.unwrap_or(Value::Object(Map::new()));
        if !preset_settings.is_object() {
            return Err(UnifiedError::bad_request(
                "preset_settings must be a JSON object",
            ));
        }
        let wizard_steps = match &input.wizard_steps {
            Some(steps) => validate_wizard_steps(&settings, steps)?,
            None => settings.wizard_step_names.clone(),
        };
        let modules = validate_package_modules(&ctx.modules, &settings, &input.modules)?;
        let vendor_id = optional_text(&input.vendor_id, "vendor_id", MAX_REF_LEN)?;
        let now = Utc::now();
        let package = SaasPackage {
            id: Uuid::now_v7(),
            slug,
            name,
            description,
            status: STATUS_DRAFT.to_string(),
            current_version: 1,
            product_id,
            preset_settings,
            wizard_steps,
            modules,
            created_by: caller.actor.clone(),
            vendor_id,
            created_at: now,
            updated_at: now,
        };
        state.store_version(&package);
        state.packages.insert(package.id, package.clone());
        emit_audit(
            &ctx,
            EVENT_PACKAGE_CREATED,
            &caller.actor,
            "saas_packages.package.created",
            "saas_package",
            &package.id.to_string(),
            None,
            json!({ "slug": package.slug, "status": package.status }),
        );
        Ok(package)
    })();
    match result {
        Ok(package) => created(package),
        Err(error) => err(error, debug),
    }
}

async fn get_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasPackage> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let package = load_package(&state, &path.into_inner())?;
        if package.status != STATUS_PUBLISHED
            && !check_capability(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)
        {
            return Err(UnifiedError::not_found("Package not found"));
        }
        Ok(package)
    })();
    match result {
        Ok(package) => ok(package),
        Err(error) => err(error, debug),
    }
}

async fn update_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePackageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasPackage> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
        require_control_plane(&req)?;
        let input = body.into_inner();
        let mut package = load_package(&state, &path.into_inner())?;
        if package.status == STATUS_ARCHIVED {
            return Err(UnifiedError::conflict(
                "An archived package cannot be edited; clone it to create a new draft",
            ));
        }
        let settings = state.settings();
        // Editing a published package creates a new version: the old
        // snapshot stays frozen and existing purchases keep it.
        let new_version = package.status == STATUS_PUBLISHED;
        if let Some(name) = &input.name {
            package.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            package.description = require_text(description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(product_id) = &input.product_id {
            package.product_id = Some(require_text(product_id, "product_id", MAX_REF_LEN)?);
        }
        if let Some(preset_settings) = &input.preset_settings {
            if !preset_settings.is_object() {
                return Err(UnifiedError::bad_request(
                    "preset_settings must be a JSON object",
                ));
            }
            package.preset_settings = preset_settings.clone();
        }
        if let Some(steps) = &input.wizard_steps {
            package.wizard_steps = validate_wizard_steps(&settings, steps)?;
        }
        if let Some(modules) = &input.modules {
            package.modules = validate_package_modules(&ctx.modules, &settings, modules)?;
        }
        if new_version {
            package.current_version += 1;
        }
        package.updated_at = now();
        state.store_version(&package);
        state.packages.insert(package.id, package.clone());
        emit_audit(
            &ctx,
            EVENT_PACKAGE_UPDATED,
            &caller.actor,
            "saas_packages.package.updated",
            "saas_package",
            &package.id.to_string(),
            None,
            json!({
                "slug": package.slug,
                "version": package.current_version,
                "new_version": new_version,
            }),
        );
        Ok(package)
    })();
    match result {
        Ok(package) => ok(package),
        Err(error) => err(error, debug),
    }
}

async fn delete_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
        require_control_plane(&req)?;
        let package = load_package(&state, &path.into_inner())?;
        if package.status == STATUS_PUBLISHED {
            return Err(UnifiedError::conflict(
                "A published package cannot be deleted; archive it first",
            ));
        }
        if state
            .purchases
            .iter()
            .any(|entry| entry.value().package_id == package.id)
        {
            return Err(UnifiedError::conflict(
                "A package with purchases cannot be deleted; archive it instead",
            ));
        }
        state.packages.remove(&package.id);
        state
            .versions
            .retain(|_, version| version.package_id != package.id);
        emit_audit(
            &ctx,
            EVENT_PACKAGE_DELETED,
            &caller.actor,
            "saas_packages.package.deleted",
            "saas_package",
            &package.id.to_string(),
            None,
            json!({ "slug": package.slug }),
        );
        Ok(json!({ "deleted": package.id, "slug": package.slug }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn publish_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasPackage> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
        require_control_plane(&req)?;
        let mut package = load_package(&state, &path.into_inner())?;
        if package.status == STATUS_PUBLISHED {
            return Err(UnifiedError::conflict("The package is already published"));
        }
        if package.status == STATUS_ARCHIVED {
            return Err(UnifiedError::conflict(
                "An archived package cannot be published; clone it to create a new draft",
            ));
        }
        if package.modules.is_empty() {
            return Err(UnifiedError::bad_request(
                "A package with no modules cannot be published",
            ));
        }
        package.status = STATUS_PUBLISHED.to_string();
        package.updated_at = now();
        state.packages.insert(package.id, package.clone());
        emit_audit(
            &ctx,
            EVENT_PACKAGE_PUBLISHED,
            &caller.actor,
            "saas_packages.package.published",
            "saas_package",
            &package.id.to_string(),
            None,
            json!({ "slug": package.slug, "version": package.current_version }),
        );
        Ok(package)
    })();
    match result {
        Ok(package) => ok(package),
        Err(error) => err(error, debug),
    }
}

async fn archive_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasPackage> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
        require_control_plane(&req)?;
        let mut package = load_package(&state, &path.into_inner())?;
        if package.status == STATUS_ARCHIVED {
            return Err(UnifiedError::conflict("The package is already archived"));
        }
        package.status = STATUS_ARCHIVED.to_string();
        package.updated_at = now();
        state.packages.insert(package.id, package.clone());
        emit_audit(
            &ctx,
            EVENT_PACKAGE_ARCHIVED,
            &caller.actor,
            "saas_packages.package.archived",
            "saas_package",
            &package.id.to_string(),
            None,
            json!({ "slug": package.slug }),
        );
        Ok(package)
    })();
    match result {
        Ok(package) => ok(package),
        Err(error) => err(error, debug),
    }
}

async fn clone_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasPackage> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
        require_control_plane(&req)?;
        let source = load_package(&state, &path.into_inner())?;
        let mut slug = format!("{}-copy", source.slug);
        let mut suffix = 2;
        while state.packages.iter().any(|entry| entry.value().slug == slug) {
            slug = format!("{}-copy-{suffix}", source.slug);
            suffix += 1;
        }
        let now = Utc::now();
        let clone = SaasPackage {
            id: Uuid::now_v7(),
            slug,
            name: format!("{} (copy)", source.name),
            description: source.description.clone(),
            status: STATUS_DRAFT.to_string(),
            current_version: 1,
            product_id: source.product_id.clone(),
            preset_settings: source.preset_settings.clone(),
            wizard_steps: source.wizard_steps.clone(),
            modules: source.modules.clone(),
            created_by: caller.actor.clone(),
            vendor_id: source.vendor_id.clone(),
            created_at: now,
            updated_at: now,
        };
        state.store_version(&clone);
        state.packages.insert(clone.id, clone.clone());
        emit_audit(
            &ctx,
            EVENT_PACKAGE_CLONED,
            &caller.actor,
            "saas_packages.package.cloned",
            "saas_package",
            &clone.id.to_string(),
            None,
            json!({ "slug": clone.slug, "source_package_id": source.id }),
        );
        Ok(clone)
    })();
    match result {
        Ok(package) => created(package),
        Err(error) => err(error, debug),
    }
}

async fn list_package_versions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let package = load_package(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<PackageVersion> = state
            .versions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|version| version.package_id == package.id)
            .collect();
        items.sort_by(|a, b| b.version.cmp(&a.version));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_package_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PackageVersion> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let (raw_id, raw_version) = path.into_inner();
        let package = load_package(&state, &raw_id)?;
        let version: i32 = raw_version.trim().parse().map_err(|_| {
            UnifiedError::bad_request(format!("Invalid version \"{raw_version}\" (expected an integer)"))
        })?;
        load_version(&state, &package, version)
    })();
    match result {
        Ok(version) => ok(version),
        Err(error) => err(error, debug),
    }
}

async fn validate_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ValidatePackageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let package = load_package(&state, &path.into_inner())?;
        let company_id = match &body.company_id {
            Some(raw) => Some(parse_uuid(raw, "company")?),
            None => None,
        };
        if let Some(company_id) = &company_id {
            require_tenant_scope(&req, company_id)?;
        }
        let module_ids: Vec<String> = package
            .modules
            .iter()
            .map(|entry| entry.module_id.clone())
            .collect();
        let mut unknown: Vec<String> = Vec::new();
        let mut not_entitled: Vec<String> = Vec::new();
        let mut module_rows: Vec<Value> = Vec::new();
        for entry in &package.modules {
            let registered = ctx.modules.get(&entry.module_id).is_some();
            if !registered {
                unknown.push(entry.module_id.clone());
            }
            let entitled = match (&company_id, registered) {
                (Some(company_id), true) => {
                    match entitlement_available_http(&ctx, company_id, &entry.module_id) {
                        Ok(entitled) => Some(entitled),
                        Err(_) => Some(false),
                    }
                }
                _ => None,
            };
            if entitled == Some(false) {
                not_entitled.push(entry.module_id.clone());
            }
            module_rows.push(json!({
                "module_id": entry.module_id,
                "required": entry.required,
                "sort_order": entry.sort_order,
                "registered": registered,
                "entitled": entitled,
            }));
        }
        let apply_order = resolve_apply_order(&ctx.modules, &module_ids)
            .ok();
        Ok(json!({
            "package_id": package.id,
            "slug": package.slug,
            "version": package.current_version,
            "status": package.status,
            "modules": module_rows,
            "apply_order": apply_order,
            "unknown_modules": unknown,
            "not_entitled_modules": not_entitled,
            "valid": unknown.is_empty() && not_entitled.is_empty(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_package_modules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let package = load_package(&state, &path.into_inner())?;
        let params = query.into_inner();
        let items: Vec<Value> = package
            .modules
            .iter()
            .map(|entry| {
                let module = ctx.modules.get(&entry.module_id);
                json!({
                    "module_id": entry.module_id,
                    "required": entry.required,
                    "sort_order": entry.sort_order,
                    "registered": module.is_some(),
                    "name": module.map(|module| module.name().to_string()),
                    "available_as_product": module.map(|module| module.available_as_product()),
                })
            })
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn diff_packages(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PackageDiff> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let (from_id, to_id) = path.into_inner();
        let from = load_package(&state, &from_id)?;
        let to = load_package(&state, &to_id)?;
        let mut diff = module_diff(&from.modules, &to.modules);
        diff.from_package_id = from.id;
        diff.to_package_id = to.id;
        Ok(diff)
    })();
    match result {
        Ok(diff) => ok(diff),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers — purchases, provisioning, lifecycle.
// ---------------------------------------------------------------------------

async fn purchase_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PurchaseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PackagePurchase> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_PURCHASE)?;
        let package = load_package(&state, &path.into_inner())?;
        if package.status != STATUS_PUBLISHED {
            return Err(UnifiedError::conflict(format!(
                "Only a published package can be purchased (current status: {})",
                package.status
            )));
        }
        let input = body.into_inner();
        let buyer_id = match &input.buyer_id {
            Some(raw) => require_text(raw, "buyer_id", MAX_REF_LEN)?,
            None => caller.actor.clone(),
        };
        let company_id = parse_uuid(&input.company_id, "company")?;
        require_tenant_scope(&req, &company_id)?;
        let subdomain = match &input.subdomain {
            Some(raw) => {
                let label = require_text(raw, "subdomain", MAX_SUBDOMAIN_LEN)?.to_lowercase();
                if !valid_subdomain(&label) {
                    return Err(UnifiedError::bad_request(
                        "subdomain must be lowercase letters, digits, and single hyphens, without leading or trailing hyphens",
                    ));
                }
                Some(label)
            }
            None => None,
        };
        let order_id = optional_text(&input.order_id, "order_id", MAX_REF_LEN)?;
        let subscription_id = optional_text(&input.subscription_id, "subscription_id", MAX_REF_LEN)?;
        let now = Utc::now();
        let purchase = PackagePurchase {
            id: Uuid::now_v7(),
            package_id: package.id,
            package_version: package.current_version,
            buyer_id: buyer_id.clone(),
            company_id,
            status: PURCHASE_PENDING_PAYMENT.to_string(),
            subdomain,
            subdomain_pending: false,
            order_id: order_id.clone(),
            subscription_id: subscription_id.clone(),
            product_id: package.product_id.clone(),
            enabled_modules: Vec::new(),
            grace_ends_at: None,
            cancelled_at: None,
            cancel_reason: None,
            provision_attempts: 0,
            created_at: now,
            updated_at: now,
        };
        state.purchases.insert(purchase.id, purchase.clone());
        state.record_history(
            purchase.id,
            "purchase.requested",
            &caller.actor,
            json!({
                "package_id": package.id,
                "package_version": package.current_version,
                "company_id": company_id,
            }),
        );
        // The store owns checkout: this only requests the purchase.
        // Provisioning starts when the store reports the order paid.
        ctx.hooks.emit(
            EVENT_PURCHASE_REQUESTED,
            json!({
                "purchase_id": purchase.id,
                "package_id": package.id,
                "package_version": package.current_version,
                "product_id": package.product_id,
                "buyer_id": buyer_id,
                "company_id": company_id,
                "tenant_schema": tenant_schema(&company_id),
                "module": "saas_packages",
                "item_type": "saas_package",
                "order_id": order_id,
                "subscription_id": subscription_id,
            }),
        );
        Ok(purchase)
    })();
    match result {
        Ok(purchase) => created(purchase),
        Err(error) => err(error, debug),
    }
}

fn purchase_visible(ctx: &AppContext, caller: &Caller, purchase: &PackagePurchase) -> bool {
    if check_capability(ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE) {
        return true;
    }
    purchase.buyer_id == caller.actor
}

async fn list_purchases(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let params = query.into_inner();
        let package_filter = params.filters.get("package_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        if let Some(company_id) = &company_filter {
            require_tenant_scope(&req, &parse_uuid(company_id, "company")?)?;
        }
        let buyer_filter = params.filters.get("buyer_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let is_manager = check_capability(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE);
        let mut items: Vec<PackagePurchase> = state
            .purchases
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|purchase| {
                if !is_manager && purchase.buyer_id != caller.actor {
                    return false;
                }
                if let Some(package_id) = &package_filter {
                    if purchase.package_id.to_string() != *package_id {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if purchase.company_id.to_string() != *company_id {
                        return false;
                    }
                }
                if let Some(buyer_id) = &buyer_filter {
                    if &purchase.buyer_id != buyer_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &purchase.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_purchase(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let purchase = load_purchase(&state, &path.into_inner())?;
        if !purchase_visible(&ctx, &caller, &purchase) {
            return Err(UnifiedError::not_found("Purchase not found"));
        }
        require_tenant_scope(&req, &purchase.company_id)?;
        let grace_expired = purchase
            .grace_ends_at
            .map(|ends| Utc::now() > ends)
            .unwrap_or(false);
        Ok(json!({
            "purchase": purchase,
            "grace_expired": grace_expired,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_tenant_purchases(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let company_id = parse_uuid(&path.into_inner(), "company")?;
        require_tenant_scope(&req, &company_id)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let is_manager = check_capability(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE);
        let mut items: Vec<PackagePurchase> = state
            .purchases
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|purchase| {
                if purchase.company_id != company_id {
                    return false;
                }
                if !is_manager && purchase.buyer_id != caller.actor {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &purchase.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        // A non-manager with no purchase of their own for this tenant
        // sees nothing (the filter above already enforces it); an
        // empty list is the answer, never another tenant's rows.
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_purchase_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let purchase = load_purchase(&state, &path.into_inner())?;
        if !purchase_visible(&ctx, &caller, &purchase) {
            return Err(UnifiedError::not_found("Purchase not found"));
        }
        require_tenant_scope(&req, &purchase.company_id)?;
        let params = query.into_inner();
        let action_filter = params.filters.get("action").cloned();
        let mut items: Vec<PurchaseHistoryEntry> = state
            .history
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.purchase_id == purchase.id)
            .filter(|entry| {
                if let Some(action) = &action_filter {
                    entry.action == *action
                } else {
                    true
                }
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn start_provisioning(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProvisionSummary> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_PROVISION)?;
        let purchase = load_purchase(&state, &path.into_inner())?;
        require_tenant_scope(&req, &purchase.company_id)?;
        match purchase.status.as_str() {
            PURCHASE_PENDING_PAYMENT | PURCHASE_PROVISIONING | PURCHASE_FAILED => {}
            PURCHASE_ACTIVE => {
                return Err(UnifiedError::conflict(
                    "The purchase is already provisioned and active",
                ));
            }
            other => {
                return Err(UnifiedError::conflict(format!(
                    "A purchase in status \"{other}\" cannot be provisioned"
                )));
            }
        }
        run_provisioning(&state, purchase.id, &caller.actor, ProvisionMode::Full, Some(&ctx))
    })();
    match result {
        Ok(summary) => ok(summary),
        Err(error) => err(error, debug),
    }
}

async fn retry_provisioning(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProvisionSummary> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_PROVISION)?;
        let purchase = load_purchase(&state, &path.into_inner())?;
        require_tenant_scope(&req, &purchase.company_id)?;
        match purchase.status.as_str() {
            PURCHASE_PROVISIONING | PURCHASE_FAILED => {}
            PURCHASE_ACTIVE => {
                return Err(UnifiedError::conflict(
                    "The purchase is already provisioned and active; nothing to retry",
                ));
            }
            other => {
                return Err(UnifiedError::conflict(format!(
                    "A purchase in status \"{other}\" has no failed provisioning steps to retry"
                )));
            }
        }
        run_provisioning(&state, purchase.id, &caller.actor, ProvisionMode::RetryFailed, Some(&ctx))
    })();
    match result {
        Ok(summary) => ok(summary),
        Err(error) => err(error, debug),
    }
}

async fn get_provisioning(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProvisionSummary> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let purchase = load_purchase(&state, &path.into_inner())?;
        require_tenant_scope(&req, &purchase.company_id)?;
        if !purchase_visible(&ctx, &caller, &purchase) {
            return Err(UnifiedError::not_found("Purchase not found"));
        }
        Ok(build_summary(&state, &purchase))
    })();
    match result {
        Ok(summary) => ok(summary),
        Err(error) => err(error, debug),
    }
}

async fn list_provision_steps(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let purchase = load_purchase(&state, &path.into_inner())?;
        require_tenant_scope(&req, &purchase.company_id)?;
        if !purchase_visible(&ctx, &caller, &purchase) {
            return Err(UnifiedError::not_found("Purchase not found"));
        }
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let step_filter = params.filters.get("step").cloned();
        let mut items = steps_for_purchase(&state, purchase.id);
        if let Some(status) = &status_filter {
            items.retain(|step| &step.status == status);
        }
        if let Some(step_name) = &step_filter {
            items.retain(|step| &step.step == step_name);
        }
        items.reverse();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_provision_step(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProvisionStep> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "provision step")?;
        let step = state
            .steps
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Provision step not found"))?;
        let purchase = load_purchase(&state, &step.purchase_id.to_string())?;
        require_tenant_scope(&req, &purchase.company_id)?;
        if !purchase_visible(&ctx, &caller, &purchase) {
            return Err(UnifiedError::not_found("Provision step not found"));
        }
        Ok(step)
    })();
    match result {
        Ok(step) => ok(step),
        Err(error) => err(error, debug),
    }
}

async fn cancel_purchase(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CancelPurchaseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PackagePurchase> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
        let mut purchase = load_purchase(&state, &path.into_inner())?;
        require_tenant_scope(&req, &purchase.company_id)?;
        match purchase.status.as_str() {
            PURCHASE_ACTIVE | PURCHASE_PROVISIONING | PURCHASE_PENDING_PAYMENT | PURCHASE_SUSPENDED => {}
            PURCHASE_GRACE_PERIOD => {
                return Err(UnifiedError::conflict(
                    "The purchase is already in its cancellation grace period",
                ));
            }
            other => {
                return Err(UnifiedError::conflict(format!(
                    "A purchase in status \"{other}\" cannot be cancelled"
                )));
            }
        }
        let input = body.into_inner();
        let reason = optional_text(&input.reason, "reason", MAX_TEXT_LEN)?;
        let settings = state.settings();
        let now = Utc::now();
        purchase.status = if settings.grace_period_days > 0 {
            PURCHASE_GRACE_PERIOD.to_string()
        } else {
            PURCHASE_CANCELLED.to_string()
        };
        purchase.grace_ends_at = if settings.grace_period_days > 0 {
            Some(now + chrono::Duration::days(settings.grace_period_days))
        } else {
            None
        };
        purchase.cancelled_at = Some(now);
        purchase.cancel_reason = reason.clone();
        purchase.updated_at = now;
        state.purchases.insert(purchase.id, purchase.clone());
        state.record_history(
            purchase.id,
            "purchase.cancelled",
            &caller.actor,
            json!({
                "reason": reason.clone(),
                "grace_ends_at": purchase.grace_ends_at,
            }),
        );
        ctx.hooks.emit(
            EVENT_CANCELLED,
            json!({
                "purchase_id": purchase.id,
                "package_id": purchase.package_id,
                "company_id": purchase.company_id,
                "actor_id": caller.actor,
                "reason": reason.clone(),
                "grace_ends_at": purchase.grace_ends_at,
            }),
        );
        // The billing module owns the subscription itself: ask it to
        // cancel the backing subscription (handoff, events only).
        if let Some(subscription_id) = &purchase.subscription_id {
            ctx.hooks.emit(
                "billing.subscription.cancel_requested",
                json!({
                    "subscription_id": subscription_id,
                    "purchase_id": purchase.id,
                    "company_id": purchase.company_id,
                    "actor_id": caller.actor,
                    "reason": reason,
                }),
            );
        }
        Ok(purchase)
    })();
    match result {
        Ok(purchase) => ok(purchase),
        Err(error) => err(error, debug),
    }
}

async fn renew_purchase(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PackagePurchase> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_PURCHASE)?;
        let purchase = load_purchase(&state, &path.into_inner())?;
        require_tenant_scope(&req, &purchase.company_id)?;
        if !purchase_visible(&ctx, &caller, &purchase)
            && !check_capability(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)
        {
            return Err(UnifiedError::not_found("Purchase not found"));
        }
        match purchase.status.as_str() {
            PURCHASE_ACTIVE | PURCHASE_GRACE_PERIOD | PURCHASE_EXPIRED => {}
            other => {
                return Err(UnifiedError::conflict(format!(
                    "A purchase in status \"{other}\" cannot be renewed"
                )));
            }
        }
        state.record_history(
            purchase.id,
            "purchase.renewal_requested",
            &caller.actor,
            json!({ "subscription_id": purchase.subscription_id }),
        );
        ctx.hooks.emit(
            EVENT_RENEWAL_REQUESTED,
            json!({
                "purchase_id": purchase.id,
                "package_id": purchase.package_id,
                "company_id": purchase.company_id,
                "actor_id": caller.actor,
                "subscription_id": purchase.subscription_id,
            }),
        );
        if let Some(subscription_id) = &purchase.subscription_id {
            ctx.hooks.emit(
                "billing.subscription.renew_requested",
                json!({
                    "subscription_id": subscription_id,
                    "purchase_id": purchase.id,
                    "company_id": purchase.company_id,
                    "actor_id": caller.actor,
                }),
            );
        }
        Ok(purchase)
    })();
    match result {
        Ok(purchase) => ok(purchase),
        Err(error) => err(error, debug),
    }
}

/// Shared upgrade/downgrade implementation: move a purchase to
/// another published package, enabling new modules and reporting
/// dropped modules as kept unless the caller explicitly confirms
/// disabling them. A module the tenant uses outside this package
/// (shared with another purchase) is NEVER disabled here.
fn change_package(
    ctx: &AppContext,
    state: &SaasPackagesState,
    req: &HttpRequest,
    caller: &Caller,
    purchase_id: &str,
    input: ChangePackageRequest,
    upgrade: bool,
) -> UnifiedResult<Value> {
    require_permission(ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
    let mut purchase = load_purchase(state, purchase_id)?;
    require_tenant_scope(req, &purchase.company_id)?;
    if purchase.status != PURCHASE_ACTIVE && purchase.status != PURCHASE_GRACE_PERIOD {
        return Err(UnifiedError::conflict(format!(
            "Only an active purchase can change package (current status: {})",
            purchase.status
        )));
    }
    let target_id = parse_uuid(&input.target_package_id, "target package")?;
    let target = state
        .packages
        .get(&target_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| {
            UnifiedError::not_found(format!(
                "Target package \"{}\" not found",
                input.target_package_id
            ))
        })?;
    if target.status != STATUS_PUBLISHED {
        return Err(UnifiedError::conflict(format!(
            "Only a published package can be moved to (target status: {})",
            target.status
        )));
    }
    if target.id == purchase.package_id {
        return Err(UnifiedError::conflict(
            "The purchase already uses this package",
        ));
    }
    let current_version = purchase_version(state, &purchase)?;
    let mut diff = module_diff(&current_version.modules, &target.modules);
    diff.from_package_id = purchase.package_id;
    diff.to_package_id = target.id;

    // Enable the new modules first (entitlement blocks the move
    // with a 403-style error naming the module — never silently
    // skipped). Already-enabled modules are reported, not touched.
    let mut enabled: Vec<String> = Vec::new();
    let mut already_enabled: Vec<String> = Vec::new();
    for module_id in &diff.to_enable {
        if state.tenant_has_module(&purchase.company_id, module_id) {
            already_enabled.push(module_id.clone());
            continue;
        }
        if ctx.modules.get(module_id).is_none() {
            return Err(UnifiedError::not_found(format!(
                "Module \"{module_id}\" not found"
            )));
        }
        match entitlement_available_http(ctx, &purchase.company_id, module_id) {
            Ok(true) => {}
            Ok(false) => {
                return Err(UnifiedError::forbidden(format!(
                    "Tenant \"{}\" is not entitled to module \"{module_id}\": package change blocked (the module requires an active license entitlement)",
                    purchase.company_id
                )));
            }
            Err(error) => return Err(error),
        }
        state.mark_tenant_module(&purchase.company_id, module_id, purchase.id);
        ctx.hooks.emit(
            events::MODULE_ENABLED,
            json!({
                "id": module_id,
                "company_id": purchase.company_id,
                "package_id": target.id,
                "purchase_id": purchase.id,
                "actor_id": caller.actor,
            }),
        );
        ctx.hooks.emit(
            EVENT_MODULE_ENABLED,
            json!({
                "package_id": target.id,
                "purchase_id": purchase.id,
                "company_id": purchase.company_id,
                "module_id": module_id,
                "actor_id": caller.actor,
                "auto_activated": [],
            }),
        );
        enabled.push(module_id.clone());
    }

    // Dropped modules: shared ones are always kept; the rest are
    // kept too unless the caller explicitly confirmed disabling
    // them by listing them with `confirm_disable`.
    let mut disabled: Vec<String> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    let mut kept_shared: Vec<String> = Vec::new();
    for module_id in &diff.to_disable {
        let shared_owners = state.other_module_owners(&purchase.company_id, module_id, purchase.id);
        if !shared_owners.is_empty() {
            kept.push(module_id.clone());
            kept_shared.push(module_id.clone());
            continue;
        }
        if input.confirm_disable && input.modules_to_disable.contains(module_id) {
            state.unmark_tenant_module(&purchase.company_id, module_id, purchase.id);
            ctx.hooks.emit(
                events::MODULE_DISABLED,
                json!({
                    "id": module_id,
                    "company_id": purchase.company_id,
                    "package_id": purchase.package_id,
                    "purchase_id": purchase.id,
                    "actor_id": caller.actor,
                }),
            );
            ctx.hooks.emit(
                EVENT_MODULE_DISABLED,
                json!({
                    "package_id": purchase.package_id,
                    "purchase_id": purchase.id,
                    "company_id": purchase.company_id,
                    "module_id": module_id,
                    "actor_id": caller.actor,
                }),
            );
            disabled.push(module_id.clone());
        } else {
            kept.push(module_id.clone());
        }
    }
    diff.kept_shared = kept_shared;

    let from_package_id = purchase.package_id;
    let from_version = purchase.package_version;
    purchase.package_id = target.id;
    purchase.package_version = target.current_version;
    purchase.product_id = target.product_id.clone();
    purchase.enabled_modules = target
        .modules
        .iter()
        .map(|entry| entry.module_id.clone())
        .filter(|module_id| {
            state.tenant_has_module(&purchase.company_id, module_id)
                || enabled.contains(module_id)
        })
        .collect();
    purchase.updated_at = now();
    state.purchases.insert(purchase.id, purchase.clone());
    let action = if upgrade { "purchase.upgraded" } else { "purchase.downgraded" };
    let event = if upgrade { EVENT_UPGRADED } else { EVENT_DOWNGRADED };
    state.record_history(
        purchase.id,
        action,
        &caller.actor,
        json!({
            "from_package_id": from_package_id,
            "from_package_version": from_version,
            "to_package_id": target.id,
            "to_package_version": target.current_version,
            "enabled": enabled,
            "already_enabled": already_enabled,
            "disabled": disabled,
            "kept": kept,
        }),
    );
    emit_audit(
        ctx,
        event,
        &caller.actor,
        action,
        "saas_package_purchase",
        &purchase.id.to_string(),
        Some(purchase.company_id),
        json!({
            "from_package_id": from_package_id,
            "to_package_id": target.id,
            "enabled": enabled,
            "disabled": disabled,
            "kept": kept,
        }),
    );
    Ok(json!({
        "purchase": purchase,
        "diff": diff,
        "enabled": enabled,
        "already_enabled": already_enabled,
        "disabled": disabled,
        "kept": kept,
    }))
}

async fn upgrade_purchase(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ChangePackageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| {
        let caller = caller(&req)?;
        change_package(&ctx, &state, &req, &caller, &path.into_inner(), body.into_inner(), true)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn downgrade_purchase(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ChangePackageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| {
        let caller = caller(&req)?;
        change_package(&ctx, &state, &req, &caller, &path.into_inner(), body.into_inner(), false)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers — AI and settings.
// ---------------------------------------------------------------------------

async fn ai_recommend_package(
    ctx: web::Data<Arc<AppContext>>,
    _state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    body: web::Json<AiRecommendRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW) {
        return err(error, debug);
    }
    let input = body.into_inner();
    let company_id = match &input.company_id {
        Some(raw) => match parse_uuid(raw, "company") {
            Ok(id) => Some(id),
            Err(error) => return err(error, debug),
        },
        None => None,
    };
    if let Some(company_id) = &company_id {
        if let Err(error) = require_tenant_scope(&req, company_id) {
            return err(error, debug);
        }
    }
    // Grounding context for the AI call is contributed by this
    // module's own hook handler; the helper runs the AI gate.
    // Recommendations are suggestions only: nothing is purchased
    // or provisioned from here.
    let ai_input = json!({
        "business_type": input.business_type,
        "needs": input.needs.unwrap_or_default(),
    });
    // The AI module's Caller is the type `run_ai_capability`
    // takes; it carries the same actor and role as this module's
    // local caller.
    let ai_caller = crate::modules::ai::Caller {
        actor: caller.actor.clone(),
        role: caller.role.clone(),
    };
    match run_ai_capability(&ctx, &ai_caller, company_id, "saas_packages", "recommend_package", ai_input).await {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn ai_describe_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    body: web::Json<AiDescribeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW) {
        return err(error, debug);
    }
    let input = body.into_inner();
    let company_id = match &input.company_id {
        Some(raw) => match parse_uuid(raw, "company") {
            Ok(id) => Some(id),
            Err(error) => return err(error, debug),
        },
        None => None,
    };
    if let Some(company_id) = &company_id {
        if let Err(error) = require_tenant_scope(&req, company_id) {
            return err(error, debug);
        }
    }
    // The package must exist before the AI is asked to describe it;
    // the description itself comes from the AI helper.
    if let Err(error) = load_package(&state, &input.package_id) {
        return err(error, debug);
    }
    let ai_input = json!({ "package_id": input.package_id });
    // The AI module's Caller is the type `run_ai_capability`
    // takes; it carries the same actor and role as this module's
    // local caller.
    let ai_caller = crate::modules::ai::Caller {
        actor: caller.actor.clone(),
        role: caller.role.clone(),
    };
    match run_ai_capability(&ctx, &ai_caller, company_id, "saas_packages", "describe_package", ai_input).await {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_ai_capabilities(
    ctx: web::Data<Arc<AppContext>>,
    _state: web::Data<SaasPackagesState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        // Built from the capability fields directly, so this
        // endpoint does not depend on the AI struct's Serialize.
        let capabilities: Vec<Value> = AI_CAPABILITIES
            .iter()
            .map(|capability| {
                json!({
                    "id": capability.id,
                    "module_id": capability.module_id,
                    "name": capability.name,
                    "description": capability.description,
                    "input_fields": capability.input_fields,
                    "allows_pii": capability.allows_pii,
                    "use_case": capability.use_case,
                })
            })
            .collect();
        Ok(json!({ "capabilities": capabilities }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasPackagesSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        require_control_plane(&req)?;
        Ok(state.settings())
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    body: web::Json<SettingsUpdateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasPackagesSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
        require_control_plane(&req)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(value) = input.grace_period_days {
            settings.grace_period_days = value;
        }
        if let Some(value) = input.max_modules_per_package {
            settings.max_modules_per_package = value;
        }
        if let Some(value) = input.max_wizard_steps {
            settings.max_wizard_steps = value;
        }
        if let Some(value) = input.provisioning_max_attempts {
            settings.provisioning_max_attempts = value;
        }
        if let Some(value) = input.subdomain_base_domain {
            settings.subdomain_base_domain = value.trim().to_string();
        }
        if let Some(value) = input.vendor_packages_enabled {
            settings.vendor_packages_enabled = value;
        }
        if let Some(value) = input.wizard_step_names {
            settings.wizard_step_names = value;
        }
        settings.validate()?;
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        emit_audit(
            &ctx,
            "saas_packages.settings.updated",
            &caller.actor,
            "saas_packages.settings.updated",
            "saas_packages_settings",
            "settings",
            None,
            json!({
                "grace_period_days": settings.grace_period_days,
                "max_modules_per_package": settings.max_modules_per_package,
            }),
        );
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Hook handlers (event-driven; all read defensively — a missing
// field or an unknown purchase is skipped, never an error).
// ---------------------------------------------------------------------------

fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|value| value.as_str())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Find the purchase an order/subscription event names: a direct
/// `purchase_id`, an `order_id`/`subscription_id` matching a stored
/// purchase, or an `items` entry carrying a `purchase_id`.
fn purchase_for_event(state: &SaasPackagesState, data: &Value) -> Option<PackagePurchase> {
    if let Some(purchase_id) = opt_string(data, "purchase_id") {
        if let Ok(purchase) = load_purchase(state, &purchase_id) {
            return Some(purchase);
        }
    }
    if let Some(order_id) = opt_string(data, "order_id") {
        for entry in state.purchases.iter() {
            let purchase = entry.value();
            if purchase.order_id.as_deref() == Some(order_id.as_str()) {
                return Some(purchase.clone());
            }
        }
    }
    if let Some(order_id) = opt_string(data, "id") {
        for entry in state.purchases.iter() {
            let purchase = entry.value();
            if purchase.order_id.as_deref() == Some(order_id.as_str()) {
                return Some(purchase.clone());
            }
        }
    }
    if let Some(subscription_id) = opt_string(data, "subscription_id") {
        for entry in state.purchases.iter() {
            let purchase = entry.value();
            if purchase.subscription_id.as_deref() == Some(subscription_id.as_str()) {
                return Some(purchase.clone());
            }
        }
    }
    if let Some(items) = data.get("items").and_then(|value| value.as_array()) {
        for item in items {
            if let Some(purchase_id) = opt_string(item, "purchase_id") {
                if let Ok(purchase) = load_purchase(state, &purchase_id) {
                    return Some(purchase);
                }
            }
        }
    }
    None
}

fn on_order_paid(state: SaasPackagesState, ctx: &mut HookContext) {
    let Some(purchase) = purchase_for_event(&state, &ctx.data.clone()) else {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "order event names no SaaS package purchase" }));
        return;
    };
    if purchase.status == PURCHASE_ACTIVE {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "purchase is already provisioned and active", "purchase_id": purchase.id }));
        return;
    }
    if purchase.status != PURCHASE_PENDING_PAYMENT
        && purchase.status != PURCHASE_PROVISIONING
        && purchase.status != PURCHASE_FAILED
    {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": format!("purchase in status \"{}\" cannot be provisioned", purchase.status), "purchase_id": purchase.id }));
        return;
    }
    // The store reported the order paid: link the order and start
    // provisioning on the hook path (no AppContext here — see the
    // module header OPEN notes for the entitlement caveat).
    let mut updated = purchase.clone();
    if updated.order_id.is_none() {
        updated.order_id = opt_string(&ctx.data, "order_id").or_else(|| opt_string(&ctx.data, "id"));
    }
    if updated.subscription_id.is_none() {
        updated.subscription_id = opt_string(&ctx.data, "subscription_id");
    }
    updated.updated_at = now();
    state.purchases.insert(updated.id, updated);
    match run_provisioning(&state, purchase.id, "system", ProvisionMode::Full, None) {
        Ok(summary) => {
            ctx.add_result(json!({ "saas_packages": "provisioning_started", "purchase_id": purchase.id, "status": summary.status, "attempt": summary.attempt }));
        }
        Err(error) => {
            ctx.add_result(json!({ "saas_packages": "provisioning_error", "purchase_id": purchase.id, "reason": error.message }));
        }
    }
}

fn on_order_refunded_or_cancelled(state: SaasPackagesState, ctx: &mut HookContext) {
    let Some(purchase) = purchase_for_event(&state, &ctx.data.clone()) else {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "order event names no SaaS package purchase" }));
        return;
    };
    if purchase.status != PURCHASE_ACTIVE && purchase.status != PURCHASE_GRACE_PERIOD {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": format!("purchase in status \"{}\" is not active", purchase.status), "purchase_id": purchase.id }));
        return;
    }
    let mut updated = purchase.clone();
    updated.status = PURCHASE_SUSPENDED.to_string();
    updated.updated_at = now();
    state.purchases.insert(updated.id, updated.clone());
    state.record_history(
        updated.id,
        "purchase.suspended",
        "system",
        json!({ "reason": "order refunded or cancelled" }),
    );
    ctx.add_result(json!({ "saas_packages": "suspended", "purchase_id": updated.id }));
}

fn on_subscription_activated(state: SaasPackagesState, ctx: &mut HookContext) {
    let Some(purchase) = purchase_for_event(&state, &ctx.data.clone()) else {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "subscription event names no SaaS package purchase" }));
        return;
    };
    if purchase.status == PURCHASE_ACTIVE {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "purchase is already provisioned and active", "purchase_id": purchase.id }));
        return;
    }
    if purchase.status != PURCHASE_PENDING_PAYMENT
        && purchase.status != PURCHASE_PROVISIONING
        && purchase.status != PURCHASE_FAILED
    {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": format!("purchase in status \"{}\" cannot be provisioned", purchase.status), "purchase_id": purchase.id }));
        return;
    }
    let mut updated = purchase.clone();
    if updated.subscription_id.is_none() {
        updated.subscription_id = opt_string(&ctx.data, "subscription_id");
    }
    updated.updated_at = now();
    state.purchases.insert(updated.id, updated);
    match run_provisioning(&state, purchase.id, "system", ProvisionMode::Full, None) {
        Ok(summary) => {
            ctx.add_result(json!({ "saas_packages": "provisioning_started", "purchase_id": purchase.id, "status": summary.status, "attempt": summary.attempt }));
        }
        Err(error) => {
            ctx.add_result(json!({ "saas_packages": "provisioning_error", "purchase_id": purchase.id, "reason": error.message }));
        }
    }
}

fn on_subscription_cancelled(state: SaasPackagesState, ctx: &mut HookContext) {
    let Some(purchase) = purchase_for_event(&state, &ctx.data.clone()) else {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "subscription event names no SaaS package purchase" }));
        return;
    };
    if purchase.status != PURCHASE_ACTIVE {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": format!("purchase in status \"{}\" is not active", purchase.status), "purchase_id": purchase.id }));
        return;
    }
    let settings = state.settings();
    let now = Utc::now();
    let mut updated = purchase.clone();
    updated.status = if settings.grace_period_days > 0 {
        PURCHASE_GRACE_PERIOD.to_string()
    } else {
        PURCHASE_CANCELLED.to_string()
    };
    updated.grace_ends_at = if settings.grace_period_days > 0 {
        Some(now + chrono::Duration::days(settings.grace_period_days))
    } else {
        None
    };
    updated.cancelled_at = Some(now);
    updated.cancel_reason = Some("Subscription cancelled".to_string());
    updated.updated_at = now;
    state.purchases.insert(updated.id, updated.clone());
    state.record_history(
        updated.id,
        "purchase.cancelled",
        "system",
        json!({ "reason": "subscription cancelled", "grace_ends_at": updated.grace_ends_at }),
    );
    emit_audit_state(
        &state,
        EVENT_CANCELLED,
        "system",
        "saas_packages.cancelled",
        "saas_package_purchase",
        &updated.id.to_string(),
        Some(updated.company_id),
        json!({ "reason": "subscription cancelled", "grace_ends_at": updated.grace_ends_at }),
    );
    ctx.add_result(json!({ "saas_packages": "grace_period", "purchase_id": updated.id, "grace_ends_at": updated.grace_ends_at }));
}

fn on_subscription_expired(state: SaasPackagesState, ctx: &mut HookContext) {
    let Some(purchase) = purchase_for_event(&state, &ctx.data.clone()) else {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "subscription event names no SaaS package purchase" }));
        return;
    };
    if purchase.status != PURCHASE_ACTIVE && purchase.status != PURCHASE_GRACE_PERIOD {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": format!("purchase in status \"{}\" is not active", purchase.status), "purchase_id": purchase.id }));
        return;
    }
    let mut updated = purchase.clone();
    updated.status = PURCHASE_EXPIRED.to_string();
    updated.updated_at = now();
    state.purchases.insert(updated.id, updated.clone());
    state.record_history(
        updated.id,
        "purchase.expired",
        "system",
        json!({ "reason": "subscription expired" }),
    );
    ctx.add_result(json!({ "saas_packages": "expired", "purchase_id": updated.id }));
}

/// AI grounding for package recommendations: the published
/// packages with their module counts. Never buyer personal data.
fn on_ai_recommend_requested(state: SaasPackagesState, ctx: &mut HookContext) {
    let packages: Vec<Value> = state
        .published_packages()
        .iter()
        .map(|package| {
            json!({
                "package_id": package.id,
                "slug": package.slug,
                "name": package.name,
                "description": package.description,
                "module_count": package.modules.len(),
                "modules": package.modules.iter().map(|entry| entry.module_id.clone()).collect::<Vec<String>>(),
                "version": package.current_version,
            })
        })
        .collect();
    ctx.add_result(json!({
        "saas_packages": "recommend_grounding",
        "packages": packages,
    }));
}

/// AI grounding for describing one package: its module list and a
/// settings summary. Never buyer personal data.
fn on_ai_describe_requested(state: SaasPackagesState, ctx: &mut HookContext) {
    let Some(raw_id) = opt_string(&ctx.data, "package_id") else {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "describe request carries no package_id" }));
        return;
    };
    let Ok(package) = load_package(&state, &raw_id) else {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "no package matches the requested package_id" }));
        return;
    };
    let settings_summary = match &package.preset_settings {
        Value::Object(map) => json!(map.keys().cloned().collect::<Vec<String>>()),
        _ => json!([]),
    };
    ctx.add_result(json!({
        "saas_packages": "describe_grounding",
        "package_id": package.id,
        "slug": package.slug,
        "name": package.name,
        "description": package.description,
        "status": package.status,
        "version": package.current_version,
        "modules": package.modules.iter().map(|entry| json!({ "module_id": entry.module_id, "required": entry.required })).collect::<Vec<Value>>(),
        "wizard_steps": package.wizard_steps,
        "settings_keys": settings_summary,
    }));
}

// ---------------------------------------------------------------------------
// Module implementation.
// ---------------------------------------------------------------------------

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
        // Seed the example package as data, once. All runtime
        // behaviour reads the store from here on; the seed is never
        // consulted again.
        if self.state.packages.is_empty() {
            let seed = seed_online_exam_package();
            self.state.store_version(&seed);
            self.state.packages.insert(seed.id, seed.clone());
            log::info!("SaaS Packages module seeded example package \"online-exam-saas\"");
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
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/ai/capabilities", owner: "saas_packages", description: "List the AI capabilities this module offers (view)." },
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/ai/recommend-package", owner: "saas_packages", description: "Ask the AI to recommend a package for a business type and needs (view; suggestions only, never purchases or provisions)." },
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/ai/describe-package", owner: "saas_packages", description: "Ask the AI to describe one package (view)." },
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/settings", owner: "saas_packages", description: "Get the configurable SaaS Packages settings (view)." },
            RouteInfo { method: "PUT", path: "/api/v1/saas-packages/settings", owner: "saas_packages", description: "Update the configurable SaaS Packages settings (manage)." },
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/packages", owner: "saas_packages", description: "List packages (paginated, filter by status or vendor_id, search; non-managers see published only)." },
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/packages", owner: "saas_packages", description: "Create a package in draft (manage; module ids validated against the registry)." },
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/packages/{id}/versions", owner: "saas_packages", description: "List version snapshots of a package (paginated, newest first)." },
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/packages/{id}/versions/{version}", owner: "saas_packages", description: "Get one frozen version snapshot of a package." },
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/packages/{id}/validate", owner: "saas_packages", description: "Validate a package for a tenant: module registration, entitlement, and dependency order preview." },
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/packages/{id}/modules", owner: "saas_packages", description: "List the modules of a package's current version with registry info (paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/packages/{id}/diff/{other_id}", owner: "saas_packages", description: "Module diff between two packages: to enable, to disable, shared." },
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/packages/{id}/publish", owner: "saas_packages", description: "Publish a draft package (manage; requires at least one module)." },
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/packages/{id}/archive", owner: "saas_packages", description: "Archive a published or draft package (manage)." },
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/packages/{id}/clone", owner: "saas_packages", description: "Clone a package as a new draft (manage)." },
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/packages/{id}/purchase", owner: "saas_packages", description: "Buy a published package: creates a pending purchase and emits a purchase request for the store (checkout lives in the store)." },
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/packages/{id}", owner: "saas_packages", description: "Get one package by id or slug (drafts and archived only for managers)." },
            RouteInfo { method: "PATCH", path: "/api/v1/saas-packages/packages/{id}", owner: "saas_packages", description: "Edit a package (manage; editing a published package creates a new version, purchases keep theirs)." },
            RouteInfo { method: "DELETE", path: "/api/v1/saas-packages/packages/{id}", owner: "saas_packages", description: "Delete a package with no purchases (manage; published packages must be archived first)." },
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/purchases", owner: "saas_packages", description: "List purchases (paginated, filter by package_id, company_id, buyer_id, status; non-managers see only their own)." },
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/purchases/{id}", owner: "saas_packages", description: "Get one purchase with its computed grace-expired flag (own or manager)." },
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/purchases/{id}/cancel", owner: "saas_packages", description: "Cancel a purchase with a grace period (manage; hands the subscription cancellation to billing)." },
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/purchases/{id}/provision", owner: "saas_packages", description: "Start provisioning a purchase (provision; full run of the six-step pipeline)." },
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/purchases/{id}/provision", owner: "saas_packages", description: "Get the provisioning status and steps of a purchase (own or manager)." },
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/purchases/{id}/provision/retry", owner: "saas_packages", description: "Retry the failed, blocked, or skipped provisioning steps of a purchase (provision)." },
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/purchases/{id}/provision/steps", owner: "saas_packages", description: "List the provisioning steps of a purchase (paginated, filter by status or step)." },
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/purchases/{id}/upgrade", owner: "saas_packages", description: "Move a purchase to another published package (manage; enables new modules, never disables shared modules without explicit confirmation)." },
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/purchases/{id}/downgrade", owner: "saas_packages", description: "Move a purchase to another published package (manage; same diff rules as upgrade)." },
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/purchases/{id}/history", owner: "saas_packages", description: "List the history of a purchase (paginated, filter by action)." },
            RouteInfo { method: "POST", path: "/api/v1/saas-packages/purchases/{id}/renew", owner: "saas_packages", description: "Request renewal of a purchase through billing (purchase; hands the renewal to billing)." },
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/provision-steps/{id}", owner: "saas_packages", description: "Get one provisioning step (own purchase or manager)." },
            RouteInfo { method: "GET", path: "/api/v1/saas-packages/tenants/{company_id}/purchases", owner: "saas_packages", description: "List the purchases of one tenant (paginated, filter by status; non-managers see only their own)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}
