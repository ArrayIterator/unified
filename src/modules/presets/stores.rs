//! Presets + Setup Wizard module. — In-memory stores (authoritative at runtime; see the module header).
//!
//! Split from `src/modules/presets.rs`; behaviour unchanged.

use super::apply::*;
use super::events::*;
use super::helpers::*;
use super::identity::*;
use super::lookups::*;
use super::preset_handlers::*;
use super::records::*;
use super::requests::*;
use super::seeds::*;
use super::wizard_handlers::*;
use super::wizard_steps::*;

use unified_core::modules::ModuleRegistry;
use dashmap::DashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct PresetsState {
    pub presets: Arc<DashMap<Uuid, Preset>>,
    /// Wizard state keyed by tenant (company id): one wizard per
    /// tenant, resumable across requests.
    pub wizards: Arc<DashMap<Uuid, WizardState>>,
    pub applications: Arc<DashMap<Uuid, PresetApplication>>,
    /// Modules a preset application enabled, keyed
    /// `"<company_id>|<module_id>"`. This is this module's own
    /// record of what it enabled; modules enabled through the
    /// Extensions Manager are not visible here (see OPEN notes).
    pub tenant_modules: Arc<DashMap<String, bool>>,
}

impl PresetsState {
    pub fn new() -> Self {
        Self {
            presets: Arc::new(DashMap::new()),
            wizards: Arc::new(DashMap::new()),
            applications: Arc::new(DashMap::new()),
            tenant_modules: Arc::new(DashMap::new()),
        }
    }

    pub(super) fn tenant_module_key(company_id: &Uuid, module_id: &str) -> String {
        format!("{company_id}|{module_id}")
    }

    pub fn tenant_has_module(&self, company_id: &Uuid, module_id: &str) -> bool {
        self.tenant_modules
            .get(&Self::tenant_module_key(company_id, module_id))
            .map(|entry| *entry.value())
            .unwrap_or(false)
    }

    pub fn mark_tenant_module(&self, company_id: &Uuid, module_id: &str) {
        self.tenant_modules
            .insert(Self::tenant_module_key(company_id, module_id), true);
    }
}

impl Default for PresetsState {
    fn default() -> Self {
        Self::new()
    }
}

pub struct PresetsModule {
    pub(super) state: PresetsState,
}

impl PresetsModule {
    pub fn new() -> Self {
        Self {
            state: PresetsState::new(),
        }
    }
}

impl Default for PresetsModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PresetsModule::new()));
}
