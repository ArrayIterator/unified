// Shared state.

use unified_core::config::AppConfig;
use unified_core::hooks::HookBus;
use dashmap::DashMap;
use serde_json::Value;
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Shared state.
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct StorageManagerState {
    pub buckets: Arc<DashMap<Uuid, StorageBucket>>,
    pub bindings: Arc<DashMap<Uuid, StorageBinding>>,
    /// Keyed by `{scope}:{scope_id}`.
    pub quotas: Arc<DashMap<String, StorageQuota>>,
    /// Keyed by `{date}|{bucket_id}|{company_id}|{user_id}|{module_id}`.
    pub usage_daily: Arc<DashMap<String, UsageDaily>>,
    pub lifecycle_rules: Arc<DashMap<Uuid, LifecycleRule>>,
    pub signed_urls: Arc<DashMap<Uuid, SignedUrlRecord>>,
    pub health_checks: Arc<DashMap<Uuid, HealthCheck>>,
    /// Keyed by `{bucket_id}:{key}`.
    pub objects: Arc<DashMap<String, StoredObject>>,
    pub global_default: Arc<RwLock<Option<Uuid>>>,
    /// Company id -> default bucket id.
    pub company_defaults: Arc<DashMap<String, Uuid>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
    /// Application config handle, captured at `init`. Hook handlers
    /// have no `AppContext`, so they build providers from this
    /// handle instead (see `encryption_secret_from`).
    pub app_config: Arc<RwLock<Option<Arc<RwLock<AppConfig>>>>>,
    /// Storage plans sold as products (see the plans section).
    pub plans: Arc<DashMap<Uuid, StoragePlan>>,
    /// Active / cancelled plan assignments, keyed by assignment id.
    /// Cancelled rows are retained (never overwritten by a later
    /// assignment), so an old assignment id stays resolvable and
    /// its history is preserved; the active assignment for a buyer
    /// is found by scanning for the buyer key (see
    /// `active_assignment_for_buyer`).
    pub plan_assignments: Arc<DashMap<Uuid, PlanAssignment>>,
    /// Metering reports emitted for billing (append-only history).
    pub metering_reports: Arc<DashMap<Uuid, MeteringReport>>,
    /// Bring-your-own storage connections, keyed by connection id.
    pub byo_connections: Arc<DashMap<Uuid, ByoConnection>>,
    /// BYO objects recorded through a connection, keyed by
    /// `{connection_id}:{key}`.
    pub byo_objects: Arc<DashMap<String, ByoObject>>,
    /// BYO terms versions, keyed by version id.
    pub byo_terms: Arc<DashMap<Uuid, ByoTerms>>,
    /// Terms acceptances, keyed by `{user_id}|{terms_version}`.
    pub byo_acceptances: Arc<DashMap<String, ByoTermsAcceptance>>,
    /// BYO health-check runs, keyed by check id.
    pub byo_health_checks: Arc<DashMap<Uuid, ByoHealthCheck>>,
    /// Admin-managed BYO settings (provider allowlist, limits).
    pub byo_settings: Arc<RwLock<ByoSettings>>,
    /// Serializes the quota check + reservation step of hub uploads
    /// (see `check_quotas` / `reserve_quota`). The guard is only
    /// ever held across synchronous work — never across a provider
    /// call — so two concurrent uploads cannot both pass the check
    /// against the same counters and then both store.
    pub quota_guard: Arc<Mutex<()>>,
    /// In-flight plan-quota reservations, keyed by
    /// `{company_id}|{user_id}` (see `assignment_key`): bytes and
    /// file counts reserved by uploads that passed the plan check
    /// but have not recorded their object yet. Plan checks add the
    /// reservation to the recorded usage, so the check-then-store
    /// race cannot over-admit a buyer past their plan quota.
    pub plan_reserved: Arc<DashMap<String, (i64, i64)>>,
}

impl Default for StorageManagerState {
    fn default() -> Self {
        Self {
            buckets: Arc::new(DashMap::new()),
            bindings: Arc::new(DashMap::new()),
            quotas: Arc::new(DashMap::new()),
            usage_daily: Arc::new(DashMap::new()),
            lifecycle_rules: Arc::new(DashMap::new()),
            signed_urls: Arc::new(DashMap::new()),
            health_checks: Arc::new(DashMap::new()),
            objects: Arc::new(DashMap::new()),
            global_default: Arc::new(RwLock::new(None)),
            company_defaults: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
            app_config: Arc::new(RwLock::new(None)),
            plans: Arc::new(DashMap::new()),
            plan_assignments: Arc::new(DashMap::new()),
            metering_reports: Arc::new(DashMap::new()),
            byo_connections: Arc::new(DashMap::new()),
            byo_objects: Arc::new(DashMap::new()),
            byo_terms: Arc::new(DashMap::new()),
            byo_acceptances: Arc::new(DashMap::new()),
            byo_health_checks: Arc::new(DashMap::new()),
            byo_settings: Arc::new(RwLock::new(ByoSettings::default())),
            quota_guard: Arc::new(Mutex::new(())),
            plan_reserved: Arc::new(DashMap::new()),
        }
    }
}

impl StorageManagerState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }
}
