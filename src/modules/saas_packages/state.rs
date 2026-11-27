//! Saas Packages - In-memory state and its constructors.

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
use super::{constants::*, types::*, seed::*, auth::*, helpers::*, provisioning::*, handlers_packages::*, handlers_purchases::*, handlers_ai::*, events::*, module::*};

// In-memory stores (authoritative at runtime; see the module header).

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
    /// Paid-provisioning episodes, one entry per purchase: each
    /// episode is the set of paid identifiers one paid trigger
    /// named (`purchase:<id>` is always included, plus
    /// `order:<id>` and `subscription:<id>` when known). A later
    /// paid event whose identifiers are all already in one
    /// recorded episode is a duplicate delivery of the same paid
    /// flow and must not start another provisioning attempt. See
    /// `claim_paid_provision` in events.rs.
    pub paid_provision_episodes: Arc<DashMap<Uuid, Vec<Vec<String>>>>,
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
            paid_provision_episodes: Arc::new(DashMap::new()),
        }
    }
}

impl SaasPackagesState {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn registry(&self) -> Option<Arc<ModuleRegistry>> {
        self.modules.read().expect("modules lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit and return the handler context, so a caller can read
    /// back answers from hook queries (fail-closed shape: no answer
    /// means the queried permission or claim was not granted).
    pub(crate) fn emit_with_results(&self, event: &str, data: Value) -> Option<HookContext> {
        self.bus().map(|bus| bus.emit(event, data))
    }

    pub(crate) fn settings(&self) -> SaasPackagesSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    pub(crate) fn tenant_module_key(company_id: &Uuid, module_id: &str) -> String {
        format!("{company_id}|{module_id}")
    }

    pub(crate) fn tenant_has_module(&self, company_id: &Uuid, module_id: &str) -> bool {
        self.tenant_modules
            .get(&Self::tenant_module_key(company_id, module_id))
            .map(|entry| !entry.value().is_empty())
            .unwrap_or(false)
    }

    pub(crate) fn mark_tenant_module(&self, company_id: &Uuid, module_id: &str, purchase_id: Uuid) {
        let key = Self::tenant_module_key(company_id, module_id);
        self.tenant_modules
            .entry(key)
            .or_insert_with(Vec::new)
            .push(purchase_id);
    }

    pub(crate) fn unmark_tenant_module(&self, company_id: &Uuid, module_id: &str, purchase_id: Uuid) {
        let key = Self::tenant_module_key(company_id, module_id);
        if let Some(mut entry) = self.tenant_modules.get_mut(&key) {
            entry.retain(|id| *id != purchase_id);
        }
    }

    /// Other purchases (not `except`) that also enabled this module
    /// for the tenant. A module with other owners is shared and is
    /// never disabled by an upgrade/downgrade of one purchase.
    pub(crate) fn other_module_owners(&self, company_id: &Uuid, module_id: &str, except: Uuid) -> Vec<Uuid> {
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

    pub(crate) fn record_history(&self, purchase_id: Uuid, action: &str, actor_id: &str, detail: Value) {
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

    pub(crate) fn snapshot_version(package: &SaasPackage) -> PackageVersion {
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

    pub(crate) fn version_key(package_id: &Uuid, version: i32) -> String {
        format!("{package_id}|{version}")
    }

    pub(crate) fn store_version(&self, package: &SaasPackage) {
        let snapshot = Self::snapshot_version(package);
        self.versions
            .insert(Self::version_key(&snapshot.package_id, snapshot.version), snapshot);
    }

    /// Published packages, newest first, for AI grounding and the
    /// public list. No buyer data is ever included here.
    pub(crate) fn published_packages(&self) -> Vec<SaasPackage> {
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
