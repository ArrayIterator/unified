//! panels module - Panel adapter registry.

use unified_core::error::{UnifiedError, UnifiedResult};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use super::adapter::PanelAdapter;
use super::adapters_cpanel::CpanelAdapter;
use super::adapters_cyberpanel::CyberPanelAdapter;
use super::adapters_directadmin::DirectAdminAdapter;
use super::adapters_generic::GenericAdapter;
use super::adapters_plesk::PleskAdapter;
use super::adapters_whmcs::WhmcsAdapter;
use super::types::PanelConnection;
use super::types::PanelKind;

// ---------------------------------------------------------------------------
// Panel registry: the plug-in point for adapters.
// ---------------------------------------------------------------------------

/// Factory that builds an adapter for one stored connection.
pub type PanelFactory =
    Arc<dyn Fn(&PanelConnection) -> UnifiedResult<Arc<dyn PanelAdapter>> + Send + Sync>;

pub struct PanelRegistry {
    pub(crate) factories: RwLock<HashMap<PanelKind, PanelFactory>>,
}

impl PanelRegistry {
    pub(crate) fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
        }
    }

    /// Register (or replace) the factory for a kind. This is how a new
    /// panel plugs in: call it once at startup, or from an extension,
    /// instead of editing this module.
    pub fn register_factory(&self, kind: PanelKind, factory: PanelFactory) {
        self.factories
            .write()
            .expect("panel registry lock poisoned")
            .insert(kind, factory);
    }

    pub fn build(&self, connection: &PanelConnection) -> UnifiedResult<Arc<dyn PanelAdapter>> {
        let factories = self.factories.read().expect("panel registry lock poisoned");
        let factory = factories.get(&connection.kind).ok_or_else(|| {
            UnifiedError::internal(format!(
                "No panel adapter registered for kind \"{}\"",
                connection.kind.as_str()
            ))
        })?;
        factory(connection)
    }

    pub fn registered_kinds(&self) -> Vec<PanelKind> {
        let factories = self.factories.read().expect("panel registry lock poisoned");
        let mut kinds: Vec<PanelKind> = factories.keys().copied().collect();
        kinds.sort_by_key(|kind| kind.as_str());
        kinds
    }
}

pub(crate) static REGISTRY: OnceLock<PanelRegistry> = OnceLock::new();

/// The shared registry, with the six built-in adapters registered on
/// first use.
pub fn panel_registry() -> &'static PanelRegistry {
    REGISTRY.get_or_init(|| {
        let registry = PanelRegistry::new();
        registry.register_factory(
            PanelKind::Whmcs,
            Arc::new(|connection| {
                Ok(Arc::new(WhmcsAdapter::new(connection)?) as Arc<dyn PanelAdapter>)
            }),
        );
        registry.register_factory(
            PanelKind::Cpanel,
            Arc::new(|connection| {
                Ok(Arc::new(CpanelAdapter::new(connection)?) as Arc<dyn PanelAdapter>)
            }),
        );
        registry.register_factory(
            PanelKind::DirectAdmin,
            Arc::new(|connection| {
                Ok(Arc::new(DirectAdminAdapter::new(connection)?) as Arc<dyn PanelAdapter>)
            }),
        );
        registry.register_factory(
            PanelKind::Plesk,
            Arc::new(|connection| {
                Ok(Arc::new(PleskAdapter::new(connection)?) as Arc<dyn PanelAdapter>)
            }),
        );
        registry.register_factory(
            PanelKind::CyberPanel,
            Arc::new(|connection| {
                Ok(Arc::new(CyberPanelAdapter::new(connection)?) as Arc<dyn PanelAdapter>)
            }),
        );
        registry.register_factory(
            PanelKind::Generic,
            Arc::new(|connection| {
                Ok(Arc::new(GenericAdapter::new(connection)?) as Arc<dyn PanelAdapter>)
            }),
        );
        registry
    })
}
