// Events, capabilities, vocabularies, limits.

use unified_core::hooks::events;

// Events, capabilities, vocabularies, limits.

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
pub(crate) const LICENSE_REVOKED_EVENT: &str = events::LICENSE_REVOKED;
pub(crate) const SUBSCRIPTION_CANCELLED_EVENT: &str = "subscription.cancelled";
pub(crate) const SUBSCRIPTION_EXPIRED_EVENT: &str = "subscription.expired";
pub(crate) const SAAS_PACKAGE_CANCELLED_EVENT: &str = "saas_packages.cancelled";
/// The tenants module grants a module to a tenant. Payload:
/// `{"tenant_id", "company_id", "module_id", "granted_via",
/// "granted_via_id", "source_ref", "actor_id"}`. `granted_via_id`
/// is the package/preset id (a UUID) when the grant names one;
/// `source_ref` is the subscription id (a store string) for a
/// subscription grant - the entitlements mirror stores it as the
/// row's `source_ref`, which is what a later
/// subscription-cancel lapses the row through. The constants live
/// as plain strings (not imports) because the tenants module is
/// not registered in `src/modules/mod.rs` yet, so its items are
/// not importable.
pub(crate) const TENANT_MODULE_GRANTED_EVENT: &str = "tenant.module.granted";
/// The tenants module revokes a module grant. Payload:
/// `{"tenant_id", "company_id", "module_id", "reason", "actor_id"}`.
pub(crate) const TENANT_MODULE_REVOKED_EVENT: &str = "tenant.module.revoked";

/// Modules allowed to mint entitlements through the
/// `entitlements.grant_requested` hook command. Verified against
/// the codebase: no module in the tree emits that event today; the
/// documented provisioning emitters are `saas_packages` (package
/// provisioning) and `store` (checkout). An emitter must name
/// itself in the payload (`emitter`); anything else - including a
/// missing emitter - is rejected, never minted. This stops
/// accidental or cross-module minting; it is not authentication
/// against a compromised in-process emitter (hook payloads are
/// trusted input, same as the header identity model documented in
/// the module header).
pub(crate) const GRANT_EMITTERS: &[&str] = &["saas_packages", "store"];

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
/// A module that is not sellable needs no entitlement (see
/// `licensing::check_module_entitlement`): it ships with the
/// platform and is granted to every tenant implicitly. Derived at
/// read time only, like the owner entitlement, and deliberately
/// NOT a member of `SOURCE_KINDS`: no stored row may ever carry
/// this kind, so the admin grant endpoint and the grant-event
/// validation keep rejecting it.
pub const SOURCE_BUILTIN: &str = "builtin";
pub(crate) const SOURCE_KINDS: &[&str] = &[
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
pub(crate) const DEFAULT_GRACE_PERIOD_DAYS: i64 = 14;
pub(crate) const MAX_GRACE_PERIOD_DAYS: i64 = 365;
pub(crate) const MAX_ID_LEN: usize = 128;
pub(crate) const MAX_MODULE_ID_LEN: usize = 64;
pub(crate) const MAX_SOURCE_ID_LEN: usize = 256;
pub(crate) const MAX_REASON_LEN: usize = 1000;
pub(crate) const MAX_ACTOR_LEN: usize = 128;
pub(crate) const MAX_MODULES_PER_CHANGE: usize = 500;
pub(crate) const DENIED_LOOKBACK_DAYS: i64 = 30;
pub(crate) const MAX_SUGGESTIONS: usize = 5;
