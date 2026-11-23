//! tenants - events.

use super::{constants::*, vocabulary::*, settings::*, records::*, state::*, auth::*, helpers::*, provisioning::*, destroy::*, handlers::*, module::*, tenancy::*};



// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------
/// Hook event names this module emits. Add each one to
/// `AUDITED_EVENTS` in `audit.rs` (see the module header).
pub const EVENT_TENANT_CREATED: &str = "tenant.created";

pub const EVENT_TENANT_PROVISIONED: &str = "tenant.provisioned";

pub const EVENT_TENANT_PROVISION_FAILED: &str = "tenant.provision_failed";

pub const EVENT_TENANT_SUSPENDED: &str = "tenant.suspended";

pub const EVENT_TENANT_RESUMED: &str = "tenant.resumed";

pub const EVENT_TENANT_MODULE_GRANTED: &str = "tenant.module.granted";

pub const EVENT_TENANT_MODULE_SKIPPED: &str = "tenant.module.skipped";

pub const EVENT_TENANT_MODULE_REVOKED: &str = "tenant.module.revoked";

pub const EVENT_TENANT_DEPROVISIONING_STARTED: &str = "tenant.deprovisioning_started";

pub const EVENT_TENANT_DELETED: &str = "tenant.deleted";

pub const EVENT_TENANT_SETTINGS_UPDATED: &str = "tenant.settings.updated";


/// Hook query this module ANSWERS for the destroy flow (B3):
/// `tenant.lookup` with `{"tenant_id", "tenant": null}`; the
/// answerer sets the `tenant` payload key and adds the tenant
/// record as a result. The destroy module resolves tenants only
/// through this query and never reads the tenants tables directly.
pub const EVENT_TENANT_LOOKUP: &str = "tenant.lookup";


/// Event emitted to ask the domains module to claim a subdomain
/// for a tenant. Answered by `saas_domains`; this module only
/// emits. The payload is `{"tenant_id", "company_id",
/// "subdomain", "domain", "package_id"?, "source": "tenants"}`.
pub const EVENT_SITE_PROVISION_REQUESTED: &str = "site.provision_requested";


/// Event asked to resolve a package id to its module list.
/// Answered by `saas_packages` with `{"module_ids": [...]}`.
pub const EVENT_PACKAGE_MODULES_REQUESTED: &str = "saas_packages.package.modules_requested";
