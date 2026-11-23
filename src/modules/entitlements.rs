// Per-product feature limiting (entitlements) module.
//
// Split into submodules; the full module documentation lives in `module.rs`.

pub mod ai_capabilities;
pub mod constants;
mod validation;
pub mod types;
pub mod state;
mod helpers;
pub mod tenant;
mod handlers_packages;
mod handlers_admin;
mod handlers_ai;
mod hooks;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::constants::{EVENT_ENTITLEMENT_GRANTED, EVENT_ENTITLEMENT_REVOKED,
    EVENT_ENTITLEMENT_GRACE_STARTED, EVENT_ENTITLEMENT_DISABLED, EVENT_ENTITLEMENT_ENABLE_DENIED,
    EVENT_UPGRADE_PATH_REQUESTED, EVENT_MODULE_USAGE_REQUESTED, EVENT_CHECK_REQUESTED,
    EVENT_SAAS_ENTITLEMENT_CHECK_REQUESTED, EVENT_WRITE_CHECK_REQUESTED, EVENT_GRANT_REQUESTED,
    EVENT_AI_SUGGEST_REQUESTED, PERM_ENTITLEMENTS_READ, PERM_ENTITLEMENTS_MANAGE,
    PERM_ENTITLEMENTS_GRANT, SOURCE_LICENSE, SOURCE_SUBSCRIPTION, SOURCE_SAAS_PACKAGE,
    SOURCE_MANUAL_GRANT, SOURCE_OWNER_ENTITLEMENT, SOURCE_BUILTIN, STATUS_ACTIVE, STATUS_GRACE, STATUS_REVOKED,
    STATUS_EXPIRED, STATUS_DISABLED, ACCESS_ENTITLED, ACCESS_GRACE_READ_ONLY, ACCESS_LOCKED,
    ERROR_ENTITLEMENT_REQUIRED, ERROR_ENTITLEMENT_GRACE_READ_ONLY};
pub(crate) use self::constants::{LICENSE_REVOKED_EVENT, SUBSCRIPTION_CANCELLED_EVENT,
    SUBSCRIPTION_EXPIRED_EVENT, SAAS_PACKAGE_CANCELLED_EVENT, TENANT_MODULE_GRANTED_EVENT,
    TENANT_MODULE_REVOKED_EVENT, SOURCE_KINDS, GRANT_EMITTERS, DEFAULT_GRACE_PERIOD_DAYS, MAX_GRACE_PERIOD_DAYS,
    MAX_ID_LEN, MAX_MODULE_ID_LEN, MAX_SOURCE_ID_LEN, MAX_REASON_LEN, MAX_ACTOR_LEN,
    MAX_MODULES_PER_CHANGE, DENIED_LOOKBACK_DAYS, MAX_SUGGESTIONS};
pub(crate) use self::validation::{validate_module_id, validate_company_id, validate_source_kind,
    validate_source_id, validate_reason, validate_module_id_list};
pub use self::types::{Entitlement, EntitlementSourceView, EntitlementHistoryEntry, DeniedAttempt,
    PackageChange, ModuleActivation, EntitlementSettings, EffectiveEntitlement, MyModuleView,
    UpgradePath, UpgradeProduct, PackageSuggestion, ViewerQuery, CreateGrantRequest,
    UpdateGrantRequest, EnableModuleRequest, ApplyPackageChangeRequest, ConfirmPackageChangeRequest,
    AiSuggestRequest, SweepSummary};
pub use self::state::{EntitlementsState};
pub(crate) use self::state::{stash_ctx, hook_ctx, ENTITLEMENTS_CTX, entitled_via_live_sources,
    entitlements_state};
pub(crate) use self::helpers::{require_capability, CallerInfo, caller_of, admin_caller,
    resolve_grant_tenant_id, paginate, matches_search, require_tenant, caller_company};
pub use self::tenant::{TenantScope};
pub(crate) use self::tenant::{list_my_modules, get_module_access, enable_module, disable_module,
    set_activation};
pub(crate) use self::handlers_packages::{apply_package_change, list_package_changes,
    get_package_change, confirm_package_change};
pub(crate) use self::handlers_admin::{list_entitlements, create_grant, get_entitlement,
    update_grant, delete_grant, list_history, list_denied_attempts, get_settings, update_settings,
    run_sweep};
pub(crate) use self::handlers_ai::{ai_suggest, ai_capabilities};
pub(crate) use self::hooks::{hook_string, hook_string_from, on_check_requested,
    on_write_check_requested, on_grant_requested, on_ai_suggest_requested, on_license_revoked,
    on_license_expiring, on_subscription_cancelled, on_subscription_expired, on_package_cancelled,
    on_tenant_module_granted, on_tenant_module_revoked};
pub use self::module::{EntitlementsModule, register};
