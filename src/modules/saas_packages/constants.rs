//! Saas Packages - Constants: limits, capability names, event names, statuses.

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
use super::{types::*, state::*, seed::*, auth::*, helpers::*, provisioning::*, handlers_packages::*, handlers_purchases::*, handlers_ai::*, events::*, module::*};

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_SLUG_LEN: usize = 100;

pub(crate) const MAX_REF_LEN: usize = 500;

pub(crate) const MAX_TEXT_LEN: usize = 20000;

pub(crate) const MAX_SUBDOMAIN_LEN: usize = 63;

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
pub(crate) const ORDER_PAID: &str = "order.paid";

pub(crate) const STORE_ORDER_COMPLETED: &str = "store.order.completed";

pub(crate) const STORE_ORDER_REFUNDED: &str = "store.order.refunded";

pub(crate) const STORE_ORDER_CANCELLED: &str = "store.order.cancelled";

pub(crate) const SUBSCRIPTION_ACTIVATED: &str = "subscription.activated";

pub(crate) const SUBSCRIPTION_CANCELLED: &str = "subscription.cancelled";

pub(crate) const SUBSCRIPTION_EXPIRED: &str = "subscription.expired";

/// Package lifecycle statuses. Closed vocabulary: a package is a
/// draft until published, and archived packages are never sold.
pub(crate) const STATUS_DRAFT: &str = "draft";

pub(crate) const STATUS_PUBLISHED: &str = "published";

pub(crate) const STATUS_ARCHIVED: &str = "archived";

/// Purchase lifecycle statuses.
pub(crate) const PURCHASE_PENDING_PAYMENT: &str = "pending_payment";

pub(crate) const PURCHASE_PROVISIONING: &str = "provisioning";

pub(crate) const PURCHASE_ACTIVE: &str = "active";

pub(crate) const PURCHASE_FAILED: &str = "failed";

pub(crate) const PURCHASE_SUSPENDED: &str = "suspended";

pub(crate) const PURCHASE_GRACE_PERIOD: &str = "grace_period";

pub(crate) const PURCHASE_CANCELLED: &str = "cancelled";

pub(crate) const PURCHASE_EXPIRED: &str = "expired";

/// Provisioning pipeline step names, in execution order. These are
/// the fixed pipeline of this module (a standard constant, not a
/// business list): every provisioning run executes these six steps
/// in this order, each recorded with its own outcome.
pub(crate) const STEP_VALIDATE_ENTITLEMENTS: &str = "validate_entitlements";

pub(crate) const STEP_PROVISION_SUBDOMAIN: &str = "provision_subdomain";

pub(crate) const STEP_ENABLE_MODULES: &str = "enable_modules";

pub(crate) const STEP_APPLY_PRESET_SETTINGS: &str = "apply_preset_settings";

pub(crate) const STEP_RUN_SETUP_WIZARD: &str = "run_setup_wizard";

pub(crate) const STEP_SEND_WELCOME: &str = "send_welcome";

pub(crate) const PROVISION_STEPS: &[&str] = &[
    STEP_VALIDATE_ENTITLEMENTS,
    STEP_PROVISION_SUBDOMAIN,
    STEP_ENABLE_MODULES,
    STEP_APPLY_PRESET_SETTINGS,
    STEP_RUN_SETUP_WIZARD,
    STEP_SEND_WELCOME,
];

/// Provisioning step statuses.
pub(crate) const STEP_PENDING: &str = "pending";

pub(crate) const STEP_RUNNING: &str = "running";

pub(crate) const STEP_COMPLETED: &str = "completed";

pub(crate) const STEP_FAILED: &str = "failed";

pub(crate) const STEP_SKIPPED: &str = "skipped";

pub(crate) const STEP_BLOCKED: &str = "blocked";

/// Per-module enable outcomes, mirroring the presets module so the
/// two modules report the same words for the same situations.
pub(crate) const OUTCOME_ENABLED: &str = "enabled";

pub(crate) const OUTCOME_ALREADY_ENABLED: &str = "already_enabled";

pub(crate) const OUTCOME_SKIPPED_NOT_ENTITLED: &str = "skipped_not_entitled";

pub(crate) const OUTCOME_SKIPPED_DEPENDENCY_MISSING: &str = "skipped_dependency_missing";

pub(crate) const OUTCOME_FAILED: &str = "failed";

/// The step finished as a handoff: the request was emitted and no
/// owning module has claimed it yet (see the OPEN notes).
pub(crate) const OUTCOME_REQUESTED: &str = "requested";

// AI capabilities (smart module).

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
