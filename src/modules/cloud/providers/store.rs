// providers module -- In-memory stores and the provider registry.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::json;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// In-memory stores (same pattern as the downloader job store).
// ---------------------------------------------------------------------------

pub(crate) static CONNECTIONS: OnceLock<DashMap<Uuid, ProviderConnection>> = OnceLock::new();

pub(crate) fn connections_store() -> &'static DashMap<Uuid, ProviderConnection> {
    CONNECTIONS.get_or_init(DashMap::new)
}

/// Cached inventory per connection, with a fetch timestamp.
#[derive(Debug, Clone)]
pub(crate) struct CachedInventory {
    pub(crate) servers: Vec<ServerInfo>,
    pub(crate) fetched_at: DateTime<Utc>,
    pub(crate) fetched_instant: Instant,
}

pub(crate) static INVENTORY: OnceLock<DashMap<Uuid, CachedInventory>> = OnceLock::new();

pub(crate) fn inventory_store() -> &'static DashMap<Uuid, CachedInventory> {
    INVENTORY.get_or_init(DashMap::new)
}

// ---------------------------------------------------------------------------
// Provider registry: the plug-in point for adapters.
// ---------------------------------------------------------------------------

/// Factory that builds an adapter for one stored connection.
pub type ProviderFactory = Arc<
    dyn Fn(&ProviderConnection) -> UnifiedResult<Arc<dyn CloudProvider>> + Send + Sync,
>;


pub struct ProviderRegistry {
    pub(crate) factories: RwLock<HashMap<ProviderKind, ProviderFactory>>,
}

impl ProviderRegistry {
    pub(crate) fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
        }
    }

    /// Register (or replace) the factory for a kind. This is how a new
    /// provider plugs in: call it once at startup, or from an extension,
    /// instead of editing this module.
    pub fn register_factory(&self, kind: ProviderKind, factory: ProviderFactory) {
        self.factories
            .write()
            .expect("provider registry lock poisoned")
            .insert(kind, factory);
    }

    pub fn build(&self, connection: &ProviderConnection) -> UnifiedResult<Arc<dyn CloudProvider>> {
        let factories = self
            .factories
            .read()
            .expect("provider registry lock poisoned");
        let factory = factories.get(&connection.kind).ok_or_else(|| {
            UnifiedError::internal(format!(
                "No provider adapter registered for kind \"{}\"",
                connection.kind.as_str()
            ))
        })?;
        factory(connection)
    }

    pub fn registered_kinds(&self) -> Vec<ProviderKind> {
        let factories = self
            .factories
            .read()
            .expect("provider registry lock poisoned");
        let mut kinds: Vec<ProviderKind> = factories.keys().copied().collect();
        kinds.sort_by_key(|kind| kind.as_str());
        kinds
    }
}

pub(crate) static REGISTRY: OnceLock<ProviderRegistry> = OnceLock::new();

/// The shared registry, with the built-in adapters registered on
/// first use.
pub fn provider_registry() -> &'static ProviderRegistry {
    REGISTRY.get_or_init(|| {
        let registry = ProviderRegistry::new();
        registry.register_factory(
            ProviderKind::Hetzner,
            Arc::new(|connection| {
                Ok(Arc::new(HetznerAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Ovh,
            Arc::new(|connection| {
                Ok(Arc::new(OvhAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Tencent,
            Arc::new(|connection| {
                Ok(Arc::new(TencentAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Huawei,
            Arc::new(|connection| {
                Ok(Arc::new(HuaweiAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Aws,
            Arc::new(|connection| {
                Ok(Arc::new(AwsAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Google,
            Arc::new(|connection| {
                Ok(Arc::new(GoogleAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Virtualizor,
            Arc::new(|connection| {
                Ok(Arc::new(VirtualizorAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Docker,
            Arc::new(|connection| {
                Ok(Arc::new(DockerAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Kubernetes,
            Arc::new(|connection| {
                Ok(Arc::new(KubernetesAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Redshift,
            Arc::new(|connection| {
                Ok(Arc::new(RedshiftAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::DigitalOcean,
            Arc::new(|connection| {
                Ok(Arc::new(DigitalOceanAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Linode,
            Arc::new(|connection| {
                Ok(Arc::new(LinodeAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::OpenStack,
            Arc::new(|connection| {
                Ok(Arc::new(OpenStackAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Proxmox,
            Arc::new(|connection| {
                Ok(Arc::new(ProxmoxAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Kvm,
            Arc::new(|connection| {
                Ok(Arc::new(KvmAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Xen,
            Arc::new(|connection| {
                Ok(Arc::new(XenAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry.register_factory(
            ProviderKind::Custom,
            Arc::new(|connection| {
                Ok(Arc::new(CustomAdapter::new(connection)?) as Arc<dyn CloudProvider>)
            }),
        );
        registry
    })
}

// ---------------------------------------------------------------------------
// Inventory cache and sync.
// ---------------------------------------------------------------------------

pub(crate) fn inventory_ttl() -> Duration {
    Duration::from_secs(DEFAULT_INVENTORY_TTL_SECS)
}

/// Read the cached inventory, or fetch live from the provider and
/// cache the result. `force_refresh` skips the cache.
pub(crate) async fn get_inventory(
    connection: &ProviderConnection,
    force_refresh: bool,
) -> UnifiedResult<(Vec<ServerInfo>, bool, Option<DateTime<Utc>>)> {
    if !force_refresh {
        if let Some(cached) = inventory_store().get(&connection.id) {
            if cached.fetched_instant.elapsed() < inventory_ttl() {
                return Ok((cached.servers.clone(), true, Some(cached.fetched_at)));
            }
        }
    }
    let adapter = provider_registry().build(connection)?;
    let servers = adapter.list_servers().await?;
    inventory_store().insert(
        connection.id,
        CachedInventory {
            servers: servers.clone(),
            fetched_at: Utc::now(),
            fetched_instant: Instant::now(),
        },
    );
    Ok((servers, false, Some(Utc::now())))
}

/// Refresh one connection's inventory and emit the sync events. Used
/// by the sync-now endpoint.

pub(crate) async fn sync_connection(ctx: &Arc<AppContext>, connection: &ProviderConnection) -> UnifiedResult<usize> {
    let result = get_inventory(connection, true).await;
    match result {
        Ok((servers, _, _)) => {
            let count = servers.len();
            if let Some(mut stored) = connections_store().get_mut(&connection.id) {
                stored.last_status = Some("synced".to_string());
                stored.updated_at = Utc::now();
            }
            ctx.hooks.emit(
                PROVIDER_SYNC_COMPLETED,
                serde_json::json!({
                    "actor": "api",
                    "connection_id": connection.id,
                    "kind": connection.kind.as_str(),
                    "resource": format!("provider-connection:{}", connection.id),
                    "server_count": count,
                }),
            );
            Ok(count)
        }
        Err(error) => {
            if let Some(mut stored) = connections_store().get_mut(&connection.id) {
                stored.last_status = Some(format!("sync failed: {}", error.message));
                stored.updated_at = Utc::now();
            }
            ctx.hooks.emit(
                PROVIDER_SYNC_FAILED,
                serde_json::json!({
                    "actor": "api",
                    "connection_id": connection.id,
                    "kind": connection.kind.as_str(),
                    "resource": format!("provider-connection:{}", connection.id),
                    "error": error.message,
                }),
            );
            Err(error)
        }
    }
}
