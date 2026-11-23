// Whitelabel module - in-memory state.
//
// Split from `src/modules/whitelabel.rs`; the module root re-exports the public API of this module.

use unified_core::hooks::HookBus;
use chrono::Utc;
use dashmap::DashMap;
use serde_json::Value;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::service::{effective_allowance, resolve_branding};
use super::types::{AllowanceMode, BrandingProfile, CompanyAllowance, GlobalAllowance, GlobalDefaults, HistoryEntry, ResolvedBranding};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct WhitelabelState {
    pub global_allowance: Arc<RwLock<GlobalAllowance>>,
    pub company_allowances: Arc<DashMap<String, CompanyAllowance>>,
    /// Admin-editable platform defaults record, seeded once at
    /// construction (see `GlobalDefaults::seed`). The stored record
    /// is the source of truth; the seed is never consulted after
    /// an admin saves defaults.
    pub defaults: Arc<RwLock<GlobalDefaults>>,
    /// Profiles keyed by `(company_id, tenant_id)` with `""` for the
    /// company default (no tenant).
    pub profiles: Arc<DashMap<(String, String), BrandingProfile>>,
    pub history: Arc<DashMap<Uuid, HistoryEntry>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl WhitelabelState {
    pub fn new() -> Self {
        Self {
            global_allowance: Arc::new(RwLock::new(GlobalAllowance {
                allow_whitelabel: false,
                updated_by: None,
                updated_at: Utc::now(),
            })),
            company_allowances: Arc::new(DashMap::new()),
            defaults: Arc::new(RwLock::new(GlobalDefaults::seed())),
            profiles: Arc::new(DashMap::new()),
            history: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }

    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(crate) fn profile_key(company_id: &str, tenant_id: &Option<String>) -> (String, String) {
        (
            company_id.to_string(),
            tenant_id.clone().unwrap_or_default(),
        )
    }

    pub(crate) fn get_profile(&self, company_id: &str, tenant_id: &Option<String>) -> Option<BrandingProfile> {
        self.profiles
            .get(&Self::profile_key(company_id, tenant_id))
            .map(|entry| entry.value().clone())
    }

    pub(crate) fn company_mode(&self, company_id: &str) -> Option<AllowanceMode> {
        self.company_allowances
            .get(company_id)
            .map(|entry| entry.value().mode)
    }

    pub(crate) fn global_allow(&self) -> bool {
        self.global_allowance
            .read()
            .expect("allowance lock poisoned")
            .allow_whitelabel
    }

    pub(crate) fn global_defaults(&self) -> GlobalDefaults {
        self.defaults.read().expect("defaults lock poisoned").clone()
    }

    /// Effective allowance for a company: the per-company override
    /// wins unless it is `inherit`, in which case the global default
    /// applies.
    pub(crate) fn effective_allowance(&self, company_id: &str) -> bool {
        effective_allowance(self.global_allow(), self.company_mode(company_id))
    }

    /// Server-side branding resolution for one request: tenant
    /// override -> company default -> global defaults record, with
    /// the allowance enforced (not allowed means the global defaults
    /// record in full).
    pub(crate) fn resolve_for(&self, company_id: &str, tenant_id: &Option<String>) -> ResolvedBranding {
        let tenant = self.get_profile(company_id, tenant_id);
        let company = if tenant_id.is_some() {
            self.get_profile(company_id, &None)
        } else {
            None
        };
        let allowed = self.effective_allowance(company_id);
        resolve_branding(
            company_id,
            tenant_id,
            tenant.as_ref(),
            company.as_ref(),
            &self.global_defaults(),
            allowed,
        )
    }

    pub(crate) fn record_history(
        &self,
        company_id: &str,
        tenant_id: &Option<String>,
        actor: &str,
        action: &str,
        field: &str,
        old_value: Option<String>,
        new_value: Option<String>,
        allowed: bool,
    ) {
        let entry_id = Uuid::new_v4();
        self.history.insert(
            entry_id,
            HistoryEntry {
                id: entry_id,
                company_id: company_id.to_string(),
                tenant_id: tenant_id.clone(),
                actor: actor.to_string(),
                action: action.to_string(),
                field: field.to_string(),
                old_value,
                new_value,
                allowed,
                created_at: Utc::now(),
            },
        );
    }
}

impl Default for WhitelabelState {
    fn default() -> Self {
        Self::new()
    }
}
