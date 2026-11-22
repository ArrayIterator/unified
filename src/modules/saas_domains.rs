// SaaS Domain Management module — preregister reservations, custom
// domain aliases, tenant subdomain allocation, and tenant resolution.
//
// This module extends the Domains (`domains`) and SaaS (`saas`)
// modules through the hook bus only; it never calls into them
// directly (except for the public `company_exists` helper, the same
// direct helper the Domains module itself uses). It owns three record
// kinds, all kept in DashMap stores that are authoritative at runtime;
// the migration `2026-11-22-000070_saas_domains` creates the matching
// PostgreSQL tables as persistence targets.
//
// MODULARITY RULE (standing architecture rule): nothing in this
// module is hard-coded behaviour. Every list, pattern, quota, and
// base domain that shapes behaviour lives in `SaasDomainsSettings`
// (admin-editable through `GET/PUT /api/v1/saas/domains/settings`,
// config keys under the `saas_domains.*` namespace) with a clearly
// marked factory default that an administrator can override without
// touching code. That covers: the wildcard base domain
// (`saas_domains.wildcard_base`), the main-site host
// (`saas_domains.main_host`), the reserved subdomain labels
// (`saas_domains.reserved_labels`), the allowed label pattern
// (`saas_domains.label_pattern`), the per-tenant subdomain quota
// (`saas_domains.max_subdomains_per_tenant`), whether self-assigned
// subdomains need admin approval
// (`saas_domains.require_subdomain_approval`), the default
// reservation lifetime (`saas_domains.default_reservation_ttl_hours`),
// and the allowed alias target types
// (`saas_domains.alias_target_types`). Tenant resolution runs the
// same configurable rules in one place (`resolve_host`): control
// plane first, then tenant subdomain, then verified custom alias,
// then blocked, then no result. The three `blocked` reason codes
// (`unverified_domain`, `suspended`, `released`) are the one
// deliberate exception: they are wire-contract strings the
// tenant-resolution middleware (batch 8, builder B4) matches on
// exactly, so they are stable contract values, not tunables;
// changing them would break the middleware contract.
//
// Product subdomains (batch 15, builder B2) live in this same
// file: each purchased product or package claims ITS OWN subdomain
// (`<label>.<wildcard_base>`), scoped to that product. The surface
// is under `/api/v1/saas/domains/product-subdomains` (see the route
// list below); the record, settings, resolution, and hook contracts
// are documented in `INTEGRATION_NOTES_BATCH15_B2.md` in the staging
// root. A product subdomain resolves through the same
// `tenant.resolve_requested` contract as a generic allocation, with
// kind `subdomain` (the only kind the tenant middleware accepts)
// plus the extra fields `scope: "product"`, `product_id`,
// `package_id`, and `product_subdomain_id`, so the middleware keeps
// working unchanged while downstream handlers can enforce the
// product scope with `require_product_scope`. A request on a product
// subdomain only ever serves that product's module surfaces; the
// middleware contract and the exact integrator steps are in the
// notes file.
//
// Routes (all under `/api/v1/saas/domains`):
//   POST   /product-subdomains                 - claim a product subdomain
//   POST   /product-subdomains/check           - availability check for a label
//   GET    /product-subdomains                 - paginated product subdomains
//   GET    /product-subdomains/{id}            - one product subdomain
//   POST   /product-subdomains/{id}/verify     - mark verified (verification module must confirm first)
//   POST   /product-subdomains/{id}/activate   - activate a verified product subdomain
//   POST   /product-subdomains/{id}/suspend    - suspend a product subdomain
//   POST   /product-subdomains/{id}/release    - start release (grace period first)
//   POST   /product-subdomains/expire          - finalize releases whose grace ended (admin)
//   POST   /reservations                       - reserve a domain
//   GET    /reservations                       - paginated reservations
//   GET    /reservations/{id}                  - one reservation
//   POST   /reservations/{id}/release          - release a reservation
//   POST   /reservations/{id}/provision        - consume a reservation
//   POST   /reservations/expire                - expire stale reservations
//   POST   /aliases                            - create a custom alias
//   GET    /aliases                            - paginated aliases
//   GET    /aliases/{id}                       - one alias
//   PUT    /aliases/{id}                       - update an alias
//   POST   /aliases/{id}/suspend               - suspend an alias
//   POST   /aliases/{id}/release               - release an alias
//   DELETE /aliases/{id}                       - delete an alias
//   POST   /subdomains                         - allocate a subdomain
//   GET    /subdomains                         - paginated allocations
//   GET    /subdomains/{id}                    - one allocation
//   POST   /subdomains/{id}/approve            - approve a pending allocation
//   POST   /subdomains/{id}/reject             - reject a pending allocation
//   POST   /subdomains/{id}/suspend            - suspend an allocation
//   POST   /subdomains/{id}/release            - release an allocation
//   DELETE /subdomains/{id}                    - delete an allocation
//   GET    /resolve?host=...                   - resolve-debug (admin only)
//   GET    /settings                           - read module settings
//   PUT    /settings                           - update module settings
//
// Capability model (same shape as the Properties module): the caller
// is identified by the `X-User-Id` header (required) and `X-Role`
// carries the role slug. Capability checks use the `role.check`
// hook and fail closed (no answer means not allowed). Admin work
// needs `saas_domains.manage`; reading needs `saas_domains.read`;
// a tenant acting on its own records needs
// `saas_domains.self_assign` plus an `X-Company-Id` header that
// matches the record's company. A caller without the manage
// capability can never see or touch another tenant's records.
//
// Product-subdomain events emitted on the hook bus:
//   - `saas.product_subdomain.claimed`, `.verified`, `.activated`,
//     `.suspended`, `.release_started`, `.released`,
//     `.resolve_scoped`
// Events this module answers for product subdomains:
//   - `saas_packages.purchase.completed` — claim intent carrying an
//     explicit entitlement attestation (see the handler note)
//   - `saas_packages.cancelled` — starts release (grace) for the
//     package's product subdomains
//   - `license.revoked` / `module.entitlement.denied` — suspends the
//     affected product subdomains
//   - `domain.verification.completed` — flips a matching product
//     subdomain to verified (same event as the alias path)
//   - `site.provision_requested` — claim intent from the packages
//     provisioning flow (the packages module validates entitlements
//     before emitting it; see the handler note) and from the
//     tenants provisioning flow (source `tenants`: a generic
//     tenant-level allocation, no product id; see the handler note)
//   - `saas_domains.subdomain.release_requested` — tenant destroy
//     revokes every subdomain of the tenant (generic allocations
//     immediately, product subdomains into release grace)
//   - `whitelabel.domain.verified` — attaches a verified custom
//     domain to the product subdomain, inheriting its product scope
//   - `saas_domains.ai.suggest_subdomain_requested` — AI capability
//     hook, answered with availability-checked label suggestions
//   - `saas.product_subdomain.expire_grace` — scheduler sweep that
//     finalizes releases whose grace period ended
//
// Events emitted on the hook bus:
//   - `saas_domains.reservation.created`, `.released`, `.expired`,
//     `.provisioned`
//   - `site.provisioned` (reservation handoff; the SaaS module owns
//     the site row itself)
//   - `saas_domains.alias.created`, `.updated`, `.suspended`,
//     `.released`
//   - `saas_domains.subdomain.created`, `.approved`, `.rejected`,
//     `.suspended`, `.released`
//   - `saas_domains.settings.updated`
// Events this module answers:
//   - `tenant.resolve_requested` — `{"host": ...}`; see `resolve_host`
//   - `saas_domains.alias.lookup` — `{"domain": ...}` or
//     `{"alias_id": ...}`; returns the alias record or no result
//   - `saas_domains.reservations.expire` — scheduler sweep; returns
//     `{"expired": n, "ids": [...]}` and releases past-due
//     reservations
//   - `domain.verification.completed` — flips a matching alias to
//     `verified` (only the verification module may do this; there is
//     deliberately no API endpoint that sets an alias to verified)

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus};
use crate::core::json;
use crate::core::modules::{
    Module, ModuleRegistry, RouteInfo, RouteVersionSupport, MODULE_ENTITLEMENT_DENIED_EVENT,
};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::modules::company::company_exists;
use crate::modules::licensing::check_module_entitlement;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::net::IpAddr;
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Event names.
// ---------------------------------------------------------------------------

pub const EVENT_TENANT_RESOLVE_REQUESTED: &str = "tenant.resolve_requested";
pub const EVENT_ALIAS_LOOKUP: &str = "saas_domains.alias.lookup";
pub const EVENT_RESERVATIONS_EXPIRE: &str = "saas_domains.reservations.expire";
pub const EVENT_DOMAIN_VERIFICATION_COMPLETED: &str = "domain.verification.completed";
pub const EVENT_SITE_PROVISIONED: &str = "site.provisioned";
pub const EVENT_RESERVATION_CREATED: &str = "saas_domains.reservation.created";
pub const EVENT_RESERVATION_RELEASED: &str = "saas_domains.reservation.released";
pub const EVENT_RESERVATION_EXPIRED: &str = "saas_domains.reservation.expired";
pub const EVENT_RESERVATION_PROVISIONED: &str = "saas_domains.reservation.provisioned";
pub const EVENT_ALIAS_CREATED: &str = "saas_domains.alias.created";
pub const EVENT_ALIAS_UPDATED: &str = "saas_domains.alias.updated";
pub const EVENT_ALIAS_SUSPENDED: &str = "saas_domains.alias.suspended";
pub const EVENT_ALIAS_RELEASED: &str = "saas_domains.alias.released";
pub const EVENT_SUBDOMAIN_CREATED: &str = "saas_domains.subdomain.created";
pub const EVENT_SUBDOMAIN_APPROVED: &str = "saas_domains.subdomain.approved";
pub const EVENT_SUBDOMAIN_REJECTED: &str = "saas_domains.subdomain.rejected";
pub const EVENT_SUBDOMAIN_SUSPENDED: &str = "saas_domains.subdomain.suspended";
pub const EVENT_SUBDOMAIN_RELEASED: &str = "saas_domains.subdomain.released";
pub const EVENT_SETTINGS_UPDATED: &str = "saas_domains.settings.updated";
pub const EVENT_PRODUCT_SUBDOMAIN_CLAIMED: &str = "saas.product_subdomain.claimed";
pub const EVENT_PRODUCT_SUBDOMAIN_VERIFIED: &str = "saas.product_subdomain.verified";
pub const EVENT_PRODUCT_SUBDOMAIN_ACTIVATED: &str = "saas.product_subdomain.activated";
pub const EVENT_PRODUCT_SUBDOMAIN_SUSPENDED: &str = "saas.product_subdomain.suspended";
pub const EVENT_PRODUCT_SUBDOMAIN_RELEASE_STARTED: &str = "saas.product_subdomain.release_started";
pub const EVENT_PRODUCT_SUBDOMAIN_RELEASED: &str = "saas.product_subdomain.released";
pub const EVENT_PRODUCT_SUBDOMAIN_RESOLVE_SCOPED: &str = "saas.product_subdomain.resolve_scoped";
pub const EVENT_PRODUCT_SUBDOMAIN_EXPIRE_GRACE: &str = "saas.product_subdomain.expire_grace";
pub const EVENT_AI_SUGGEST_SUBDOMAIN_REQUESTED: &str =
    "saas_domains.ai.suggest_subdomain_requested";
pub const EVENT_PRODUCT_ENTITLEMENT_CHECK: &str = "product.entitlement.check";
pub const EVENT_PACKAGE_ENTITLEMENT_CHECK: &str = "saas_packages.entitlement.check_requested";
pub const EVENT_PACKAGE_PURCHASE_COMPLETED: &str = "saas_packages.purchase.completed";
pub const EVENT_PACKAGE_CANCELLED: &str = "saas_packages.cancelled";
pub const EVENT_SITE_PROVISION_REQUESTED: &str = "site.provision_requested";
pub const EVENT_PACKAGE_MODULES_REQUESTED: &str = "saas_packages.package.modules_requested";
/// Tenant-destroy revoke: emitted by the tenant-destroy flow after
/// the schema drop is verified (`{"tenant_id", "schema",
/// "destroy_request_id", "actor"}`); this module releases every
/// subdomain allocation and product subdomain of the tenant.
/// Same string as `tenant_destroy::EVENT_SUBDOMAIN_RELEASE_REQUESTED`.
pub const EVENT_SUBDOMAIN_RELEASE_REQUESTED: &str = "saas_domains.subdomain.release_requested";
pub const EVENT_WHITELABEL_DOMAIN_VERIFIED: &str = "whitelabel.domain.verified";
/// Status-query event answered by the domain-verification module
/// (`{"domain"}` -> `{"domain", "status", "verified_at"}`). Kept as
/// a string constant (not a module path) because
/// `domain_verification` is not declared in `src/modules/mod.rs`.
pub const EVENT_DOMAIN_VERIFICATION_STATUS_REQUESTED: &str =
    "domain.verification.status_requested";

// Capabilities checked through the `role.check` hook (fail closed).
pub const CAP_MANAGE: &str = "saas_domains.manage";
pub const CAP_READ: &str = "saas_domains.read";
pub const CAP_SELF_ASSIGN: &str = "saas_domains.self_assign";

// Blocked-resolution reason codes. Wire contract with the tenant
// middleware: the middleware matches these exact strings, so they
// are stable contract values, not settings (see the header note).
pub const BLOCKED_UNVERIFIED: &str = "unverified_domain";
pub const BLOCKED_SUSPENDED: &str = "suspended";

// A product subdomain inside its release grace period. The
// middleware maps this reason to its dedicated `Released` blocked
// variant (`core::middleware::BlockedReason::Released`), which
// serves the export notice instead of the generic blocked
// response. The host is blocked either way and never serves
// tenant data.
pub const BLOCKED_RELEASED: &str = "released";

// ---------------------------------------------------------------------------
// Settings — every tunable lives here, with a factory default that an
// administrator can override through PUT /settings. Nothing below is
// a behaviour constant: change the setting, change the behaviour.
// ---------------------------------------------------------------------------

/// Factory defaults. Each one is overridable through the settings
/// endpoints (`saas_domains.*` config keys); the defaults only apply
/// until an administrator changes them.
fn default_reserved_labels() -> Vec<String> {
    [
        "www", "admin", "api", "mail", "app", "dashboard", "static", "cdn", "support",
        "status",
    ]
    .iter()
    .map(|label| label.to_string())
    .collect()
}

fn default_label_pattern() -> String {
    // One DNS label: 1-63 chars, starts/ends alphanumeric, hyphens inside.
    r"^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$".to_string()
}

fn default_max_product_subdomains_per_tenant() -> u32 {
    10
}

fn default_max_product_subdomains_per_product() -> u32 {
    1
}

fn default_product_subdomain_grace_days() -> u32 {
    30
}

fn default_product_subdomain_suggestion_suffixes() -> Vec<String> {
    ["", "-app", "-online", "-portal", "-hub", "-2", "-3"]
        .iter()
        .map(|suffix| suffix.to_string())
        .collect()
}

fn default_alias_target_types() -> Vec<String> {
    ["cname_subdomain", "a_record", "cname_server"]
        .iter()
        .map(|kind| kind.to_string())
        .collect()
}

