//! Saas Packages - Shared types: request and response bodies, views, and state shapes.

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
use super::{constants::*, state::*, seed::*, auth::*, helpers::*, provisioning::*, handlers_packages::*, handlers_purchases::*, handlers_ai::*, events::*, module::*};

// Configurable settings (admin-editable; defaults are defaults only)

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
    pub(crate) fn validate(&self) -> UnifiedResult<()> {
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

// Stored records.

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
/// code. Money never lives here -- `product_id` is only a reference
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

// Request bodies.

#[derive(Debug, Deserialize)]
pub(crate) struct PackageModuleInput {
    pub(crate) module_id: String,
    #[serde(default = "default_required")]
    pub(crate) required: bool,
    #[serde(default)]
    pub(crate) sort_order: Option<i32>,
}

pub(crate) fn default_required() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreatePackageRequest {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) slug: Option<String>,
    #[serde(default)]
    pub(crate) product_id: Option<String>,
    #[serde(default)]
    pub(crate) preset_settings: Option<Value>,
    #[serde(default)]
    pub(crate) wizard_steps: Option<Vec<String>>,
    pub(crate) modules: Vec<PackageModuleInput>,
    #[serde(default)]
    pub(crate) vendor_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdatePackageRequest {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) product_id: Option<String>,
    #[serde(default)]
    pub(crate) preset_settings: Option<Value>,
    #[serde(default)]
    pub(crate) wizard_steps: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) modules: Option<Vec<PackageModuleInput>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PurchaseRequest {
    #[serde(default)]
    pub(crate) buyer_id: Option<String>,
    pub(crate) company_id: String,
    #[serde(default)]
    pub(crate) subdomain: Option<String>,
    #[serde(default)]
    pub(crate) order_id: Option<String>,
    #[serde(default)]
    pub(crate) subscription_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CancelPurchaseRequest {
    #[serde(default)]
    pub(crate) reason: Option<String>,
    #[serde(default)]
    pub(crate) actor_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChangePackageRequest {
    pub(crate) target_package_id: String,
    #[serde(default)]
    pub(crate) confirm_disable: bool,
    #[serde(default)]
    pub(crate) modules_to_disable: Vec<String>,
    #[serde(default)]
    pub(crate) actor_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ValidatePackageRequest {
    #[serde(default)]
    pub(crate) company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AiRecommendRequest {
    #[serde(default)]
    pub(crate) business_type: Option<String>,
    #[serde(default)]
    pub(crate) needs: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AiDescribeRequest {
    pub(crate) package_id: String,
    #[serde(default)]
    pub(crate) company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SettingsUpdateRequest {
    #[serde(default)]
    pub(crate) grace_period_days: Option<i64>,
    #[serde(default)]
    pub(crate) max_modules_per_package: Option<usize>,
    #[serde(default)]
    pub(crate) max_wizard_steps: Option<usize>,
    #[serde(default)]
    pub(crate) provisioning_max_attempts: Option<i32>,
    #[serde(default)]
    pub(crate) subdomain_base_domain: Option<String>,
    #[serde(default)]
    pub(crate) vendor_packages_enabled: Option<bool>,
    #[serde(default)]
    pub(crate) wizard_step_names: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_is_valid() {
        assert!(SaasPackagesSettings::default().validate().is_ok());
    }

    #[test]
    fn settings_validate_rejects_bad_values() {
        let mut settings = SaasPackagesSettings::default();
        settings.grace_period_days = -1;
        assert!(settings.validate().is_err());
        let mut settings = SaasPackagesSettings::default();
        settings.grace_period_days = 366;
        assert!(settings.validate().is_err());
        let mut settings = SaasPackagesSettings::default();
        settings.max_modules_per_package = 0;
        assert!(settings.validate().is_err());
        let mut settings = SaasPackagesSettings::default();
        settings.max_wizard_steps = 21;
        assert!(settings.validate().is_err());
        let mut settings = SaasPackagesSettings::default();
        settings.provisioning_max_attempts = 0;
        assert!(settings.validate().is_err());
        let mut settings = SaasPackagesSettings::default();
        settings.subdomain_base_domain = "x".repeat(501);
        assert!(settings.validate().is_err());
    }
}
