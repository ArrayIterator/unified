//! tenants - state.

use unified_core::modules::{ModuleRegistry};
use dashmap::DashMap;
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;
use super::{constants::*, vocabulary::*, events::*, settings::*, records::*, auth::*, helpers::*, provisioning::*, destroy::*, handlers::*, module::*, tenancy::*};



#[derive(Debug, Clone)]
// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------
/// Runtime state. The DashMap stores are the runtime authority for
/// this round (see the module header); the migration tables are the
/// durable target the integrator wires in. The maps are Arc-backed
/// so one `TenantsState` clone can be shared with the destroy
/// module (B3) through `TenantsModule::with_state`.
pub struct TenantsState {
    pub tenants: Arc<DashMap<Uuid, Tenant>>,
    pub steps: Arc<DashMap<String, ProvisioningStep>>,
    pub grants: Arc<DashMap<Uuid, ModuleGrant>>,
    pub settings: Arc<RwLock<TenantsSettings>>,
    /// Serializes the slug-uniqueness check and the tenant insert
    /// in `claim_tenant`, so two concurrent creates with the same
    /// slug cannot both pass the check before either inserts.
    pub slug_claim_lock: Arc<Mutex<()>>,
}


impl Default for TenantsState {
    fn default() -> Self {
        Self {
            tenants: Arc::new(DashMap::new()),
            steps: Arc::new(DashMap::new()),
            grants: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(TenantsSettings::default())),
            slug_claim_lock: Arc::new(Mutex::new(())),
        }
    }
}


/// Create a fresh, empty state. The integrator creates one state
/// and hands the same clone to this module and to the destroy
/// module so both see the same rows.
pub fn new_state() -> TenantsState {
    TenantsState::default()
}


impl TenantsState {
    /// Current settings (cloned; the stored value is the only
    /// source of truth for slug rules, grace, and quotas).
    pub fn settings(&self) -> TenantsSettings {
        self.settings
            .read()
            .expect("tenants settings lock poisoned")
            .clone()
    }

    /// Key for the provisioning-step map: one row per
    /// (tenant, step).
    pub(crate) fn step_key(tenant_id: Uuid, step: &str) -> String {
        format!("{tenant_id}|{step}")
    }

    /// Load a recorded step, if one exists.
    pub fn step(&self, tenant_id: Uuid, step: &str) -> Option<ProvisioningStep> {
        self.steps
            .get(&Self::step_key(tenant_id, step))
            .map(|entry| entry.value().clone())
    }

    /// Steps for one tenant, in pipeline order.
    pub fn steps_for(&self, tenant_id: Uuid) -> Vec<ProvisioningStep> {
        PROVISION_STEPS
            .iter()
            .filter_map(|step| self.step(tenant_id, step))
            .collect()
    }

    /// Live (unrevoked) grants for one tenant, ordered by module id.
    pub fn live_grants_for(&self, tenant_id: Uuid) -> Vec<ModuleGrant> {
        let mut grants: Vec<ModuleGrant> = self
            .grants
            .iter()
            .filter(|entry| {
                entry.value().tenant_id == tenant_id && entry.value().revoked_at.is_none()
            })
            .map(|entry| entry.value().clone())
            .collect();
        grants.sort_by(|a, b| a.module_id.cmp(&b.module_id));
        grants
    }

    /// A live grant for one (tenant, module), if one exists.
    pub fn live_grant(&self, tenant_id: Uuid, module_id: &str) -> Option<ModuleGrant> {
        self.grants.iter().find_map(|entry| {
            let grant = entry.value();
            if grant.tenant_id == tenant_id
                && grant.module_id == module_id
                && grant.revoked_at.is_none()
            {
                Some(grant.clone())
            } else {
                None
            }
        })
    }
}


pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(TenantsModule::new()));
}
