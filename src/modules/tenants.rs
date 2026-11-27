// Tenants - control-plane tenant registry, lifecycle, and provisioning.

// Split into one file per concern under `src/modules/tenants/`;
// this root only declares the submodules and re-exports the
// public API so every item stays reachable as `crate::modules::tenants::Item`.

pub mod constants;
pub mod vocabulary;
pub mod events;
pub mod settings;
pub mod records;
pub mod state;
pub mod auth;
pub mod helpers;
mod provisioning;
pub mod destroy;
mod handlers;
pub mod module;
pub mod tenancy;

pub use self::constants::{MODULE_ID, PERM_TENANTS_MANAGE, PERM_TENANTS_PROVISION, PERM_TENANTS_SUSPEND, PERM_TENANTS_VIEW};
pub use self::vocabulary::{PROVISION_STEPS, STATUS_ACTIVE, STATUS_DELETED, STATUS_DEPROVISIONING, STATUS_PROVISIONING, STATUS_PROVISION_FAILED, STATUS_SUSPENDED, STEP_ACTIVATE, STEP_COMPLETED, STEP_CREATE_RECORD, STEP_CREATE_SCHEMA, STEP_FAILED, STEP_GRANT_MODULES, STEP_PENDING, STEP_REGISTER_SUBDOMAIN, STEP_RUNNING, STEP_RUN_MIGRATIONS, STEP_SKIPPED, TENANT_STATUSES, TENANT_TRANSITIONS, transition_allowed};
pub use self::events::{EVENT_PACKAGE_MODULES_REQUESTED, EVENT_SITE_PROVISION_REQUESTED, EVENT_TENANT_CREATED, EVENT_TENANT_DELETED, EVENT_TENANT_DEPROVISIONING_STARTED, EVENT_TENANT_LOOKUP, EVENT_TENANT_MODULE_GRANTED, EVENT_TENANT_MODULE_REVOKED, EVENT_TENANT_MODULE_SKIPPED, EVENT_TENANT_PROVISIONED, EVENT_TENANT_PROVISION_FAILED, EVENT_TENANT_RESUMED, EVENT_TENANT_SETTINGS_UPDATED, EVENT_TENANT_SUSPENDED};
pub use self::settings::{TenantsSettings};
pub(crate) use self::settings::{default_max_tenants_per_owner, default_reserved_slugs, default_slug_max_length, default_slug_min_length, default_suspension_grace_days};
pub use self::records::{CreateTenantRequest, GrantModulesRequest, ModuleGrant, ProvisionStatus, ProvisioningStep, RevokeGrantRequest, SkippedModule, SuspendTenantRequest, Tenant, TenantContextView};
pub use self::state::{TenantsState, new_state, register};
pub use self::auth::{Caller};
pub(crate) use self::auth::{caller, check_capability, require_permission};
pub use self::helpers::{get_tenant};
pub(crate) use self::helpers::{created, db_pool, emit_audit, err, now, ok, paginate, transition_tenant, validate_slug};
pub(crate) use self::provisioning::{complete_step, fail_step, get_tenant_by_id, grant_modules_for_tenant, grant_one_module, granted_via_for, mark_provision_failed, record_step, register_subdomain_step, resolve_module_ids, run_provisioning_pipeline, skip_step, start_step};
pub use self::destroy::{begin_deprovision, finish_deprovision_as_deleted, set_export_reference};
pub(crate) use self::handlers::{create_tenant, get_provision_status, get_settings, get_tenant_context, get_tenant_handler, grant_tenant_modules, list_provision_steps, list_tenant_modules, list_tenants, resume_tenant, retry_provisioning, revoke_tenant_module, suspend_tenant, update_settings};
pub use self::module::{TenantsModule};
pub use self::tenancy::{tenant_context_for, tenant_context_schema, tenant_schema_name};
pub(crate) use self::tenancy::{ensure_tenant_schema, migrate_tenant_schema};