/// Admin-editable module settings (`saas_domains.*` config keys).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaasDomainsSettings {
    /// Base domain for tenant subdomains, e.g. `tenants.example.com`.
    /// Empty means tenant-subdomain resolution is disabled.
    #[serde(default)]
    pub wildcard_base: String,
    /// Host that serves the control plane (main site / dashboard).
    /// Empty means "first listen host from the app config".
    #[serde(default)]
    pub main_host: String,
    /// Subdomain labels no tenant may claim.
    #[serde(default = "default_reserved_labels")]
    pub reserved_labels: Vec<String>,
    /// Regex an allocated subdomain label must match.
    #[serde(default = "default_label_pattern")]
    pub label_pattern: String,
    /// Maximum active/pending subdomain allocations per tenant.
    #[serde(default = "default_max_subdomains")]
    pub max_subdomains_per_tenant: u32,
    /// When true, tenant self-assignments stay `pending_approval`
    /// until an admin approves them; admin assignments are active
    /// immediately either way.
    #[serde(default)]
    pub require_subdomain_approval: bool,
    /// Default reservation lifetime in hours.
    #[serde(default = "default_reservation_ttl_hours")]
    pub default_reservation_ttl_hours: u32,
    /// Allowed alias target types (`cname_subdomain`, `a_record`,
    /// `cname_server`). A type outside this list is rejected.
    #[serde(default = "default_alias_target_types")]
    pub alias_target_types: Vec<String>,
    /// Maximum live product subdomains per tenant across all
    /// products.
    #[serde(default = "default_max_product_subdomains_per_tenant")]
    pub max_product_subdomains_per_tenant: u32,
    /// Maximum live product subdomains per tenant for one product
    /// (and one package when a package is named). The factory
    /// default of 1 enforces the "one active product subdomain per
    /// (tenant, product/package)" rule.
    #[serde(default = "default_max_product_subdomains_per_product")]
    pub max_product_subdomains_per_product: u32,
    /// Default grace period in days between a product-subdomain
    /// release and the moment its label may be claimed again.
    #[serde(default = "default_product_subdomain_grace_days")]
    pub default_product_subdomain_grace_days: u32,
    /// When true, activating a product subdomain needs an admin
    /// (`saas_domains.manage`); otherwise the owning tenant may
    /// activate its own verified product subdomain.
    #[serde(default)]
    pub require_product_subdomain_approval: bool,
    /// Suffixes the AI suggestion hook appends to a slugified
    /// product name when proposing labels. The empty string means
    /// "the bare product name itself".
    #[serde(default = "default_product_subdomain_suggestion_suffixes")]
    pub product_subdomain_suggestion_suffixes: Vec<String>,
}

fn default_max_subdomains() -> u32 {
    5
}

fn default_reservation_ttl_hours() -> u32 {
    48
}

impl Default for SaasDomainsSettings {
    fn default() -> Self {
        Self {
            wildcard_base: String::new(),
            main_host: String::new(),
            reserved_labels: default_reserved_labels(),
            label_pattern: default_label_pattern(),
            max_subdomains_per_tenant: default_max_subdomains(),
            require_subdomain_approval: false,
            default_reservation_ttl_hours: default_reservation_ttl_hours(),
            alias_target_types: default_alias_target_types(),
            max_product_subdomains_per_tenant: default_max_product_subdomains_per_tenant(),
            max_product_subdomains_per_product: default_max_product_subdomains_per_product(),
            default_product_subdomain_grace_days: default_product_subdomain_grace_days(),
            require_product_subdomain_approval: false,
            product_subdomain_suggestion_suffixes:
                default_product_subdomain_suggestion_suffixes(),
        }
    }
}

impl SaasDomainsSettings {
    /// Validate a settings value before it is stored. The label
    /// pattern must compile, the wildcard base (when set) must be a
    /// valid hostname, and numeric limits must stay in sane bounds.
    fn validate(&self) -> UnifiedResult<()> {
        if !self.wildcard_base.is_empty() {
            validate_hostname(&self.wildcard_base)?;
        }
        if !self.main_host.is_empty() {
            validate_hostname(&self.main_host)?;
        }
        if Regex::new(&self.label_pattern).is_err() {
            return Err(UnifiedError::bad_request(
                "label_pattern is not a valid regular expression",
            ));
        }
        if self.max_subdomains_per_tenant == 0 || self.max_subdomains_per_tenant > 1000 {
            return Err(UnifiedError::bad_request(
                "max_subdomains_per_tenant must be between 1 and 1000",
            ));
        }
        if self.default_reservation_ttl_hours == 0
            || self.default_reservation_ttl_hours > 24 * 365
        {
            return Err(UnifiedError::bad_request(
                "default_reservation_ttl_hours must be between 1 and 8760",
            ));
        }
        if self.alias_target_types.is_empty() {
            return Err(UnifiedError::bad_request(
                "alias_target_types must not be empty",
            ));
        }
        if self.max_product_subdomains_per_tenant == 0
            || self.max_product_subdomains_per_tenant > 1000
        {
            return Err(UnifiedError::bad_request(
                "max_product_subdomains_per_tenant must be between 1 and 1000",
            ));
        }
        if self.max_product_subdomains_per_product == 0
            || self.max_product_subdomains_per_product > 100
        {
            return Err(UnifiedError::bad_request(
                "max_product_subdomains_per_product must be between 1 and 100",
            ));
        }
        if self.default_product_subdomain_grace_days > 365 {
            return Err(UnifiedError::bad_request(
                "default_product_subdomain_grace_days must be between 0 and 365",
            ));
        }
        for suffix in &self.product_subdomain_suggestion_suffixes {
            let suffix = suffix.trim().to_lowercase();
            if suffix.len() > 63
                || (!suffix.is_empty()
                    && (!suffix
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                        || suffix.starts_with('-')
                        || suffix.ends_with('-')))
            {
                return Err(UnifiedError::bad_request(
                    "product_subdomain_suggestion_suffixes entries must be empty or valid label suffixes",
                ));
            }
        }
        for label in &self.reserved_labels {
            let label = label.trim().to_lowercase();
            if label.is_empty() || label.len() > 63 {
                return Err(UnifiedError::bad_request(
                    "reserved_labels entries must be non-empty DNS labels",
                ));
            }
        }
        Ok(())
    }

    /// Compiled label pattern, falling back to the factory default
    /// when the stored pattern ever fails to compile.
    fn label_regex(&self) -> Regex {
        Regex::new(&self.label_pattern)
            .unwrap_or_else(|_| Regex::new(&default_label_pattern()).expect("default pattern compiles"))
    }
}

// ---------------------------------------------------------------------------
// Records.
// ---------------------------------------------------------------------------

/// Reservation statuses: `reserved`, `active`, `released`, `expired`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainReservation {
    pub id: Uuid,
    pub domain: String,
    pub company_id: Uuid,
    pub reserved_by: String,
    pub reserved_until: DateTime<Utc>,
    pub status: String,
    pub site_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Alias statuses: `pending_verification`, `verified`, `suspended`,
/// `released`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainAlias {
    pub id: Uuid,
    pub domain: String,
    pub company_id: Uuid,
    pub target_type: String,
    pub target_value: String,
    pub status: String,
    pub site_id: Option<Uuid>,
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Product-subdomain statuses: `pending_verification`, `active`,
/// `suspended`, `released`. A claim always starts
/// `pending_verification`; verification only records `verified_at`
/// (it never activates on its own); activation moves a verified
/// record to `active`. `released` starts the grace period
/// (`grace_until`); the label is free for reclaim only after the
/// grace period ends (computed from `grace_until`, never by
/// deleting the record).
pub const PRODUCT_SUBDOMAIN_STATUSES: [&str; 4] =
    ["pending_verification", "active", "suspended", "released"];

/// One purchased product's own subdomain. `product_id` is the
/// product identifier from the products module, or the sellable
/// module id itself (for example `academic` for the academic system
/// and `exams` for online exams); `package_id` names the purchased
/// the claim is package-scoped and is null for a standalone product
/// claim. `custom_domain` holds a verified whitelabel domain that
/// replaces this subdomain for the same product scope; it is only
/// ever set from the `whitelabel.domain.verified` event, never from
/// an API request. `site_id` is set when the product surface is
/// provisioned (the SaaS module owns site rows).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductSubdomain {
    pub id: Uuid,
    pub company_id: Uuid,
    pub product_id: String,
    pub package_id: Option<String>,
    pub label: String,
    pub host: String,
    pub custom_domain: Option<String>,
    pub site_id: Option<Uuid>,
    pub status: String,
    pub claimed_by: String,
    pub claimed_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
    pub released_at: Option<DateTime<Utc>>,
    pub grace_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The product scope a request arrived under: the tenant, the
/// product (or package) the host is scoped to, and the record that
/// granted the scope. Handlers of product modules call
/// `require_product_scope` with their own module id to enforce it.
#[derive(Debug, Clone, Serialize)]
pub struct ProductScope {
    pub company_id: Uuid,
    pub product_id: String,
    pub package_id: Option<String>,
    pub product_subdomain_id: Uuid,
    pub host: String,
}

/// Subdomain allocation statuses: `active`, `pending_approval`,
/// `suspended`, `released`, `rejected`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubdomainAllocation {
    pub id: Uuid,
    pub label: String,
    pub hostname: String,
    pub company_id: Uuid,
    pub site_id: Option<Uuid>,
    pub status: String,
    pub approved_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Stores (authoritative at runtime; migration tables are the
// persistence targets for the integrator's Diesel layer).
// ---------------------------------------------------------------------------

static RESERVATIONS: OnceLock<DashMap<Uuid, DomainReservation>> = OnceLock::new();
static ALIASES: OnceLock<DashMap<Uuid, DomainAlias>> = OnceLock::new();
static ALLOCATIONS: OnceLock<DashMap<Uuid, SubdomainAllocation>> = OnceLock::new();
static PRODUCT_SUBDOMAINS: OnceLock<DashMap<Uuid, ProductSubdomain>> = OnceLock::new();
static SETTINGS: OnceLock<RwLock<SaasDomainsSettings>> = OnceLock::new();

fn reservations_store() -> &'static DashMap<Uuid, DomainReservation> {
    RESERVATIONS.get_or_init(DashMap::new)
}

fn aliases_store() -> &'static DashMap<Uuid, DomainAlias> {
    ALIASES.get_or_init(DashMap::new)
}

fn allocations_store() -> &'static DashMap<Uuid, SubdomainAllocation> {
    ALLOCATIONS.get_or_init(DashMap::new)
}

fn product_subdomains_store() -> &'static DashMap<Uuid, ProductSubdomain> {
    PRODUCT_SUBDOMAINS.get_or_init(DashMap::new)
}

fn settings_store() -> &'static RwLock<SaasDomainsSettings> {
    SETTINGS.get_or_init(|| RwLock::new(SaasDomainsSettings::default()))
}

fn settings() -> SaasDomainsSettings {
    settings_store()
        .read()
        .expect("saas_domains settings lock poisoned")
        .clone()
}

fn db_pool(ctx: &AppContext) -> Option<crate::db::DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

// ---------------------------------------------------------------------------
// Validation helpers.
// ---------------------------------------------------------------------------

fn domain_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^(?=.{1,253}$)[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)+$")
            .expect("domain regex must compile")
    })
}

/// Validate a hostname: valid labels, at least one dot, alphabetic TLD.
fn validate_hostname(domain: &str) -> UnifiedResult<()> {
    let domain = domain.trim().to_lowercase();
    if !domain_regex().is_match(&domain) {
        return Err(UnifiedError::bad_request(format!(
            "Domain '{domain}' is not a valid hostname"
        )));
    }
    let tld = domain.rsplit('.').next().unwrap_or("");
    if tld.len() < 2 || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(format!(
            "Domain '{domain}' must end with an alphabetic top-level domain"
        )));
    }
    Ok(())
}

fn normalize_domain(raw: &str) -> UnifiedResult<String> {
    let domain = raw.trim().to_lowercase();
    validate_hostname(&domain)?;
    Ok(domain)
}

/// Validate an alias target value for its type. CNAME targets must
/// be valid hostnames; A targets must be valid public IP literals —
/// private, loopback, link-local, unspecified, and multicast
/// addresses are rejected so an alias can never point a tenant at
/// internal infrastructure.
fn validate_target(target_type: &str, target_value: &str) -> UnifiedResult<String> {
    match target_type {
        "cname_subdomain" | "cname_server" => {
            let value = target_value.trim().to_lowercase();
            validate_hostname(&value)?;
            Ok(value)
        }
        "a_record" => {
            let value = target_value.trim().to_string();
            let address: IpAddr = value.parse().map_err(|_| {
                UnifiedError::bad_request(format!(
                    "A-record target '{value}' is not a valid IP address literal"
                ))
            })?;
            let rejected = match address {
                IpAddr::V4(ip) => {
                    ip.is_private()
                        || ip.is_loopback()
                        || ip.is_link_local()
                        || ip.is_unspecified()
                        || ip.is_multicast()
                        || ip.is_broadcast()
                }
                IpAddr::V6(ip) => {
                    ip.is_loopback()
                        || ip.is_unspecified()
                        || ip.is_multicast()
                        || ip.is_unique_local()
                        || ip.is_unicast_link_local()
                }
            };
            if rejected {
                return Err(UnifiedError::bad_request(format!(
                    "A-record target '{value}' is not a public IP address"
                )));
            }
            Ok(value)
        }
        other => Err(UnifiedError::bad_request(format!(
            "Unknown alias target type '{other}'"
        ))),
    }
}

/// Validate a subdomain label against the configured pattern and
/// the configured reserved-label blocklist.
fn validate_label(label_raw: &str, cfg: &SaasDomainsSettings) -> UnifiedResult<String> {
    let label = label_raw.trim().to_lowercase();
    if !cfg.label_regex().is_match(&label) {
        return Err(UnifiedError::bad_request(
            "Subdomain label must match the configured label pattern (1-63 characters, letters, digits, hyphens, never starting or ending with a hyphen)",
        ));
    }
    if cfg
        .reserved_labels
        .iter()
        .any(|blocked| blocked.trim().to_lowercase() == label)
    {
        return Err(UnifiedError::bad_request(format!(
            "Subdomain label '{label}' is reserved"
        )));
    }
    Ok(label)
}

// ---------------------------------------------------------------------------
// Caller identity and capability checks (properties-module shape).
// ---------------------------------------------------------------------------

struct Caller {
    actor: String,
    role: String,
    company_id: Option<Uuid>,
}

