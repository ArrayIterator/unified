//! SaaS Domains - event, capability and blocked-reason names.
//!
//! Part of the `saas_domains` module split; see `src/modules/saas_domains.rs`.

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

pub const EVENT_ALIAS_RESUMED: &str = "saas_domains.alias.resumed";

pub const EVENT_ALIAS_RELEASED: &str = "saas_domains.alias.released";

pub const EVENT_SUBDOMAIN_CREATED: &str = "saas_domains.subdomain.created";

pub const EVENT_SUBDOMAIN_APPROVED: &str = "saas_domains.subdomain.approved";

pub const EVENT_SUBDOMAIN_REJECTED: &str = "saas_domains.subdomain.rejected";

pub const EVENT_SUBDOMAIN_SUSPENDED: &str = "saas_domains.subdomain.suspended";

pub const EVENT_SUBDOMAIN_RESUMED: &str = "saas_domains.subdomain.resumed";

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
pub const EVENT_DOMAIN_VERIFICATION_STATUS_REQUESTED: &str = "domain.verification.status_requested";

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
