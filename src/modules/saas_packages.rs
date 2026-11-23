// SaaS Packages module - sellable product bundles of modules.
//
// A package is data, never code: an admin or vendor defines a named bundle
// (module list, preset settings payload, wizard steps, and a link to a
// product/plan in the products module), publishes it, and customers buy it
// through the store / billing / subscription flow. The full route and
// behaviour documentation lives at the top of `saas_packages/module.rs`.

pub mod constants;
pub mod types;
pub mod state;
pub mod seed;
pub mod auth;
mod helpers;
mod provisioning;
mod handlers_packages;
mod handlers_purchases;
mod handlers_ai;
mod events;
pub mod module;

pub use self::constants::{PERM_SAAS_PACKAGES_VIEW, PERM_SAAS_PACKAGES_MANAGE, PERM_SAAS_PACKAGES_PURCHASE, PERM_SAAS_PACKAGES_PROVISION, EVENT_PACKAGE_CREATED, EVENT_PACKAGE_UPDATED, EVENT_PACKAGE_PUBLISHED, EVENT_PACKAGE_ARCHIVED, EVENT_PACKAGE_DELETED, EVENT_PACKAGE_CLONED, EVENT_PURCHASE_REQUESTED, EVENT_PURCHASE_COMPLETED, EVENT_PROVISION_STARTED, EVENT_PROVISION_STEP_COMPLETED, EVENT_PROVISION_COMPLETED, EVENT_PROVISION_FAILED, EVENT_MODULE_ENABLED, EVENT_MODULE_DISABLED, EVENT_UPGRADED, EVENT_DOWNGRADED, EVENT_CANCELLED, EVENT_RENEWAL_REQUESTED, EVENT_SITE_PROVISION_REQUESTED, EVENT_SETTINGS_APPLIED, EVENT_WIZARD_PREFILL_REQUESTED, EVENT_ENTITLEMENT_CHECK_REQUESTED, EVENT_AI_RECOMMEND_REQUESTED, EVENT_AI_DESCRIBE_REQUESTED, AI_CAPABILITIES};
pub use self::types::{SaasPackagesSettings, PackageModuleEntry, SaasPackage, PackageVersion, PackagePurchase, ProvisionStep, PurchaseHistoryEntry, ModuleProvisionOutcome, ProvisionSummary, PackageDiff};
pub use self::state::{SaasPackagesState};
pub use self::seed::{SEED_ONLINE_EXAM_PACKAGE_ID};
pub use self::auth::{Caller};
pub use self::module::{SaasPackagesModule, register};