fn caller_from(req: &HttpRequest) -> UnifiedResult<Caller> {
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
    let company_id = req
        .headers()
        .get("x-company-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<Uuid>().ok());
    Ok(Caller { actor, role, company_id })
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

/// Authorize work on one tenant's records. Admins (manage
/// capability) may act for any tenant. Otherwise the caller needs
/// the self-assign capability AND an `X-Company-Id` header that
/// matches the record's company; anything else is forbidden, so one
/// tenant can never touch another tenant's domains.
fn authorize_company(
    ctx: &AppContext,
    caller: &Caller,
    company_id: Uuid,
) -> UnifiedResult<()> {
    if check_capability(ctx, &caller.role, CAP_MANAGE) {
        return Ok(());
    }
    if check_capability(ctx, &caller.role, CAP_SELF_ASSIGN)
        && caller.company_id == Some(company_id)
    {
        return Ok(());
    }
    Err(UnifiedError::forbidden(
        "Caller may only act on its own company's domains",
    ))
}

/// Authorize a list read. Admins see everything; other callers must
/// hold the read capability and are force-scoped to their own
/// company (taken from `X-Company-Id`); returns the forced scope.
fn authorize_list(ctx: &AppContext, caller: &Caller) -> UnifiedResult<Option<Uuid>> {
    if check_capability(ctx, &caller.role, CAP_MANAGE) {
        return Ok(None);
    }
    if check_capability(ctx, &caller.role, CAP_READ) {
        return caller.company_id.ok_or_else(|| {
            UnifiedError::forbidden(
                "Non-admin list reads require an X-Company-Id header",
            )
        }).map(Some);
    }
    Err(UnifiedError::forbidden(format!(
        "Role \"{}\" does not hold permission \"{}\"",
        caller.role, CAP_READ
    )))
}

// ---------------------------------------------------------------------------
// Cross-module conflict check: one domain maps to at most one tenant.
// ---------------------------------------------------------------------------

/// True when `domain` is already claimed anywhere: our own
/// reservations / aliases / allocations, the Domains module's table
/// (Diesel read of the `domains` table), or a hook answer from the
/// Domains / SaaS modules (`domain.lookup`, `saas.site.lookup`).
/// `except_reservation`, `except_alias`, and `except_allocation`
/// exclude the record being updated. Released / expired / rejected
/// records do not block a domain.
fn domain_claimed(
    ctx: &AppContext,
    domain: &str,
    except_reservation: Option<Uuid>,
    except_alias: Option<Uuid>,
    except_allocation: Option<Uuid>,
) -> bool {
    if reservations_store().iter().any(|entry| {
        let row = entry.value();
        row.domain == domain
            && (row.status == "reserved" || row.status == "active")
            && Some(row.id) != except_reservation
    }) {
        return true;
    }
    if aliases_store().iter().any(|entry| {
        let row = entry.value();
        row.domain == domain
            && row.status != "released"
            && Some(row.id) != except_alias
    }) {
        return true;
    }
    if allocations_store().iter().any(|entry| {
        let row = entry.value();
        row.hostname == domain
            && row.status != "released"
            && row.status != "rejected"
            && Some(row.id) != except_allocation
    }) {
        return true;
    }
    if let Some(pool) = db_pool(ctx) {
        use crate::db::schema::domains::dsl as dom_dsl;
        if let Ok(mut conn) = pool.get() {
            if let Ok(true) = dom_dsl::domains
                .filter(dom_dsl::name.eq(domain))
                .select(diesel::dsl::count_star())
                .first::<i64>(&mut conn)
                .map(|count| count > 0)
            {
                return true;
            }
        }
    }
    for event in ["domain.lookup", "saas.site.lookup"] {
        let answer = ctx.hooks.emit(event, json!({ "domain": domain }));
        if answer.results.iter().any(|result| {
            result
                .get("exists")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || result
                    .get("found")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
        }) {
            return true;
        }
    }
    false
}

fn conflict_error(domain: &str) -> UnifiedError {
    UnifiedError::conflict(format!(
        "Domain \"{domain}\" is already claimed by another tenant"
    ))
}

// ---------------------------------------------------------------------------
// Tenant resolution — the contract the tenant middleware depends on.
// ---------------------------------------------------------------------------

/// Normalize a Host header value: strip any `:port`, lowercase,
/// strip one trailing dot.
fn normalize_host(raw: &str) -> String {
    let host = raw.trim().to_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host).to_string();
    match host.rsplit_once(':') {
        Some((bare, port)) if port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty() => {
            bare.to_string()
        }
        _ => host,
    }
}

/// Main-site host: the configured `saas_domains.main_host` when set,
/// otherwise the host part of the first configured listen address
/// (`app.tcp`), otherwise `localhost`.
fn main_host(ctx: &AppContext, cfg: &SaasDomainsSettings) -> String {
    if !cfg.main_host.trim().is_empty() {
        return cfg.main_host.trim().to_lowercase();
    }
    let config = ctx.config.read().expect("config lock poisoned");
    for listen in &config.app.tcp {
        if let Some((host, _)) = listen.rsplit_once(':') {
            if !host.trim().is_empty() {
                return host.trim().to_lowercase();
            }
        }
    }
    "localhost".to_string()
}

/// Resolve a host to a tenant. Resolution order (all driven by the
/// configured rules in `SaasDomainsSettings`, in this fixed order):
///   (a) main-site host -> no result (control plane, caller falls through)
///   (b) `<label>.<wildcard_base>` -> active allocation -> tenant result
///   (c) verified custom-domain alias -> tenant result
///   (d) pending/unverified alias or suspended allocation/alias ->
///       `blocked` result (callers must return a clear error and
///       NEVER serve tenant data for a blocked host)
///   (e) otherwise no result.
/// Every lookup is by exact host match only, so resolution can never
/// return one tenant's data for another tenant's host.
pub fn resolve_host(ctx: &AppContext, raw_host: &str) -> Option<Value> {
    let cfg = settings();
    resolve_host_with(raw_host, &cfg, &main_host(ctx, &cfg))
}

/// Shared resolution core: the hook handler and `resolve_host` both
/// run this, so the resolution order and the blocked reasons live in
/// exactly one place. `main_host_value` is the control-plane host
/// the caller resolved (full chain in `resolve_host`; the configured
/// setting, or `localhost`, in the hook handler, which has no
/// AppContext to read the listen addresses from).
fn resolve_host_with(raw_host: &str, cfg: &SaasDomainsSettings, main_host_value: &str) -> Option<Value> {
    let host = normalize_host(raw_host);
    if host.is_empty() {
        return None;
    }
    if host == main_host_value {
        return None;
    }
    // Product subdomains resolve before generic allocations: a host
    // claimed for one product serves only that product. The result
    // keeps kind "subdomain" so the existing tenant middleware
    // accepts it, and carries the product scope in extra fields.
    // A released host inside its grace period is blocked with the
    // new reason `released`; a released host past its grace period
    // returns no result here, so the label is free for reclaim.
    if let Some(result) = resolve_product_subdomain_value(&host) {
        return Some(result);
    }
    if !cfg.wildcard_base.is_empty() {
        let suffix = format!(".{}", cfg.wildcard_base.trim().to_lowercase());
        if let Some(label) = host.strip_suffix(&suffix) {
            if !label.is_empty() && !label.contains('.') {
                let allocation = allocations_store()
                    .iter()
                    .find(|entry| entry.value().hostname == host)
                    .map(|entry| entry.value().clone());
                if let Some(allocation) = allocation {
                    match allocation.status.as_str() {
                        "active" => {
                            return Some(json!({
                                "tenant_id": allocation.company_id,
                                "company_id": allocation.company_id,
                                "site_id": allocation.site_id,
                                "kind": "subdomain",
                                "verified": true,
                                "host": host,
                            }));
                        }
                        "suspended" | "pending_approval" | "rejected" => {
                            let reason = if allocation.status == "suspended" {
                                BLOCKED_SUSPENDED
                            } else {
                                BLOCKED_UNVERIFIED
                            };
                            return Some(json!({
                                "kind": "blocked",
                                "verified": false,
                                "reason": reason,
                                "host": host,
                            }));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    let alias = aliases_store()
        .iter()
        .find(|entry| entry.value().domain == host)
        .map(|entry| entry.value().clone());
    if let Some(alias) = alias {
        match alias.status.as_str() {
            "verified" => {
                return Some(json!({
                    "tenant_id": alias.company_id,
                    "company_id": alias.company_id,
                    "site_id": alias.site_id,
                    "kind": "custom_domain",
                    "verified": true,
                    "host": host,
                }));
            }
            "suspended" => {
                return Some(json!({
                    "kind": "blocked",
                    "verified": false,
                    "reason": BLOCKED_SUSPENDED,
                    "host": host,
                }));
            }
            _ => {
                return Some(json!({
                    "kind": "blocked",
                    "verified": false,
                    "reason": BLOCKED_UNVERIFIED,
                    "host": host,
                }));
            }
        }
    }
    None
}

/// Blocked-resolution object shared by the product-subdomain
/// branch. The `reason` is one of the stable wire-contract codes
/// (`unverified_domain`, `suspended`) or the new product code
/// `released` (see `BLOCKED_RELEASED`).
fn blocked_resolution(host: &str, reason: &str) -> Value {
    json!({
        "kind": "blocked",
        "verified": false,
        "reason": reason,
        "host": host,
    })
}

/// Resolve one host against the product-subdomain store only.
/// Returns the tenant result (kind `subdomain` plus product-scope
/// fields), a blocked result, or nothing when the host is not a
/// product subdomain (or is a released one past its grace period).
/// The host matches either the subdomain host or the verified
/// custom domain attached to the record.
fn resolve_product_subdomain_value(host: &str) -> Option<Value> {
    let row = product_subdomains_store()
        .iter()
        .find_map(|entry| {
            let record = entry.value();
            if record.host == host || record.custom_domain.as_deref() == Some(host) {
                Some(record.clone())
            } else {
                None
            }
        })?;
    let is_custom_domain = row.custom_domain.as_deref() == Some(host);
    match row.status.as_str() {
        "active" => {
            if row.verified_at.is_none() {
                return Some(blocked_resolution(host, BLOCKED_UNVERIFIED));
            }
            Some(json!({
                "tenant_id": row.company_id,
                "company_id": row.company_id,
                "site_id": row.site_id,
                "kind": "subdomain",
                "verified": true,
                "host": host,
                "scope": "product",
                "product_id": row.product_id,
                "package_id": row.package_id,
                "product_subdomain_id": row.id,
                "custom_domain": is_custom_domain,
            }))
        }
        "suspended" => Some(blocked_resolution(host, BLOCKED_SUSPENDED)),
        "released" => {
            let in_grace = row
                .grace_until
                .map_or(true, |until| until > Utc::now());
            if in_grace {
                Some(blocked_resolution(host, BLOCKED_RELEASED))
            } else {
                None
            }
        }
        _ => Some(blocked_resolution(host, BLOCKED_UNVERIFIED)),
    }
}

/// Resolve a host to its product scope, emitting
/// `saas.product_subdomain.resolve_scoped` when the host resolves
/// to an active product subdomain. This is the entry point the
/// tenant middleware (or its product-scope extension) calls when it
/// needs the scope, not just the tenant: `resolve_host` already
/// carries the same scope fields for the plain tenant path.
pub fn resolve_product_host(ctx: &AppContext, raw_host: &str) -> Option<Value> {
    let host = normalize_host(raw_host);
    let result = resolve_product_subdomain_value(&host)?;
    if result.get("scope").and_then(Value::as_str) == Some("product")
        && result.get("verified").and_then(Value::as_bool) == Some(true)
    {
        ctx.hooks.emit(
            EVENT_PRODUCT_SUBDOMAIN_RESOLVE_SCOPED,
            json!({
                "host": host,
                "company_id": result.get("company_id"),
                "product_id": result.get("product_id"),
                "package_id": result.get("package_id"),
                "product_subdomain_id": result.get("product_subdomain_id"),
            }),
        );
    }
    Some(result)
}

/// The live product scope for a host, if the host is an active,
/// verified product subdomain. Blocked hosts return nothing here;
/// use `resolve_product_subdomain_value` when the blocked reason
/// itself matters.
pub fn product_scope_for_host(raw_host: &str) -> Option<ProductScope> {
    let host = normalize_host(raw_host);
    let row = product_subdomains_store()
        .iter()
        .find_map(|entry| {
            let record = entry.value();
            if record.host == host || record.custom_domain.as_deref() == Some(host) {
                Some(record.clone())
            } else {
                None
            }
        })?;
    if row.status != "active" || row.verified_at.is_none() {
        return None;
    }
    Some(ProductScope {
        company_id: row.company_id,
        product_id: row.product_id,
        package_id: row.package_id,
        product_subdomain_id: row.id,
        host,
    })
}

/// True when the scope covers `module_id`: a direct product match,
/// or the scope's package lists the module. Package membership is
/// asked through `saas_packages.package.modules_requested`
/// (`{"package_id"}`; an answer carries `{"module_ids": [...]}`);
/// when nothing answers, only a direct product match counts.
fn product_scope_allows(ctx: &AppContext, scope: &ProductScope, module_id: &str) -> bool {
    if scope.product_id == module_id {
        return true;
    }
    let Some(package_id) = &scope.package_id else {
        return false;
    };
    let answer = ctx.hooks.emit(
        EVENT_PACKAGE_MODULES_REQUESTED,
        json!({ "package_id": package_id }),
    );
    answer.results.iter().any(|result| {
        result
            .get("module_ids")
            .and_then(Value::as_array)
            .is_some_and(|ids| {
                ids.iter()
                    .any(|id| id.as_str() == Some(module_id))
            })
    })
}

/// Enforce the product scope for one request. Returns the scope
/// when the request arrived on an active product subdomain that
/// covers `module_id`, `None` when the request did not arrive on a
/// product subdomain at all (main host and generic tenant hosts are
/// unaffected), and an error otherwise: 403 when the product host
/// is blocked (suspended, unverified, or inside its release
/// grace), 404 when the host is scoped to a different product.
/// The 404 is deliberate: it never confirms which product the host
/// does serve. Product-module handlers call this at the top with
/// their own module id; the integrator wires the same check into
/// the tenant middleware (see the integration notes).
pub fn require_product_scope(
    ctx: &AppContext,
    req: &HttpRequest,
    module_id: &str,
) -> UnifiedResult<Option<ProductScope>> {
    let Some(host) = req
        .headers()
        .get("host")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
    else {
        return Ok(None);
    };
    let normalized = normalize_host(&host);
    let Some(resolution) = resolve_product_subdomain_value(&normalized) else {
        return Ok(None);
    };
    if resolution.get("kind").and_then(Value::as_str) == Some("blocked") {
        let reason = resolution
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or(BLOCKED_UNVERIFIED);
        return Err(UnifiedError::forbidden(format!(
            "Product subdomain host is blocked ({reason}); the product is not served here"
        ))
        .with_metadata("code", Value::String(reason.to_string())));
    }
    let Some(scope) = product_scope_for_host(&normalized) else {
        return Err(UnifiedError::forbidden(
            "Product subdomain host is not active; the product is not served here",
        ));
    };
    if product_scope_allows(ctx, &scope, module_id) {
        Ok(Some(scope))
    } else {
        Err(UnifiedError::not_found(
            "This product is not available on this subdomain",
        )
        .with_metadata("code", Value::String("product_scope.mismatch".to_string())))
    }
}

/// Expire every reservation whose `reserved_until` has passed.
/// Returns the expired ids. Called by the scheduler hook handler
/// and by the admin expire endpoint.
fn expire_stale_reservations(ctx: &AppContext) -> Vec<Uuid> {
    let now = Utc::now();
    let mut expired: Vec<Uuid> = Vec::new();
    for mut entry in reservations_store().iter_mut() {
        let row = entry.value_mut();
        if row.status == "reserved" && row.reserved_until <= now {
            row.status = "expired".to_string();
            row.updated_at = now;
            expired.push(row.id);
        }
    }
    for id in &expired {
        if let Some(row) = reservations_store().get(id) {
            ctx.hooks.emit(
                EVENT_RESERVATION_EXPIRED,
                json!({
                    "reservation_id": row.id,
                    "domain": row.domain,
                    "company_id": row.company_id,
                }),
            );
        }
    }
    expired
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateReservationRequest {
    domain: String,
    company_id: Uuid,
    #[serde(default)]
    reserved_until: Option<DateTime<Utc>>,
    #[serde(default)]
    site_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct ProvisionReservationRequest {
    company_id: Uuid,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    theme_id: Option<String>,
    #[serde(default)]
    site_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct CreateAliasRequest {
    domain: String,
    company_id: Uuid,
    target_type: String,
    target_value: String,
    #[serde(default)]
    site_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct UpdateAliasRequest {
    #[serde(default)]
    target_type: Option<String>,
    #[serde(default)]
    target_value: Option<String>,
    #[serde(default)]
    site_id: Option<Option<Uuid>>,
}

#[derive(Debug, Deserialize)]
struct CreateSubdomainRequest {
    label: String,
    company_id: Uuid,
    #[serde(default)]
    site_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct ResolveQuery {
    host: String,
}

#[derive(Debug, Deserialize)]
struct CheckProductSubdomainRequest {
    label: String,
    #[serde(default)]
    product_id: Option<String>,
    #[serde(default)]
    company_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct ClaimProductSubdomainRequest {
    label: String,
    company_id: Uuid,
    product_id: String,
    #[serde(default)]
    package_id: Option<String>,
    #[serde(default)]
    site_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct UpdateSettingsRequest {
    #[serde(default)]
    wildcard_base: Option<String>,
    #[serde(default)]
    main_host: Option<String>,
    #[serde(default)]
    reserved_labels: Option<Vec<String>>,
    #[serde(default)]
    label_pattern: Option<String>,
    #[serde(default)]
    max_subdomains_per_tenant: Option<u32>,
    #[serde(default)]
    require_subdomain_approval: Option<bool>,
    #[serde(default)]
    default_reservation_ttl_hours: Option<u32>,
    #[serde(default)]
    alias_target_types: Option<Vec<String>>,
    #[serde(default)]
    max_product_subdomains_per_tenant: Option<u32>,
    #[serde(default)]
    max_product_subdomains_per_product: Option<u32>,
    #[serde(default)]
    default_product_subdomain_grace_days: Option<u32>,
    #[serde(default)]
    require_product_subdomain_approval: Option<bool>,
    #[serde(default)]
    product_subdomain_suggestion_suffixes: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Reservation handlers.
// ---------------------------------------------------------------------------

async fn create_reservation(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateReservationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DomainReservation> = (|| {
        let caller = caller_from(&req)?;
        authorize_company(&ctx, &caller, body.company_id)?;
        let domain = normalize_domain(&body.domain)?;
        if !company_exists(&ctx, body.company_id) {
            return Err(UnifiedError::not_found("Company not found"));
        }
        if domain_claimed(&ctx, &domain, None, None, None) {
            return Err(conflict_error(&domain));
        }
        let cfg = settings();
        let now = Utc::now();
        let reserved_until = match body.reserved_until {
            Some(until) if until > now => until,
            Some(_) => {
                return Err(UnifiedError::bad_request(
                    "reserved_until must be in the future",
                ))
            }
            None => now + chrono::Duration::hours(i64::from(cfg.default_reservation_ttl_hours)),
        };
        let reservation = DomainReservation {
            id: Uuid::now_v7(),
            domain: domain.clone(),
            company_id: body.company_id,
            reserved_by: caller.actor.clone(),
            reserved_until,
            status: "reserved".to_string(),
            site_id: body.site_id,
            created_at: now,
            updated_at: now,
        };
        reservations_store().insert(reservation.id, reservation.clone());
        ctx.hooks.emit(
            EVENT_RESERVATION_CREATED,
            json!({
                "reservation_id": reservation.id,
                "domain": reservation.domain,
                "company_id": reservation.company_id,
                "reserved_by": reservation.reserved_by,
                "reserved_until": reservation.reserved_until,
            }),
        );
        Ok(reservation)
    })();
    match result {
        Ok(row) => json::success_response(row, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_reservations(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        let scope = authorize_list(&ctx, &caller)?;
        let mut items: Vec<DomainReservation> = reservations_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(company_id) = scope {
            items.retain(|row| row.company_id == company_id);
        }
        if let Some(company_id) = params.filters.get("company_id") {
            if let Ok(wanted) = company_id.parse::<Uuid>() {
                if scope.is_some_and(|scoped| scoped != wanted) {
                    return Err(UnifiedError::forbidden(
                        "Cannot list another company's reservations",
                    ));
                }
                items.retain(|row| row.company_id == wanted);
            }
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|row| row.status == *status);
        }
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| row.domain.to_lowercase().contains(&needle));
        }
        let sort = params.allowed_sort(&["domain", "status", "reserved_until", "created_at"], "created_at");
        match sort.as_str() {
            "domain" => items.sort_by(|a, b| a.domain.cmp(&b.domain)),
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            "reserved_until" => items.sort_by(|a, b| a.reserved_until.cmp(&b.reserved_until)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_reservation(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DomainReservation> = (|| {
        let caller = caller_from(&req)?;
        let row = reservations_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Reservation not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        Ok(row)
    })();
    json::respond(result, debug)
}

async fn release_reservation(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DomainReservation> = (|| {
        let caller = caller_from(&req)?;
        let id = id.into_inner();
        let mut row = reservations_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Reservation not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        if row.status != "reserved" {
            return Err(UnifiedError::conflict(format!(
                "Only a reserved reservation can be released (current status: {})",
                row.status
            )));
        }
        row.status = "released".to_string();
        row.updated_at = Utc::now();
        reservations_store().insert(row.id, row.clone());
        ctx.hooks.emit(
            EVENT_RESERVATION_RELEASED,
            json!({
                "reservation_id": row.id,
                "domain": row.domain,
                "company_id": row.company_id,
            }),
        );
        Ok(row)
    })();
    json::respond(result, debug)
}

async fn provision_reservation(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<ProvisionReservationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        let id = id.into_inner();
        let mut row = reservations_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Reservation not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        if body.company_id != row.company_id {
            return Err(UnifiedError::forbidden(
                "A reservation can only be provisioned for the tenant it was reserved for",
            ));
        }
        if row.status != "reserved" {
            return Err(UnifiedError::conflict(format!(
                "Only a reserved reservation can be provisioned (current status: {})",
                row.status
            )));
        }
        if row.reserved_until <= Utc::now() {
            row.status = "expired".to_string();
            row.updated_at = Utc::now();
            reservations_store().insert(row.id, row.clone());
            ctx.hooks.emit(
                EVENT_RESERVATION_EXPIRED,
                json!({
                    "reservation_id": row.id,
                    "domain": row.domain,
                    "company_id": row.company_id,
                }),
            );
            return Err(UnifiedError::conflict(
                "Reservation has expired; reserve the domain again before provisioning",
            ));
        }
        if let Some(site_id) = body.site_id {
            if !crate::modules::company::site_exists(&ctx, site_id) {
                return Err(UnifiedError::not_found("Company site not found"));
            }
            row.site_id = Some(site_id);
        }
        let site_id = body.site_id.unwrap_or_else(Uuid::now_v7);
        row.status = "active".to_string();
        row.updated_at = Utc::now();
        reservations_store().insert(row.id, row.clone());
        ctx.hooks.emit(
            EVENT_RESERVATION_PROVISIONED,
            json!({
                "reservation_id": row.id,
                "domain": row.domain,
                "company_id": row.company_id,
                "site_id": site_id,
            }),
        );
        // Hand the site itself to the SaaS module, which owns site
        // rows; provisioning directly without a reservation keeps
        // working through POST /api/v1/saas/sites as before.
        ctx.hooks.emit(
            EVENT_SITE_PROVISIONED,
            json!({
                "site_id": site_id,
                "company_id": row.company_id,
                "domain": row.domain,
                "theme_id": body.theme_id.clone().unwrap_or_default(),
                "name": body.name.clone().unwrap_or_default(),
                "reservation_id": row.id,
            }),
        );
        Ok(json!({ "reservation": row, "site_id": site_id }))
    })();
    json::respond(result, debug)
}

async fn expire_reservations_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        require_permission(&ctx, &caller.role, CAP_MANAGE)?;
        let expired = expire_stale_reservations(&ctx);
        Ok(json!({ "expired": expired.len(), "ids": expired }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Alias handlers.
// ---------------------------------------------------------------------------

async fn create_alias(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateAliasRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DomainAlias> = (|| {
        let caller = caller_from(&req)?;
        authorize_company(&ctx, &caller, body.company_id)?;
        let domain = normalize_domain(&body.domain)?;
        if !company_exists(&ctx, body.company_id) {
            return Err(UnifiedError::not_found("Company not found"));
        }
        let cfg = settings();
        if !cfg.alias_target_types.contains(&body.target_type) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown alias target type '{}' (allowed: {})",
                body.target_type,
                cfg.alias_target_types.join(", ")
            )));
        }
        let target_value = validate_target(&body.target_type, &body.target_value)?;
        if domain_claimed(&ctx, &domain, None, None, None) {
            return Err(conflict_error(&domain));
        }
        if let Some(site_id) = body.site_id {
            if !crate::modules::company::site_exists(&ctx, site_id) {
                return Err(UnifiedError::not_found("Company site not found"));
            }
        }
        let now = Utc::now();
        let alias = DomainAlias {
            id: Uuid::now_v7(),
            domain: domain.clone(),
            company_id: body.company_id,
            target_type: body.target_type.clone(),
            target_value,
            status: "pending_verification".to_string(),
            site_id: body.site_id,
            verified_at: None,
            created_at: now,
            updated_at: now,
        };
        aliases_store().insert(alias.id, alias.clone());
        ctx.hooks.emit(
            EVENT_ALIAS_CREATED,
            json!({
                "alias_id": alias.id,
                "domain": alias.domain,
                "company_id": alias.company_id,
                "target_type": alias.target_type,
                "target_value": alias.target_value,
            }),
        );
        Ok(alias)
    })();
    match result {
        Ok(row) => json::success_response(row, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_aliases(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        let scope = authorize_list(&ctx, &caller)?;
        let mut items: Vec<DomainAlias> = aliases_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(company_id) = scope {
            items.retain(|row| row.company_id == company_id);
        }
        if let Some(company_id) = params.filters.get("company_id") {
            if let Ok(wanted) = company_id.parse::<Uuid>() {
                if scope.is_some_and(|scoped| scoped != wanted) {
                    return Err(UnifiedError::forbidden(
                        "Cannot list another company's aliases",
                    ));
                }
                items.retain(|row| row.company_id == wanted);
            }
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|row| row.status == *status);
        }
        if let Some(target_type) = params.filters.get("target_type") {
            items.retain(|row| row.target_type == *target_type);
        }
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.domain.to_lowercase().contains(&needle)
                    || row.target_value.to_lowercase().contains(&needle)
            });
        }
        let sort = params.allowed_sort(&["domain", "status", "target_type", "created_at"], "created_at");
        match sort.as_str() {
            "domain" => items.sort_by(|a, b| a.domain.cmp(&b.domain)),
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            "target_type" => items.sort_by(|a, b| a.target_type.cmp(&b.target_type)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_alias(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DomainAlias> = (|| {
        let caller = caller_from(&req)?;
        let row = aliases_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Alias not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        Ok(row)
    })();
    json::respond(result, debug)
}

async fn update_alias(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateAliasRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DomainAlias> = (|| {
        let caller = caller_from(&req)?;
        let id = id.into_inner();
        let mut row = aliases_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Alias not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        if row.status == "released" {
            return Err(UnifiedError::conflict(
                "A released alias cannot be updated",
            ));
        }
        let cfg = settings();
        let target_type = body.target_type.clone().unwrap_or_else(|| row.target_type.clone());
        if !cfg.alias_target_types.contains(&target_type) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown alias target type '{target_type}' (allowed: {})",
                cfg.alias_target_types.join(", ")
            )));
        }
        let target_value = match &body.target_value {
            Some(value) => validate_target(&target_type, value)?,
            None => {
                if body.target_type.is_some() {
                    validate_target(&target_type, &row.target_value)?
                } else {
                    row.target_value.clone()
                }
            }
        };
        if let Some(site_id) = body.site_id {
            match site_id {
                Some(site) => {
                    if !crate::modules::company::site_exists(&ctx, site) {
                        return Err(UnifiedError::not_found("Company site not found"));
                    }
                    row.site_id = Some(site);
                }
                None => row.site_id = None,
            }
        }
        row.target_type = target_type;
        row.target_value = target_value;
        row.updated_at = Utc::now();
        aliases_store().insert(row.id, row.clone());
        ctx.hooks.emit(
            EVENT_ALIAS_UPDATED,
            json!({
                "alias_id": row.id,
                "domain": row.domain,
                "company_id": row.company_id,
                "target_type": row.target_type,
                "target_value": row.target_value,
            }),
        );
        Ok(row)
    })();
    json::respond(result, debug)
}

async fn suspend_alias(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DomainAlias> = (|| {
        let caller = caller_from(&req)?;
        let id = id.into_inner();
        let mut row = aliases_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Alias not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        if row.status != "verified" && row.status != "pending_verification" {
            return Err(UnifiedError::conflict(format!(
                "Only a verified or pending alias can be suspended (current status: {})",
                row.status
            )));
        }
        row.status = "suspended".to_string();
        row.updated_at = Utc::now();
        aliases_store().insert(row.id, row.clone());
        ctx.hooks.emit(
            EVENT_ALIAS_SUSPENDED,
            json!({
                "alias_id": row.id,
                "domain": row.domain,
                "company_id": row.company_id,
            }),
        );
        Ok(row)
    })();
    json::respond(result, debug)
}

async fn release_alias(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DomainAlias> = (|| {
        let caller = caller_from(&req)?;
        let id = id.into_inner();
        let mut row = aliases_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Alias not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        if row.status == "released" {
            return Err(UnifiedError::conflict("Alias is already released"));
        }
        row.status = "released".to_string();
        row.updated_at = Utc::now();
        aliases_store().insert(row.id, row.clone());
        ctx.hooks.emit(
            EVENT_ALIAS_RELEASED,
            json!({
                "alias_id": row.id,
                "domain": row.domain,
                "company_id": row.company_id,
            }),
        );
        Ok(row)
    })();
    json::respond(result, debug)
}

async fn delete_alias(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        let id = id.into_inner();
        let row = aliases_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Alias not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        aliases_store().remove(&id);
        ctx.hooks.emit(
            EVENT_ALIAS_RELEASED,
            json!({
                "alias_id": row.id,
                "domain": row.domain,
                "company_id": row.company_id,
                "deleted": true,
            }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Subdomain allocation handlers.
// ---------------------------------------------------------------------------

async fn create_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateSubdomainRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SubdomainAllocation> = (|| {
        let caller = caller_from(&req)?;
        let cfg = settings();
        if cfg.wildcard_base.trim().is_empty() {
            return Err(UnifiedError::unprocessable(
                "Tenant subdomains are not configured: set saas_domains.wildcard_base first",
            ));
        }
        let is_admin = check_capability(&ctx, &caller.role, CAP_MANAGE);
        if is_admin {
            // Admin assignment: any tenant.
        } else {
            if !check_capability(&ctx, &caller.role, CAP_SELF_ASSIGN)
                || caller.company_id != Some(body.company_id)
            {
                return Err(UnifiedError::forbidden(
                    "Caller may only self-assign subdomains for its own company",
                ));
            }
        }
        if !company_exists(&ctx, body.company_id) {
            return Err(UnifiedError::not_found("Company not found"));
        }
        let label = validate_label(&body.label, &cfg)?;
        let hostname = format!(
            "{label}.{}",
            cfg.wildcard_base.trim().to_lowercase()
        );
        if domain_claimed(&ctx, &hostname, None, None, None) {
            return Err(conflict_error(&hostname));
        }
        let live_count = allocations_store()
            .iter()
            .filter(|entry| {
                let row = entry.value();
                row.company_id == body.company_id
                    && (row.status == "active" || row.status == "pending_approval")
            })
            .count();
        if u32::try_from(live_count).unwrap_or(u32::MAX) >= cfg.max_subdomains_per_tenant {
            return Err(UnifiedError::conflict(format!(
                "Tenant already holds the maximum of {} subdomain allocations",
                cfg.max_subdomains_per_tenant
            )));
        }
        if let Some(site_id) = body.site_id {
            if !crate::modules::company::site_exists(&ctx, site_id) {
                return Err(UnifiedError::not_found("Company site not found"));
            }
        }
        let now = Utc::now();
        // Admin assignments are active immediately; tenant
        // self-assignments stay pending when approval is required
        // (config key saas_domains.require_subdomain_approval).
        let status = if is_admin || !cfg.require_subdomain_approval {
            "active".to_string()
        } else {
            "pending_approval".to_string()
        };
        let allocation = SubdomainAllocation {
            id: Uuid::now_v7(),
            label,
            hostname: hostname.clone(),
            company_id: body.company_id,
            site_id: body.site_id,
            status,
            approved_by: if is_admin { Some(caller.actor.clone()) } else { None },
            created_at: now,
            updated_at: now,
        };
        allocations_store().insert(allocation.id, allocation.clone());
        ctx.hooks.emit(
            EVENT_SUBDOMAIN_CREATED,
            json!({
                "allocation_id": allocation.id,
                "label": allocation.label,
                "hostname": allocation.hostname,
                "company_id": allocation.company_id,
                "status": allocation.status,
            }),
        );
        Ok(allocation)
    })();
    match result {
        Ok(row) => json::success_response(row, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_subdomains(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        let scope = authorize_list(&ctx, &caller)?;
        let mut items: Vec<SubdomainAllocation> = allocations_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(company_id) = scope {
            items.retain(|row| row.company_id == company_id);
        }
        if let Some(company_id) = params.filters.get("company_id") {
            if let Ok(wanted) = company_id.parse::<Uuid>() {
                if scope.is_some_and(|scoped| scoped != wanted) {
                    return Err(UnifiedError::forbidden(
                        "Cannot list another company's subdomains",
                    ));
                }
                items.retain(|row| row.company_id == wanted);
            }
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|row| row.status == *status);
        }
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| row.hostname.to_lowercase().contains(&needle));
        }
        let sort = params.allowed_sort(&["label", "hostname", "status", "created_at"], "created_at");
        match sort.as_str() {
            "label" => items.sort_by(|a, b| a.label.cmp(&b.label)),
            "hostname" => items.sort_by(|a, b| a.hostname.cmp(&b.hostname)),
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SubdomainAllocation> = (|| {
        let caller = caller_from(&req)?;
        let row = allocations_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subdomain allocation not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        Ok(row)
    })();
    json::respond(result, debug)
}

fn transition_allocation(
    ctx: &AppContext,
    req: &HttpRequest,
    id: Uuid,
    from: &[&str],
    to: &str,
    event: &str,
    admin_only: bool,
) -> UnifiedResult<SubdomainAllocation> {
    let caller = caller_from(req)?;
    let mut row = allocations_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Subdomain allocation not found"))?;
    if admin_only {
        require_permission(ctx, &caller.role, CAP_MANAGE)?;
    } else {
        authorize_company(ctx, &caller, row.company_id)?;
    }
    if !from.contains(&row.status.as_str()) {
        return Err(UnifiedError::conflict(format!(
            "Allocation in status '{}' cannot move to '{to}' (allowed from: {})",
            row.status,
            from.join(", ")
        )));
    }
    row.status = to.to_string();
    if to == "active" {
        row.approved_by = Some(caller.actor.clone());
    }
    row.updated_at = Utc::now();
    allocations_store().insert(row.id, row.clone());
    ctx.hooks.emit(
        event,
        json!({
            "allocation_id": row.id,
            "label": row.label,
            "hostname": row.hostname,
            "company_id": row.company_id,
            "status": row.status,
        }),
    );
    Ok(row)
}

async fn approve_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_allocation(
        &ctx,
        &req,
        id.into_inner(),
        &["pending_approval"],
        "active",
        EVENT_SUBDOMAIN_APPROVED,
        true,
    );
    json::respond(result, debug)
}

async fn reject_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_allocation(
        &ctx,
        &req,
        id.into_inner(),
        &["pending_approval"],
        "rejected",
        EVENT_SUBDOMAIN_REJECTED,
        true,
    );
    json::respond(result, debug)
}

async fn suspend_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_allocation(
        &ctx,
        &req,
        id.into_inner(),
        &["active"],
        "suspended",
        EVENT_SUBDOMAIN_SUSPENDED,
        false,
    );
    json::respond(result, debug)
}

async fn release_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_allocation(
        &ctx,
        &req,
        id.into_inner(),
        &["active", "suspended", "pending_approval"],
        "released",
        EVENT_SUBDOMAIN_RELEASED,
        false,
    );
    json::respond(result, debug)
}

async fn delete_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        let id = id.into_inner();
        let row = allocations_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subdomain allocation not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        allocations_store().remove(&id);
        ctx.hooks.emit(
            EVENT_SUBDOMAIN_RELEASED,
            json!({
                "allocation_id": row.id,
                "hostname": row.hostname,
                "company_id": row.company_id,
                "deleted": true,
            }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Product-subdomain helpers.
// ---------------------------------------------------------------------------

/// Validate a product or package identifier: 1-128 characters,
/// lowercase letters, digits, hyphens, and underscores, starting
/// with a letter or digit. Product ids are opaque identifiers from
/// the products module or sellable module ids (`academic`, `exams`);
/// this module never invents them.
fn validate_product_id(raw: &str) -> UnifiedResult<String> {
    let value = raw.trim().to_lowercase();
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        || !value
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return Err(UnifiedError::bad_request(
            "product_id must be 1-128 characters: lowercase letters, digits, hyphens, underscores",
        ));
    }
    Ok(value)
}

/// True when a host is unavailable for a new product subdomain:
/// a live product-subdomain record holds it (active, pending
/// verification, or suspended), a released record is still inside
/// its grace period, or the cross-module conflict check
/// (`domain_claimed`) finds it anywhere else. `except` excludes the
/// record being updated. A released record past its grace period
/// never blocks: its label is free for reclaim.
fn product_host_blocked(ctx: &AppContext, host: &str, except: Option<Uuid>) -> bool {
    let now = Utc::now();
    if product_subdomains_store().iter().any(|entry| {
        let row = entry.value();
        if Some(row.id) == except {
            return false;
        }
        if row.host != host && row.custom_domain.as_deref() != Some(host) {
            return false;
        }
        match row.status.as_str() {
            "active" | "pending_verification" | "suspended" => true,
            "released" => row.grace_until.map_or(true, |until| until > now),
            _ => false,
        }
    }) {
        return true;
    }
    domain_claimed(ctx, host, None, None, None)
}

/// Store-only version of `product_host_blocked` for hook handlers,
/// which have no `AppContext` and therefore cannot run the
/// cross-module (`domain_claimed`) check. The HTTP claim path always
/// runs the full check; hook claims additionally re-check on the
/// next HTTP read of the record.
fn product_host_blocked_stores(host: &str, except: Option<Uuid>) -> bool {
    let now = Utc::now();
    if product_subdomains_store().iter().any(|entry| {
        let row = entry.value();
        if Some(row.id) == except {
            return false;
        }
        if row.host != host && row.custom_domain.as_deref() != Some(host) {
            return false;
        }
        match row.status.as_str() {
            "active" | "pending_verification" | "suspended" => true,
            "released" => row.grace_until.map_or(true, |until| until > now),
            _ => false,
        }
    }) {
        return true;
    }
    if reservations_store().iter().any(|entry| {
        let row = entry.value();
        row.domain == host && (row.status == "reserved" || row.status == "active")
    }) {
        return true;
    }
    if aliases_store().iter().any(|entry| {
        let row = entry.value();
        row.domain == host && row.status != "released"
    }) {
        return true;
    }
    allocations_store().iter().any(|entry| {
        let row = entry.value();
        row.hostname == host && row.status != "released" && row.status != "rejected"
    })
}

/// Count live product subdomains for a tenant, optionally scoped to
/// one product (and one package when given). Live means
/// `pending_verification`, `active`, or `suspended`; released
/// records never count toward a quota.
fn count_live_product_subdomains(
    company_id: Uuid,
    product_id: Option<&str>,
    package_id: Option<&str>,
) -> usize {
    product_subdomains_store()
        .iter()
        .filter(|entry| {
            let row = entry.value();
            row.company_id == company_id
                && (row.status == "pending_verification"
                    || row.status == "active"
                    || row.status == "suspended")
                && product_id.map_or(true, |wanted| row.product_id == wanted)
                && package_id.map_or(true, |wanted| {
                    row.package_id.as_deref() == Some(wanted)
                })
        })
        .count()
}

/// Verify the tenant is entitled to the product (or package) before
/// a claim is accepted. Every source is asked and ANY positive
/// answer entitles; when nothing answers positively the claim is
/// rejected with 403 `entitlement.required` (metadata `code`).
/// Sources, in order:
///   1. `product.entitlement.check` (products module, backed by the
///      store's entitlement data).
///   2. `saas_packages.entitlement.check_requested` (packages
///      module, fail-closed `{"allowed": false}` shape), asked once
///      for the product id and once for the package id when given.
///   3. The licensing module's direct check, only when the product
///      id names a registered sellable module (for example
///      `academic` or `exams`); non-sellable and unknown ids never
///      pass through this source, so a core module id cannot be
///      used to claim without a purchase.
/// Fail-closed on purpose: when no entitlement source is
/// registered at all, every claim is refused rather than granted
/// on a guess. See the integration notes for which source is live
/// today.
fn check_product_entitlement(
    ctx: &AppContext,
    company_id: Uuid,
    product_id: &str,
    package_id: Option<&str>,
    user_id: &str,
) -> UnifiedResult<()> {
    let product_answer = ctx.hooks.emit(
        EVENT_PRODUCT_ENTITLEMENT_CHECK,
        json!({ "product_id": product_id, "user_id": user_id }),
    );
    if product_answer.results.iter().any(|result| {
        result
            .get("entitled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }) {
        return Ok(());
    }
    for module_id in [Some(product_id), package_id] {
        let Some(module_id) = module_id else {
            continue;
        };
        let package_answer = ctx.hooks.emit(
            EVENT_PACKAGE_ENTITLEMENT_CHECK,
            json!({
                "company_id": company_id.to_string(),
                "module_id": module_id,
                "allowed": false,
            }),
        );
        if package_answer.results.iter().any(|result| {
            result
                .get("allowed")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        }) {
            return Ok(());
        }
    }
    let sellable = ctx
        .modules
        .get(product_id)
        .map(|module| module.available_as_product())
        .unwrap_or(false);
    if sellable
        && check_module_entitlement(ctx, &company_id.to_string(), product_id).is_ok()
    {
        return Ok(());
    }
    Err(UnifiedError::forbidden(
        "Tenant is not entitled to this product or package; purchase it before claiming a subdomain",
    )
    .with_metadata("code", Value::String("entitlement.required".to_string())))
}

/// Emit one product-subdomain lifecycle event. Every mutation
/// carries the actor and the tenant/product scope so the audit
/// module can record it once the event names are added to its
/// audited list (see the integration notes).
fn emit_product_subdomain_event(
    ctx: &AppContext,
    event: &str,
    row: &ProductSubdomain,
    actor: &str,
) {
    ctx.hooks.emit(
        event,
        json!({
            "product_subdomain_id": row.id,
            "company_id": row.company_id,
            "product_id": row.product_id,
            "package_id": row.package_id,
            "label": row.label,
            "host": row.host,
            "status": row.status,
            "actor": actor,
        }),
    );
}

/// Slugify a product name into a DNS-label candidate base:
/// lowercase, runs of other characters become one hyphen, leading
/// and trailing hyphens stripped. An empty result means the name
/// cannot seed a suggestion.
fn slugify_product_name(name: &str) -> String {
    let mut base = String::new();
    let mut last_was_hyphen = true;
    for c in name.trim().to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            base.push(c);
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            base.push('-');
            last_was_hyphen = true;
        }
    }
    while base.ends_with('-') {
        base.pop();
    }
    base
}

/// Finalize every released product subdomain whose grace period
/// has ended. The record stays `released` (the audit trail is
/// kept); its label becomes free because the availability check
/// treats past-grace releases as non-blocking. Emits one
/// `saas.product_subdomain.released` event per finalized record.
fn finalize_expired_graces(ctx: &AppContext) -> Vec<Uuid> {
    let now = Utc::now();
    let mut finalized: Vec<Uuid> = Vec::new();
    for entry in product_subdomains_store().iter() {
        let row = entry.value();
        if row.status == "released"
            && row.grace_until.map_or(false, |until| until <= now)
        {
            finalized.push(row.id);
        }
    }
    for id in &finalized {
        if let Some(row) = product_subdomains_store().get(id) {
            emit_product_subdomain_event(
                ctx,
                EVENT_PRODUCT_SUBDOMAIN_RELEASED,
                &row,
                "system:grace-expiry",
            );
        }
    }
    finalized
}

/// Hook-side version of `finalize_expired_graces`: same state
/// outcome (past-grace labels are free by computation), but hook
/// handlers have no hook bus, so no `.released` event can be
/// emitted here. The HTTP expire endpoint is the emitting path.
fn finalize_expired_graces_hook() -> Vec<Uuid> {
    let now = Utc::now();
    product_subdomains_store()
        .iter()
        .filter_map(|entry| {
            let row = entry.value();
            if row.status == "released"
                && row.grace_until.map_or(false, |until| until <= now)
            {
                Some(row.id)
            } else {
                None
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Product-subdomain handlers.
// ---------------------------------------------------------------------------

async fn check_product_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CheckProductSubdomainRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        // Any caller holding a domains capability may check
        // availability; the answer never names the owner of a taken
        // label, so the check itself leaks nothing.
        if !check_capability(&ctx, &caller.role, CAP_MANAGE)
            && !check_capability(&ctx, &caller.role, CAP_READ)
            && !check_capability(&ctx, &caller.role, CAP_SELF_ASSIGN)
        {
            return Err(UnifiedError::forbidden(format!(
                "Role \"{}\" does not hold permission \"{}\"",
                caller.role, CAP_READ
            )));
        }
        if let Some(product_id) = &body.product_id {
            validate_product_id(product_id)?;
        }
        if let Some(company_id) = body.company_id {
            if !check_capability(&ctx, &caller.role, CAP_MANAGE)
                && caller.company_id != Some(company_id)
            {
                return Err(UnifiedError::forbidden(
                    "Cannot check availability for another company's product subdomain",
                ));
            }
        }
        let cfg = settings();
        let label = body.label.trim().to_lowercase();
        if cfg.wildcard_base.trim().is_empty() {
            return Err(UnifiedError::unprocessable(
                "Tenant subdomains are not configured: set saas_domains.wildcard_base first",
            ));
        }
        let host = format!("{label}.{}", cfg.wildcard_base.trim().to_lowercase());
        let (status, reason): (&str, &str) = if validate_label(&label, &cfg).is_err() {
            ("invalid", "Label does not match the configured label pattern")
        } else if cfg
            .reserved_labels
            .iter()
            .any(|blocked| blocked.trim().to_lowercase() == label)
        {
            ("reserved", "Label is reserved")
        } else if product_host_blocked(&ctx, &host, None) {
            ("taken", "Label is already claimed")
        } else {
            ("available", "Label is available")
        };
        Ok(json!({
            "label": label,
            "host": host,
            "available": status == "available",
            "status": status,
            "reason": reason,
        }))
    })();
    json::respond(result, debug)
}

async fn claim_product_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<ClaimProductSubdomainRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductSubdomain> = (|| {
        let caller = caller_from(&req)?;
        let cfg = settings();
        if cfg.wildcard_base.trim().is_empty() {
            return Err(UnifiedError::unprocessable(
                "Tenant subdomains are not configured: set saas_domains.wildcard_base first",
            ));
        }
        authorize_company(&ctx, &caller, body.company_id)?;
        if !company_exists(&ctx, body.company_id) {
            return Err(UnifiedError::not_found("Company not found"));
        }
        let product_id = validate_product_id(&body.product_id)?;
        let package_id = match &body.package_id {
            Some(raw) if !raw.trim().is_empty() => Some(validate_product_id(raw)?),
            _ => None,
        };
        // Entitlement first: no purchase, no subdomain.
        check_product_entitlement(
            &ctx,
            body.company_id,
            &product_id,
            package_id.as_deref(),
            &caller.actor,
        )?;
        let label = validate_label(&body.label, &cfg)?;
        let host = format!("{label}.{}", cfg.wildcard_base.trim().to_lowercase());
        if product_host_blocked(&ctx, &host, None) {
            return Err(UnifiedError::conflict(format!(
                "Subdomain \"{host}\" is already claimed"
            )));
        }
        if count_live_product_subdomains(body.company_id, None, None)
            >= usize::try_from(cfg.max_product_subdomains_per_tenant).unwrap_or(usize::MAX)
        {
            return Err(UnifiedError::conflict(format!(
                "Tenant already holds the maximum of {} product subdomains",
                cfg.max_product_subdomains_per_tenant
            )));
        }
        if count_live_product_subdomains(
            body.company_id,
            Some(&product_id),
            package_id.as_deref(),
        ) >= usize::try_from(cfg.max_product_subdomains_per_product).unwrap_or(usize::MAX)
        {
            return Err(UnifiedError::conflict(
                "Tenant already holds a product subdomain for this product",
            ));
        }
        if let Some(site_id) = body.site_id {
            if !crate::modules::company::site_exists(&ctx, site_id) {
                return Err(UnifiedError::not_found("Company site not found"));
            }
        }
        let now = Utc::now();
        let record = ProductSubdomain {
            id: Uuid::now_v7(),
            company_id: body.company_id,
            product_id: product_id.clone(),
            package_id: package_id.clone(),
            label: label.clone(),
            host: host.clone(),
            custom_domain: None,
            site_id: body.site_id,
            status: "pending_verification".to_string(),
            claimed_by: caller.actor.clone(),
            claimed_at: now,
            verified_at: None,
            released_at: None,
            grace_until: None,
            created_at: now,
            updated_at: now,
        };
        product_subdomains_store().insert(record.id, record.clone());
        emit_product_subdomain_event(
            &ctx,
            EVENT_PRODUCT_SUBDOMAIN_CLAIMED,
            &record,
            &caller.actor,
        );
        Ok(record)
    })();
    match result {
        Ok(row) => json::success_response(row, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_product_subdomains(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        let scope = authorize_list(&ctx, &caller)?;
        let mut items: Vec<ProductSubdomain> = product_subdomains_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(company_id) = scope {
            items.retain(|row| row.company_id == company_id);
        }
        if let Some(company_id) = params.filters.get("company_id") {
            if let Ok(wanted) = company_id.parse::<Uuid>() {
                if scope.is_some_and(|scoped| scoped != wanted) {
                    return Err(UnifiedError::forbidden(
                        "Cannot list another company's product subdomains",
                    ));
                }
                items.retain(|row| row.company_id == wanted);
            }
        }
        if let Some(product_id) = params.filters.get("product_id") {
            items.retain(|row| row.product_id == *product_id);
        }
        if let Some(package_id) = params.filters.get("package_id") {
            items.retain(|row| row.package_id.as_deref() == Some(package_id.as_str()));
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|row| row.status == *status);
        }
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.host.to_lowercase().contains(&needle)
                    || row.label.to_lowercase().contains(&needle)
                    || row.product_id.to_lowercase().contains(&needle)
            });
        }
        let sort = params.allowed_sort(
            &["label", "host", "product_id", "status", "created_at"],
            "created_at",
        );
        match sort.as_str() {
            "label" => items.sort_by(|a, b| a.label.cmp(&b.label)),
            "host" => items.sort_by(|a, b| a.host.cmp(&b.host)),
            "product_id" => items.sort_by(|a, b| a.product_id.cmp(&b.product_id)),
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_product_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductSubdomain> = (|| {
        let caller = caller_from(&req)?;
        let row = product_subdomains_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product subdomain not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        Ok(row)
    })();
    json::respond(result, debug)
}

async fn verify_product_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductSubdomain> = (|| {
        let caller = caller_from(&req)?;
        let id = id.into_inner();
        let mut row = product_subdomains_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product subdomain not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        if row.status != "pending_verification" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending product subdomain can be verified (current status: {})",
                row.status
            )));
        }
        // Idempotent: already verified through the hook path.
        if row.verified_at.is_some() {
            return Ok(row);
        }
        // Verification is owned by the domain-verification module:
        // this endpoint only records its answer, it never verifies
        // by itself. Ask for the status of the subdomain host (and
        // of the attached custom domain, when one is set).
        let mut verified = false;
        for domain in [row.host.clone(), row.custom_domain.clone().unwrap_or_default()] {
            if domain.is_empty() {
                continue;
            }
            let answer = ctx.hooks.emit(
                EVENT_DOMAIN_VERIFICATION_STATUS_REQUESTED,
                json!({ "domain": domain }),
            );
            if answer.results.iter().any(|result| {
                result.get("status").and_then(Value::as_str) == Some("verified")
                    || result
                        .get("verified")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
            }) {
                verified = true;
                break;
            }
        }
        if !verified {
            return Err(UnifiedError::unprocessable(
                "Domain verification has not completed for this subdomain; complete verification first",
            ));
        }
        row.verified_at = Some(Utc::now());
        row.updated_at = Utc::now();
        product_subdomains_store().insert(row.id, row.clone());
        emit_product_subdomain_event(
            &ctx,
            EVENT_PRODUCT_SUBDOMAIN_VERIFIED,
            &row,
            &caller.actor,
        );
        Ok(row)
    })();
    json::respond(result, debug)
}

/// Shared transition for activate / suspend / release. Activation
/// from `pending_verification` requires a completed verification;
/// activation from `suspended` restores the record. Release starts
/// the grace period instead of freeing the label immediately.
fn transition_product_subdomain(
    ctx: &AppContext,
    req: &HttpRequest,
    id: Uuid,
    to: &str,
    event: &str,
) -> UnifiedResult<ProductSubdomain> {
    let caller = caller_from(req)?;
    let mut row = product_subdomains_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Product subdomain not found"))?;
    let cfg = settings();
    if to == "active" && cfg.require_product_subdomain_approval {
        require_permission(ctx, &caller.role, CAP_MANAGE)?;
    } else {
        authorize_company(ctx, &caller, row.company_id)?;
    }
    let allowed_from: &[&str] = match to {
        "active" => &["pending_verification", "suspended"],
        "suspended" => &["active", "pending_verification"],
        "released" => &["active", "suspended", "pending_verification"],
        _ => &[],
    };
    if !allowed_from.contains(&row.status.as_str()) {
        return Err(UnifiedError::conflict(format!(
            "Product subdomain in status '{}' cannot move to '{to}' (allowed from: {})",
            row.status,
            allowed_from.join(", ")
        )));
    }
    if to == "active" && row.status == "pending_verification" && row.verified_at.is_none() {
        return Err(UnifiedError::conflict(
            "Product subdomain cannot be activated before its domain is verified",
        ));
    }
    let now = Utc::now();
    row.status = to.to_string();
    if to == "released" {
        row.released_at = Some(now);
        row.grace_until = Some(
            now + chrono::Duration::days(i64::from(cfg.default_product_subdomain_grace_days)),
        );
    }
    row.updated_at = now;
    product_subdomains_store().insert(row.id, row.clone());
    emit_product_subdomain_event(ctx, event, &row, &caller.actor);
    Ok(row)
}

async fn activate_product_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_product_subdomain(
        &ctx,
        &req,
        id.into_inner(),
        "active",
        EVENT_PRODUCT_SUBDOMAIN_ACTIVATED,
    );
    json::respond(result, debug)
}

async fn suspend_product_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_product_subdomain(
        &ctx,
        &req,
        id.into_inner(),
        "suspended",
        EVENT_PRODUCT_SUBDOMAIN_SUSPENDED,
    );
    json::respond(result, debug)
}

async fn release_product_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_product_subdomain(
        &ctx,
        &req,
        id.into_inner(),
        "released",
        EVENT_PRODUCT_SUBDOMAIN_RELEASE_STARTED,
    );
    json::respond(result, debug)
}

async fn expire_product_subdomains(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        require_permission(&ctx, &caller.role, CAP_MANAGE)?;
        let finalized = finalize_expired_graces(&ctx);
        Ok(json!({ "finalized": finalized.len(), "ids": finalized }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Resolve-debug and settings handlers.
// ---------------------------------------------------------------------------

async fn resolve_debug(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<ResolveQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        require_permission(&ctx, &caller.role, CAP_MANAGE)?;
        let host = normalize_host(&query.host);
        let resolution = resolve_host(&ctx, &query.host);
        Ok(json!({
            "host": host,
            "resolved": resolution.is_some(),
            "resolution": resolution,
        }))
    })();
    json::respond(result, debug)
}

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasDomainsSettings> = (|| {
        let caller = caller_from(&req)?;
        require_permission(&ctx, &caller.role, CAP_MANAGE)?;
        Ok(settings())
    })();
    json::respond(result, debug)
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasDomainsSettings> = (|| {
        let caller = caller_from(&req)?;
        require_permission(&ctx, &caller.role, CAP_MANAGE)?;
        let mut next = settings();
        if let Some(value) = &body.wildcard_base {
            next.wildcard_base = value.trim().to_lowercase();
        }
        if let Some(value) = &body.main_host {
            next.main_host = value.trim().to_lowercase();
        }
        if let Some(value) = &body.reserved_labels {
            next.reserved_labels = value
                .iter()
                .map(|label| label.trim().to_lowercase())
                .collect();
        }
        if let Some(value) = &body.label_pattern {
            next.label_pattern = value.clone();
        }
        if let Some(value) = body.max_subdomains_per_tenant {
            next.max_subdomains_per_tenant = value;
        }
        if let Some(value) = body.require_subdomain_approval {
            next.require_subdomain_approval = value;
        }
        if let Some(value) = body.default_reservation_ttl_hours {
            next.default_reservation_ttl_hours = value;
        }
        if let Some(value) = &body.alias_target_types {
            next.alias_target_types = value
                .iter()
                .map(|kind| kind.trim().to_lowercase())
                .collect();
        }
        if let Some(value) = body.max_product_subdomains_per_tenant {
            next.max_product_subdomains_per_tenant = value;
        }
        if let Some(value) = body.max_product_subdomains_per_product {
            next.max_product_subdomains_per_product = value;
        }
        if let Some(value) = body.default_product_subdomain_grace_days {
            next.default_product_subdomain_grace_days = value;
        }
        if let Some(value) = body.require_product_subdomain_approval {
            next.require_product_subdomain_approval = value;
        }
        if let Some(value) = &body.product_subdomain_suggestion_suffixes {
            next.product_subdomain_suggestion_suffixes = value
                .iter()
                .map(|suffix| suffix.trim().to_lowercase())
                .collect();
        }
        next.validate()?;
        *settings_store()
            .write()
            .expect("saas_domains settings lock poisoned") = next.clone();
        ctx.hooks.emit(
            EVENT_SETTINGS_UPDATED,
            json!({
                "wildcard_base": next.wildcard_base,
                "main_host": next.main_host,
                "max_subdomains_per_tenant": next.max_subdomains_per_tenant,
                "require_subdomain_approval": next.require_subdomain_approval,
                "default_reservation_ttl_hours": next.default_reservation_ttl_hours,
                "alias_target_types": next.alias_target_types,
                "reserved_labels": next.reserved_labels,
                "max_product_subdomains_per_tenant": next.max_product_subdomains_per_tenant,
                "max_product_subdomains_per_product": next.max_product_subdomains_per_product,
                "default_product_subdomain_grace_days": next.default_product_subdomain_grace_days,
                "require_product_subdomain_approval": next.require_product_subdomain_approval,
                "product_subdomain_suggestion_suffixes": next.product_subdomain_suggestion_suffixes,
            }),
        );
        Ok(next)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Hook handlers.
// ---------------------------------------------------------------------------

/// Flip a matching alias to `verified` when the verification module
/// reports a completed verification. Only this event may flip an
/// alias to verified — there is deliberately no API endpoint that
/// does it. Payload: `{"domain": ..., "verified": true}` (or
/// `{"alias_id": ..., "verified": true}`).
///
/// NOTE: hook handlers receive no hook bus, so this handler cannot
/// re-emit a `saas_domains.alias.verified` event; the
/// `domain.verification.completed` event itself is the observable
/// signal, and the alias record carries `verified_at`.
fn apply_verification_completed(domain: Option<&str>, alias_id: Option<Uuid>, verified: bool) {
    if !verified {
        return;
    }
    let target = aliases_store().iter().find_map(|entry| {
        let row = entry.value();
        let matches = alias_id.map_or(false, |id| row.id == id)
            || domain.map_or(false, |name| row.domain == name);
        if matches && row.status == "pending_verification" {
            Some(row.clone())
        } else {
            None
        }
    });
    if let Some(mut alias) = target {
        alias.status = "verified".to_string();
        alias.verified_at = Some(Utc::now());
        alias.updated_at = Utc::now();
        aliases_store().insert(alias.id, alias);
    }
}

// ---------------------------------------------------------------------------
// Product-subdomain hook handlers.
//
// Hook handlers receive no `AppContext` and no hook bus, so they can
// mutate the stores but CANNOT re-emit events and CANNOT run the
// cross-module checks that need a context (`domain_claimed`,
// `company_exists`, the entitlement hooks). Every handler below is
// therefore written defensively: it skips payloads it cannot scope
// safely instead of guessing, and the HTTP endpoints remain the
// paths that verify entitlements and emit the lifecycle events.
// This is the same limitation the alias-verification handler
// already documents, not a new pattern.
// ---------------------------------------------------------------------------

/// Flip a matching product subdomain to verified when the
/// verification module reports a completed verification. Accepts
/// the real payload shape (`{"domain", "status": "verified", ...}`
/// or `{"product_subdomain_id", "verified": true}`); the existing
/// alias handler only reads the `verified` boolean, which the
/// verification module does not send — product subdomains accept
/// both shapes here.
fn apply_product_verification_completed(
    domain: Option<&str>,
    product_subdomain_id: Option<Uuid>,
    status: Option<&str>,
    verified: bool,
) {
    if !verified && status != Some("verified") {
        return;
    }
    let target = product_subdomains_store().iter().find_map(|entry| {
        let row = entry.value();
        let matches = product_subdomain_id.map_or(false, |id| row.id == id)
            || domain.map_or(false, |name| {
                row.host == name || row.custom_domain.as_deref() == Some(name)
            });
        if matches && row.status == "pending_verification" && row.verified_at.is_none() {
            Some(row.clone())
        } else {
            None
        }
    });
    if let Some(mut row) = target {
        let now = Utc::now();
        row.verified_at = Some(now);
        row.updated_at = now;
        product_subdomains_store().insert(row.id, row);
    }
}

/// Claim a product subdomain from an event payload (purchase
/// completion / site-provision request). The claim is created ONLY
/// when the emitting flow attests that entitlements were already
/// validated: `site.provision_requested` is trusted when its
/// `source` is `saas_packages` (that module validates entitlements
/// in its provisioning step before emitting it), and
/// `saas_packages.purchase.completed` is trusted only when it
/// carries `entitlement_verified: true`. Any other payload is
/// skipped: the dashboard claim endpoint (`POST
/// /product-subdomains`) is the path that verifies entitlements.
/// Returns the new record id, or nothing when the claim was
/// skipped. Idempotent: an existing live record for the same
/// (tenant, product/package) or host is returned, never duplicated.
fn claim_product_subdomain_from_event(
    company_id: Uuid,
    product_id_raw: &str,
    package_id_raw: Option<&str>,
    label_raw: &str,
    claimed_by: &str,
) -> Option<Uuid> {
    let cfg = settings();
    if cfg.wildcard_base.trim().is_empty() {
        return None;
    }
    let product_id = validate_product_id(product_id_raw).ok()?;
    let package_id = match package_id_raw {
        Some(raw) if !raw.trim().is_empty() => Some(validate_product_id(raw).ok()?),
        _ => None,
    };
    let label = validate_label(label_raw, &cfg).ok()?;
    let host = format!("{label}.{}", cfg.wildcard_base.trim().to_lowercase());
    if product_host_blocked_stores(&host, None) {
        return product_subdomains_store()
            .iter()
            .find_map(|entry| {
                let row = entry.value();
                if row.company_id == company_id
                    && row.product_id == product_id
                    && row.package_id == package_id
                    && (row.status == "pending_verification"
                        || row.status == "active"
                        || row.status == "suspended")
                {
                    Some(row.id)
                } else {
                    None
                }
            });
    }
    if count_live_product_subdomains(company_id, None, None)
        >= usize::try_from(cfg.max_product_subdomains_per_tenant).unwrap_or(usize::MAX)
    {
        return None;
    }
    if count_live_product_subdomains(
        company_id,
        Some(&product_id),
        package_id.as_deref(),
    ) >= usize::try_from(cfg.max_product_subdomains_per_product).unwrap_or(usize::MAX)
    {
        return None;
    }
    let now = Utc::now();
    let record = ProductSubdomain {
        id: Uuid::now_v7(),
        company_id,
        product_id,
        package_id,
        label,
        host,
        custom_domain: None,
        site_id: None,
        status: "pending_verification".to_string(),
        claimed_by: claimed_by.to_string(),
        claimed_at: now,
        verified_at: None,
        released_at: None,
        grace_until: None,
        created_at: now,
        updated_at: now,
    };
    product_subdomains_store().insert(record.id, record.clone());
    Some(record.id)
}

/// Claim a tenant-level subdomain allocation from an event
/// payload (the tenants provisioning flow). Unlike the product
/// claim above, no `product_id` is involved: the allocation is a
/// generic tenant subdomain (`<label>.<wildcard_base>`) recorded
/// in the allocations store, exactly as the HTTP
/// `POST /subdomains` endpoint creates it.
///
/// Trust basis (same as the product claim): `site.provision_requested`
/// is trusted when its `source` is `tenants` — the tenants module
/// emits it only from its provisioning pipeline, for a tenant it
/// created under a package, after the tenant record itself was
/// validated. The handler, not this function, enforces the source;
/// this function only validates the label, the store-only conflict
/// check, and the per-tenant quota.
///
/// Returns the allocation id and its status, or nothing when the
/// claim was skipped (wildcard base unconfigured, invalid or
/// reserved label, host already claimed by another tenant, or
/// quota reached). Idempotent: an existing live allocation for the
/// same (tenant, host) is returned, never duplicated. A
/// `pending_approval` allocation is returned as-is (the caller
/// decides whether that counts as claimed — the
/// `site.provision_requested` handler only reports `claimed: true`
/// for an `active` allocation, so a pending one stays
/// `subdomain_pending` on the tenant until an admin approves it).
fn claim_tenant_subdomain_from_event(
    company_id: Uuid,
    label_raw: &str,
) -> Option<(Uuid, String)> {
    let cfg = settings();
    if cfg.wildcard_base.trim().is_empty() {
        return None;
    }
    let label = validate_label(label_raw, &cfg).ok()?;
    let host = format!("{label}.{}", cfg.wildcard_base.trim().to_lowercase());
    if let Some(existing) = allocations_store().iter().find_map(|entry| {
        let row = entry.value();
        if row.company_id == company_id
            && row.hostname == host
            && (row.status == "active"
                || row.status == "pending_approval"
                || row.status == "suspended")
        {
            Some((row.id, row.status.clone()))
        } else {
            None
        }
    }) {
        return Some(existing);
    }
    // Store-only conflict check (same coverage as the product-claim
    // path: product subdomains, reservations, aliases, allocations).
    // The Diesel / cross-module `domain.lookup` checks in
    // `domain_claimed` need an AppContext, which hook handlers do
    // not have; the HTTP allocation endpoint remains the full
    // cross-module availability source.
    if product_host_blocked_stores(&host, None) {
        return None;
    }
    let live_count = allocations_store()
        .iter()
        .filter(|entry| {
            let row = entry.value();
            row.company_id == company_id
                && (row.status == "active" || row.status == "pending_approval")
        })
        .count();
    if u32::try_from(live_count).unwrap_or(u32::MAX) >= cfg.max_subdomains_per_tenant {
        return None;
    }
    let now = Utc::now();
    // System-driven provisioning claims follow the admin-assignment
    // rule from `create_subdomain`: active immediately, unless the
    // administrator configured approval for tenant assignments.
    let status = if cfg.require_subdomain_approval {
        "pending_approval".to_string()
    } else {
        "active".to_string()
    };
    let allocation = SubdomainAllocation {
        id: Uuid::now_v7(),
        label,
        hostname: host,
        company_id,
        site_id: None,
        status: status.clone(),
        approved_by: None,
        created_at: now,
        updated_at: now,
    };
    allocations_store().insert(allocation.id, allocation.clone());
    Some((allocation.id, status))
}

/// Release every subdomain of one tenant, driven by
/// `saas_domains.subdomain.release_requested` from the tenant-destroy
/// flow (schema already dropped and verified; serving already stopped
/// at suspension). Generic allocations move straight to `released`
/// (they carry no grace period — a released allocation stops
/// resolving immediately). Product subdomains move to `released`
/// with the configured grace period, exactly like a package
/// cancellation, so an in-grace host resolves as blocked with reason
/// `released` (the middleware serves the export notice) and the
/// label frees only after the grace expires. Returns the released
/// allocation ids and product-subdomain ids.
fn apply_tenant_destroy_release(company_id: Uuid) -> (Vec<Uuid>, Vec<Uuid>) {
    let cfg = settings();
    let now = Utc::now();
    let mut released_allocations: Vec<Uuid> = Vec::new();
    for mut entry in allocations_store().iter_mut() {
        let row = entry.value_mut();
        if row.company_id == company_id
            && (row.status == "active"
                || row.status == "suspended"
                || row.status == "pending_approval")
        {
            row.status = "released".to_string();
            row.updated_at = now;
            released_allocations.push(row.id);
        }
    }
    let grace_days = i64::from(cfg.default_product_subdomain_grace_days);
    let mut released_products: Vec<Uuid> = Vec::new();
    for mut entry in product_subdomains_store().iter_mut() {
        let row = entry.value_mut();
        if row.company_id == company_id
            && (row.status == "active"
                || row.status == "suspended"
                || row.status == "pending_verification")
        {
            row.status = "released".to_string();
            row.released_at = Some(now);
            row.grace_until = Some(now + chrono::Duration::days(grace_days));
            row.updated_at = now;
            released_products.push(row.id);
        }
    }
    (released_allocations, released_products)
}

/// Start release (grace period) for every live product subdomain
/// of one tenant scoped to one package. Driven by
/// `saas_packages.cancelled`. The `.release_started` event cannot
/// be emitted from a hook handler (no bus); the state change is
/// the signal, and the HTTP release endpoint / expire sweep emit
/// the events for operator-driven releases.
fn apply_package_cancelled(company_id: Uuid, package_id: &str) {
    let cfg = settings();
    let now = Utc::now();
    let grace_days = i64::from(cfg.default_product_subdomain_grace_days);
    for mut entry in product_subdomains_store().iter_mut() {
        let row = entry.value_mut();
        if row.company_id == company_id
            && row.package_id.as_deref() == Some(package_id)
            && (row.status == "active"
                || row.status == "suspended"
                || row.status == "pending_verification")
        {
            row.status = "released".to_string();
            row.released_at = Some(now);
            row.grace_until = Some(now + chrono::Duration::days(grace_days));
            row.updated_at = now;
        }
    }
}

/// Suspend every live product subdomain affected by a revoked
/// entitlement. Payloads differ by emitter (`license.revoked`
/// carries a license `product`; `module.entitlement.denied`
/// carries `module_id` / `company_id`), so all three spellings
/// are read. A payload without a company id is skipped: suspending
/// without a tenant scope would risk touching another tenant.
fn apply_entitlement_revoked(
    company_id: Option<Uuid>,
    product_id: Option<&str>,
    package_id: Option<&str>,
) {
    let Some(company_id) = company_id else {
        return;
    };
    let now = Utc::now();
    for mut entry in product_subdomains_store().iter_mut() {
        let row = entry.value_mut();
        if row.company_id != company_id {
            continue;
        }
        let product_matches = product_id.map_or(false, |wanted| row.product_id == wanted);
        let package_matches = package_id.map_or(false, |wanted| {
            row.package_id.as_deref() == Some(wanted)
        });
        if !product_matches && !package_matches {
            continue;
        }
        if row.status == "active" || row.status == "pending_verification" {
            row.status = "suspended".to_string();
            row.updated_at = now;
        }
    }
}

/// Attach a verified whitelabel custom domain to the product
/// subdomain, inheriting its product scope. The whitelabel payload
/// carries no product identifier, so the attach only happens when
/// the tenant holds exactly one live product subdomain; with
/// several, the event is skipped as ambiguous (the whitelabel
/// module must carry `product_subdomain_id` for multi-product
/// tenants — see the integration notes). Unverified domains are
/// never attached: only `whitelabel.domain.verified` is listened
/// to, never `.requested`.
fn apply_whitelabel_domain_verified(company_id_text: &str, domain_raw: &str) {
    let Ok(company_id) = company_id_text.parse::<Uuid>() else {
        return;
    };
    let Ok(domain) = normalize_domain(domain_raw) else {
        return;
    };
    let live: Vec<ProductSubdomain> = product_subdomains_store()
        .iter()
        .filter_map(|entry| {
            let row = entry.value();
            if row.company_id == company_id
                && (row.status == "active" || row.status == "pending_verification")
            {
                Some(row.clone())
            } else {
                None
            }
        })
        .collect();
    if live.len() != 1 {
        return;
    }
    let mut row = live.into_iter().next().expect("exactly one live record");
    row.custom_domain = Some(domain);
    row.updated_at = Utc::now();
    product_subdomains_store().insert(row.id, row);
}

/// Answer the AI suggestion hook
/// (`saas_domains.ai.suggest_subdomain_requested`): payload
/// `{"product_name" | "product_id", "company_id"?, "count"?}`.
/// Suggestions only, availability-checked against the live store;
/// nothing is reserved. The AI module gates the call through its
/// own contracts before emitting this event; this handler never
/// calls the AI module back.
fn answer_ai_suggest_subdomain(ctx: &mut crate::core::hooks::HookContext) {
    let product_name = ctx
        .get("product_name")
        .and_then(Value::as_str)
        .or_else(|| ctx.get("product_id").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    if product_name.trim().is_empty() {
        return;
    }
    let count = ctx
        .get("count")
        .and_then(Value::as_u64)
        .map(|value| value.clamp(1, 10) as usize)
        .unwrap_or(5);
    // Hook handlers have no AppContext, so the availability check
    // runs against the stores only (the same store-only check the
    // hook claim path uses). The HTTP check endpoint remains the
    // full cross-module availability source.
    let cfg = settings();
    let base = slugify_product_name(&product_name);
    if base.is_empty() || cfg.wildcard_base.trim().is_empty() {
        ctx.add_result(json!({ "suggestions": [] }));
        return;
    }
    let mut suggestions: Vec<Value> = Vec::new();
    for suffix in &cfg.product_subdomain_suggestion_suffixes {
        if suggestions.len() >= count {
            break;
        }
        let label = format!("{base}{suffix}");
        if validate_label(&label, &cfg).is_err() {
            continue;
        }
        let host = format!("{label}.{}", cfg.wildcard_base.trim().to_lowercase());
        if product_host_blocked_stores(&host, None) {
            continue;
        }
        suggestions.push(json!({
            "label": label,
            "host": host,
            "available": true,
        }));
    }
    ctx.add_result(json!({
        "product_name": product_name,
        "suggestions": suggestions,
    }));
}

/// Expire stale reservations without an AppContext (hook handlers
/// have none): mutate the store and return the expired ids. The
/// HTTP expire endpoint uses `expire_stale_reservations`, which
/// additionally emits one `saas_domains.reservation.expired` event
/// per reservation.
fn expire_stale_reservations_hook() -> Vec<Uuid> {
    let now = Utc::now();
    let mut expired: Vec<Uuid> = Vec::new();
    for mut entry in reservations_store().iter_mut() {
        let row = entry.value_mut();
        if row.status == "reserved" && row.reserved_until <= now {
            row.status = "expired".to_string();
            row.updated_at = now;
            expired.push(row.id);
        }
    }
    expired
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct SaasDomainsModule;

impl Module for SaasDomainsModule {
    fn id(&self) -> &'static str {
        "saas_domains"
    }

    fn name(&self) -> &'static str {
        "SaaS Domain Management"
    }

    fn description(&self) -> &'static str {
        "Preregister domain reservations, custom domain aliases, tenant subdomain allocation, per-product subdomains with entitlement-gated claims and product-scoped tenant resolution, and tenant resolution for SaaS sites."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["domains", "saas", "company"]
    }

    fn requires(&self) -> &[&'static str] {
        &["domains", "saas", "company"]
    }

    fn available_as_product(&self) -> bool {
        false
    }

    fn product_category(&self) -> Option<&'static str> {
        None
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Tenant resolution (middleware contract): answer with the
        // resolution object, or answer nothing so the caller falls
        // through to its next resolver. Exact host match only.
        bus.register(
            EVENT_TENANT_RESOLVE_REQUESTED,
            "saas_domains",
            Arc::new(|context: &mut crate::core::hooks::HookContext| {
                let Some(host) = context.get("host").and_then(Value::as_str) else {
                    return;
                };
                // Hook handlers have no AppContext, so the
                // control-plane host here is the configured
                // saas_domains.main_host setting, or "localhost"
                // when it is unset; resolve_host applies the full
                // fallback chain (settings, then first listen host).
                let cfg = settings();
                let main = if cfg.main_host.trim().is_empty() {
                    "localhost".to_string()
                } else {
                    cfg.main_host.trim().to_lowercase()
                };
                if let Some(result) = resolve_host_with(host, &cfg, &main) {
                    context.add_result(result);
                }
            }),
        );

        // Alias lookup by domain or id.
        bus.register(
            EVENT_ALIAS_LOOKUP,
            "saas_domains",
            Arc::new(|context: &mut crate::core::hooks::HookContext| {
                let by_id = context
                    .get("alias_id")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<Uuid>().ok());
                let by_domain = context
                    .get("domain")
                    .and_then(Value::as_str)
                    .map(|value| value.trim().to_lowercase());
                let found = aliases_store().iter().find_map(|entry| {
                    let row = entry.value();
                    let matches = by_id.map_or(false, |id| row.id == id)
                        || by_domain
                            .as_deref()
                            .map_or(false, |domain| row.domain == domain);
                    if matches {
                        Some(row.clone())
                    } else {
                        None
                    }
                });
                if let Some(alias) = found {
                    if let Ok(value) = serde_json::to_value(&alias) {
                        context.add_result(value);
                    }
                }
            }),
        );

        // Scheduler sweep: expire past-due reservations.
        bus.register(
            EVENT_RESERVATIONS_EXPIRE,
            "saas_domains",
            Arc::new(|context: &mut crate::core::hooks::HookContext| {
                let expired = expire_stale_reservations_hook();
                context.add_result(json!({
                    "expired": expired.len(),
                    "ids": expired,
                }));
            }),
        );

        // Verification completion flips a pending alias to verified.
        bus.register(
            EVENT_DOMAIN_VERIFICATION_COMPLETED,
            "saas_domains",
            Arc::new(|context: &mut crate::core::hooks::HookContext| {
                let domain = context.get("domain").and_then(Value::as_str);
                let alias_id = context
                    .get("alias_id")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<Uuid>().ok());
                let verified = context
                    .get("verified")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                apply_verification_completed(domain, alias_id, verified);
            }),
        );

        // Verification completion also flips a matching product
        // subdomain to verified. The verification module sends
        // `status: "verified"` (no `verified` boolean); both shapes
        // are accepted here.
        bus.register(
            EVENT_DOMAIN_VERIFICATION_COMPLETED,
            "saas_domains",
            Arc::new(|context: &mut crate::core::hooks::HookContext| {
                let domain = context.get("domain").and_then(Value::as_str);
                let product_subdomain_id = context
                    .get("product_subdomain_id")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<Uuid>().ok());
                let status = context.get("status").and_then(Value::as_str);
                let verified = context
                    .get("verified")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                apply_product_verification_completed(
                    domain,
                    product_subdomain_id,
                    status,
                    verified,
                );
            }),
        );

        // Package purchase completed: claim intent only when the
        // payload attests entitlement was validated
        // (`entitlement_verified: true`) and names a subdomain
        // label. Anything else is skipped; the dashboard claim
        // endpoint is the normal path.
        bus.register(
            EVENT_PACKAGE_PURCHASE_COMPLETED,
            "saas_domains",
            Arc::new(|context: &mut crate::core::hooks::HookContext| {
                if context
                    .get("entitlement_verified")
                    .and_then(Value::as_bool)
                    != Some(true)
                {
                    return;
                }
                let Some(company_id) = context
                    .get("company_id")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<Uuid>().ok())
                else {
                    return;
                };
                let Some(product_id) = context
                    .get("product_id")
                    .and_then(Value::as_str)
                    .or_else(|| context.get("module_id").and_then(Value::as_str))
                else {
                    return;
                };
                let Some(label) = context.get("subdomain").and_then(Value::as_str) else {
                    return;
                };
                let package_id = context.get("package_id").and_then(Value::as_str);
                let claimed_by = context
                    .get("buyer_id")
                    .and_then(Value::as_str)
                    .unwrap_or("system:purchase");
                if let Some(id) = claim_product_subdomain_from_event(
                    company_id,
                    product_id,
                    package_id,
                    label,
                    claimed_by,
                ) {
                    context.add_result(json!({
                        "product_subdomain_id": id,
                        "claimed": true,
                    }));
                }
            }),
        );

        // Package cancelled: start release (grace) for the
        // package's product subdomains.
        bus.register(
            EVENT_PACKAGE_CANCELLED,
            "saas_domains",
            Arc::new(|context: &mut crate::core::hooks::HookContext| {
                let Some(company_id) = context
                    .get("company_id")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<Uuid>().ok())
                else {
                    return;
                };
                let Some(package_id) = context.get("package_id").and_then(Value::as_str)
                else {
                    return;
                };
                apply_package_cancelled(company_id, package_id);
            }),
        );

        // Entitlement revoked: suspend the affected product
        // subdomains so a revoked purchase stops serving. Both the
        // license event and the module-entitlement denial are
        // listened to; a payload without a tenant scope is skipped.
        for event in [events::LICENSE_REVOKED, MODULE_ENTITLEMENT_DENIED_EVENT] {
            bus.register(
                event,
                "saas_domains",
                Arc::new(move |context: &mut crate::core::hooks::HookContext| {
                    let company_id = context
                        .get("company_id")
                        .and_then(Value::as_str)
                        .and_then(|value| value.parse::<Uuid>().ok());
                    let product_id = context
                        .get("product_id")
                        .and_then(Value::as_str)
                        .or_else(|| context.get("module_id").and_then(Value::as_str))
                        .or_else(|| context.get("product").and_then(Value::as_str));
                    let package_id = context.get("package_id").and_then(Value::as_str);
                    apply_entitlement_revoked(company_id, product_id, package_id);
                }),
            );
        }

        // Site-provision request: claim intent from a provisioning
        // flow. Two trusted sources, each validated by its own
        // module before emitting:
        //   - `saas_packages`: a PRODUCT subdomain claim (requires
        //     `product_id`; that module validates entitlements in
        //     its provisioning step before emitting). This closes
        //     the open item the packages module records ("the
        //     domains flow has no provision-request listener
        //     today").
        //   - `tenants`: a TENANT-level subdomain claim (requires
        //     the tenant id; no `product_id`). The tenants module
        //     emits it only from its provisioning pipeline, for a
        //     tenant it created under a package; the claim is a
        //     generic allocation, not a product subdomain. Only an
        //     `active` allocation counts as claimed: a
        //     `pending_approval` allocation is created (or found)
        //     but reported unclaimed, so the tenant keeps
        //     `subdomain_pending: true` until an admin approves it.
        // Any other source is declined with `subdomain_pending:
        // true` and a reason, never claimed.
        bus.register(
            EVENT_SITE_PROVISION_REQUESTED,
            "saas_domains",
            Arc::new(|context: &mut crate::core::hooks::HookContext| {
                let source = context.get("source").and_then(Value::as_str);
                if source == Some("tenants") {
                    let Some(company_id) = context
                        .get("company_id")
                        .and_then(Value::as_str)
                        .or_else(|| context.get("tenant_id").and_then(Value::as_str))
                        .and_then(|value| value.parse::<Uuid>().ok())
                    else {
                        context.add_result(json!({
                            "claimed": false,
                            "subdomain_pending": true,
                            "reason": "provision request carries no company_id or tenant_id",
                        }));
                        return;
                    };
                    let Some(label) = context
                        .get("subdomain")
                        .and_then(Value::as_str)
                        .or_else(|| context.get("label").and_then(Value::as_str))
                    else {
                        context.add_result(json!({
                            "claimed": false,
                            "subdomain_pending": true,
                            "reason": "provision request carries no subdomain label",
                        }));
                        return;
                    };
                    match claim_tenant_subdomain_from_event(company_id, label) {
                        Some((id, status)) if status == "active" => {
                            context.add_result(json!({
                                "allocation_id": id,
                                "claimed": true,
                                "subdomain_pending": false,
                                "status": status,
                            }));
                        }
                        Some((id, status)) => {
                            context.add_result(json!({
                                "allocation_id": id,
                                "claimed": false,
                                "subdomain_pending": true,
                                "status": status,
                                "reason": "subdomain allocation created but pending approval",
                            }));
                        }
                        None => {
                            context.add_result(json!({
                                "claimed": false,
                                "subdomain_pending": true,
                                "reason": "label unavailable, quota reached, or wildcard base unconfigured",
                            }));
                        }
                    }
                    return;
                }
                if source != Some("saas_packages") {
                    context.add_result(json!({
                        "claimed": false,
                        "subdomain_pending": true,
                        "reason": "provision request source is not a trusted provisioning module",
                    }));
                    return;
                }
                let Some(company_id) = context
                    .get("company_id")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<Uuid>().ok())
                else {
                    context.add_result(json!({
                        "claimed": false,
                        "subdomain_pending": true,
                        "reason": "provision request carries no company_id",
                    }));
                    return;
                };
                let Some(product_id) = context
                    .get("product_id")
                    .and_then(Value::as_str)
                    .or_else(|| context.get("module_id").and_then(Value::as_str))
                else {
                    context.add_result(json!({
                        "claimed": false,
                        "subdomain_pending": true,
                        "reason": "provision request carries no product_id",
                    }));
                    return;
                };
                let Some(label) = context.get("subdomain").and_then(Value::as_str) else {
                    context.add_result(json!({
                        "claimed": false,
                        "subdomain_pending": true,
                        "reason": "provision request carries no subdomain label",
                    }));
                    return;
                };
                let package_id = context.get("package_id").and_then(Value::as_str);
                match claim_product_subdomain_from_event(
                    company_id,
                    product_id,
                    package_id,
                    label,
                    "system:saas_packages",
                ) {
                    Some(id) => {
                        context.add_result(json!({
                            "product_subdomain_id": id,
                            "claimed": true,
                            "subdomain_pending": false,
                        }));
                    }
                    None => {
                        context.add_result(json!({
                            "claimed": false,
                            "subdomain_pending": true,
                            "reason": "label unavailable, quota reached, or record already exists",
                        }));
                    }
                }
            }),
        );

        // Tenant-destroy revoke: release every subdomain of the
        // destroyed tenant (generic allocations immediately,
        // product subdomains into their release grace period).
        // The destroy flow already verified the schema drop and
        // stopped serving at suspension, so this listener only
        // moves records; it never serves or blocks traffic itself.
        bus.register(
            EVENT_SUBDOMAIN_RELEASE_REQUESTED,
            "saas_domains",
            Arc::new(|context: &mut crate::core::hooks::HookContext| {
                let Some(company_id) = context
                    .get("tenant_id")
                    .and_then(Value::as_str)
                    .or_else(|| context.get("company_id").and_then(Value::as_str))
                    .and_then(|value| value.parse::<Uuid>().ok())
                else {
                    return;
                };
                let (allocations, products) = apply_tenant_destroy_release(company_id);
                context.add_result(json!({
                    "tenant_id": company_id,
                    "released_allocations": allocations,
                    "released_product_subdomains": products,
                }));
            }),
        );

        // Whitelabel verified custom domain: attach it to the
        // tenant's product subdomain, inheriting the product scope.
        bus.register(
            EVENT_WHITELABEL_DOMAIN_VERIFIED,
            "saas_domains",
            Arc::new(|context: &mut crate::core::hooks::HookContext| {
                let Some(company_id) = context.get("company_id").and_then(Value::as_str)
                else {
                    return;
                };
                let Some(domain) = context.get("domain").and_then(Value::as_str) else {
                    return;
                };
                apply_whitelabel_domain_verified(company_id, domain);
            }),
        );

        // AI capability hook: suggest available labels for a
        // product name. Suggestions only; nothing is reserved.
        bus.register(
            EVENT_AI_SUGGEST_SUBDOMAIN_REQUESTED,
            "saas_domains",
            Arc::new(|context: &mut crate::core::hooks::HookContext| {
                answer_ai_suggest_subdomain(context);
            }),
        );

        // Scheduler sweep: finalize releases whose grace ended.
        bus.register(
            EVENT_PRODUCT_SUBDOMAIN_EXPIRE_GRACE,
            "saas_domains",
            Arc::new(|context: &mut crate::core::hooks::HookContext| {
                let finalized = finalize_expired_graces_hook();
                context.add_result(json!({
                    "finalized": finalized.len(),
                    "ids": finalized,
                }));
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/reservations", owner: "saas_domains", description: "Create domain reservation" },
            RouteInfo { method: "GET", path: "/api/v1/saas/domains/reservations", owner: "saas_domains", description: "List domain reservations" },
            RouteInfo { method: "GET", path: "/api/v1/saas/domains/reservations/{id}", owner: "saas_domains", description: "Get domain reservation" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/reservations/{id}/release", owner: "saas_domains", description: "Release domain reservation" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/reservations/{id}/provision", owner: "saas_domains", description: "Provision reserved domain" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/reservations/expire", owner: "saas_domains", description: "Expire stale reservations" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/aliases", owner: "saas_domains", description: "Create custom domain alias" },
            RouteInfo { method: "GET", path: "/api/v1/saas/domains/aliases", owner: "saas_domains", description: "List custom domain aliases" },
            RouteInfo { method: "GET", path: "/api/v1/saas/domains/aliases/{id}", owner: "saas_domains", description: "Get custom domain alias" },
            RouteInfo { method: "PUT", path: "/api/v1/saas/domains/aliases/{id}", owner: "saas_domains", description: "Update custom domain alias" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/aliases/{id}/suspend", owner: "saas_domains", description: "Suspend custom domain alias" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/aliases/{id}/release", owner: "saas_domains", description: "Release custom domain alias" },
            RouteInfo { method: "DELETE", path: "/api/v1/saas/domains/aliases/{id}", owner: "saas_domains", description: "Delete custom domain alias" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/subdomains", owner: "saas_domains", description: "Allocate tenant subdomain" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/product-subdomains/check", owner: "saas_domains", description: "Check availability of a product-subdomain label" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/product-subdomains", owner: "saas_domains", description: "Claim a product subdomain for a purchased product or package" },
            RouteInfo { method: "GET", path: "/api/v1/saas/domains/product-subdomains", owner: "saas_domains", description: "List product subdomains" },
            RouteInfo { method: "GET", path: "/api/v1/saas/domains/product-subdomains/{id}", owner: "saas_domains", description: "Get product subdomain" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/product-subdomains/{id}/verify", owner: "saas_domains", description: "Mark product subdomain verified after domain verification confirms it" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/product-subdomains/{id}/activate", owner: "saas_domains", description: "Activate a verified product subdomain" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/product-subdomains/{id}/suspend", owner: "saas_domains", description: "Suspend a product subdomain" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/product-subdomains/{id}/release", owner: "saas_domains", description: "Start release of a product subdomain (grace period first)" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/product-subdomains/expire", owner: "saas_domains", description: "Finalize product-subdomain releases whose grace period ended" },
            RouteInfo { method: "GET", path: "/api/v1/saas/domains/subdomains", owner: "saas_domains", description: "List subdomain allocations" },
            RouteInfo { method: "GET", path: "/api/v1/saas/domains/subdomains/{id}", owner: "saas_domains", description: "Get subdomain allocation" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/subdomains/{id}/approve", owner: "saas_domains", description: "Approve subdomain allocation" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/subdomains/{id}/reject", owner: "saas_domains", description: "Reject subdomain allocation" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/subdomains/{id}/suspend", owner: "saas_domains", description: "Suspend subdomain allocation" },
            RouteInfo { method: "POST", path: "/api/v1/saas/domains/subdomains/{id}/release", owner: "saas_domains", description: "Release subdomain allocation" },
            RouteInfo { method: "DELETE", path: "/api/v1/saas/domains/subdomains/{id}", owner: "saas_domains", description: "Delete subdomain allocation" },
            RouteInfo { method: "GET", path: "/api/v1/saas/domains/resolve", owner: "saas_domains", description: "Resolve debug for a host" },
            RouteInfo { method: "GET", path: "/api/v1/saas/domains/settings", owner: "saas_domains", description: "Get SaaS domains settings" },
            RouteInfo { method: "PUT", path: "/api/v1/saas/domains/settings", owner: "saas_domains", description: "Update SaaS domains settings" },
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/saas/domains")
                .route("/reservations", web::post().to(create_reservation))
                .route("/reservations", web::get().to(list_reservations))
                .route("/reservations/expire", web::post().to(expire_reservations_endpoint))
                .route("/reservations/{id}", web::get().to(get_reservation))
                .route("/reservations/{id}/release", web::post().to(release_reservation))
                .route("/reservations/{id}/provision", web::post().to(provision_reservation))
                .route("/aliases", web::post().to(create_alias))
                .route("/aliases", web::get().to(list_aliases))
                .route("/aliases/{id}", web::get().to(get_alias))
                .route("/aliases/{id}", web::put().to(update_alias))
                .route("/aliases/{id}", web::delete().to(delete_alias))
                .route("/aliases/{id}/suspend", web::post().to(suspend_alias))
                .route("/aliases/{id}/release", web::post().to(release_alias))
                .route("/subdomains", web::post().to(create_subdomain))
                .route("/subdomains", web::get().to(list_subdomains))
                .route("/subdomains/{id}", web::get().to(get_subdomain))
                .route("/subdomains/{id}", web::delete().to(delete_subdomain))
                .route("/subdomains/{id}/approve", web::post().to(approve_subdomain))
                .route("/subdomains/{id}/reject", web::post().to(reject_subdomain))
                .route("/subdomains/{id}/suspend", web::post().to(suspend_subdomain))
                .route("/subdomains/{id}/release", web::post().to(release_subdomain))
                .route("/product-subdomains/check", web::post().to(check_product_subdomain))
                .route("/product-subdomains/expire", web::post().to(expire_product_subdomains))
                .route("/product-subdomains", web::post().to(claim_product_subdomain))
                .route("/product-subdomains", web::get().to(list_product_subdomains))
                .route("/product-subdomains/{id}", web::get().to(get_product_subdomain))
                .route("/product-subdomains/{id}/verify", web::post().to(verify_product_subdomain))
                .route("/product-subdomains/{id}/activate", web::post().to(activate_product_subdomain))
                .route("/product-subdomains/{id}/suspend", web::post().to(suspend_product_subdomain))
                .route("/product-subdomains/{id}/release", web::post().to(release_product_subdomain))
                .route("/resolve", web::get().to(resolve_debug))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SaasDomainsModule));
}
