// Shared application context.
//
// The context is built once at startup and handed to every module, handler,
// and command. It carries the (validated) configuration, the hook bus, the
// module registry, the optional database pool, and the global Geo
// information variable: the result of the most recent Geo lookup for the
// current request context, so any module can read Geo data without a new
// lookup.

use crate::core::cache::{build_cache, Cache};
use crate::core::config::AppConfig;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::modules::ModuleRegistry;
use crate::core::routes::RouteRegistry;
use crate::core::storage::{build_storage_registry, StorageRegistry};
use crate::db::DbPool;
use crate::http::tls::SslStorage;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

/// Geo information resolved for an IP address.
///
/// `source` names where the data came from: `settings` for a settings
/// override, `geolite` for the GeoLite / MaxMind database, or `unknown`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct GeoInfo {
    pub ip: String,
    pub country_code: Option<String>,
    pub country_name: Option<String>,
    pub city: Option<String>,
    pub region: Option<String>,
    pub asn: Option<u32>,
    pub asn_organization: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub source: String,
}

pub struct AppContext {
    pub config: Arc<RwLock<AppConfig>>,
    pub hooks: Arc<HookBus>,
    pub modules: Arc<ModuleRegistry>,
    /// Shared cache (filesystem adapter by default, Redis when
    /// `redis.enabled` is true). Built once from the configuration.
    pub cache: Arc<Cache>,
    /// Global Geo information variable, updated by the Network / GEO
    /// module on every lookup.
    pub geo: Arc<RwLock<Option<GeoInfo>>>,
    /// Database pool, set once the database connection is established.
    pub db: Arc<RwLock<Option<DbPool>>>,
    /// Storage targets registered at startup from the `storage`
    /// config section. Built once from the configuration; targets
    /// registered at runtime (storage module) join the same registry.
    pub storage_registry: Arc<StorageRegistry>,
    /// Named page routes with their config and settings overrides,
    /// seeded from the `routes` config section at startup. The
    /// legacy `app.dashboard_path` value is honoured as a config
    /// override for `user_dashboard` when the `routes` section does
    /// not set it. This is the single registry the server's redirect
    /// handler and the theme engine resolve paths through; modules
    /// that keep their own registry (Routes, Dashboards) must be
    /// handed this same instance to stay in sync.
    pub routes: Arc<RouteRegistry>,
    /// Live TLS certificate storage, attached by the HTTP server
    /// once it builds the storage from `app.ssl`. Until then `None`
    /// and certificate lookups fall back to the configuration. One
    /// storage is shared by the HTTPS listener, the load balancer
    /// TLS endpoint, the certificates module, and the gateway: no
    /// second storage is ever built from the same configuration.
    pub tls: Arc<RwLock<Option<Arc<SslStorage>>>>,
    pub start_time: Instant,
    pub paused: AtomicBool,
    pub maintenance: AtomicBool,
}

impl AppContext {
    pub fn new(config: AppConfig, hooks: Arc<HookBus>, modules: Arc<ModuleRegistry>) -> Self {
        let cache = build_cache(&config);
        let storage_registry = build_storage_registry(&config.storage);
        let (routes, route_problems) =
            RouteRegistry::from_config_section(&config.routes);
        for problem in route_problems {
            log::warn!("Ignoring invalid routes config section: {problem}");
        }
        if config.routes.user_dashboard.is_none() {
            if let Some(problem) = routes.apply_legacy_dashboard_path(&config.app.dashboard_path)
            {
                log::warn!("Ignoring invalid app.dashboard_path: {problem}");
            }
        }
        Self {
            config: Arc::new(RwLock::new(config)),
            hooks,
            modules,
            cache,
            geo: Arc::new(RwLock::new(None)),
            db: Arc::new(RwLock::new(None)),
            storage_registry,
            routes,
            tls: Arc::new(RwLock::new(None)),
            start_time: Instant::now(),
            paused: AtomicBool::new(false),
            maintenance: AtomicBool::new(false),
        }
    }

    /// True when the application runs in development mode.
    pub fn is_debug(&self) -> bool {
        self.config
            .read()
            .expect("config lock poisoned")
            .is_debug()
    }

    /// Uptime in seconds since the process started.
    pub fn uptime_seconds(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    /// Update the global Geo information variable.
    pub fn set_geo(&self, info: GeoInfo) {
        *self.geo.write().expect("geo lock poisoned") = Some(info);
    }

    /// Read the global Geo information variable.
    pub fn geo(&self) -> Option<GeoInfo> {
        self.geo.read().expect("geo lock poisoned").clone()
    }

    /// Attach a database pool to the context.
    pub fn set_db(&self, pool: DbPool) {
        *self.db.write().expect("db lock poisoned") = Some(pool);
    }

    /// Attach the live TLS storage built by the HTTP server. Called
    /// once at startup; the same handle serves the HTTPS listener,
    /// the load balancer TLS endpoint, the certificates module, and
    /// the gateway.
    pub fn set_tls(&self, storage: Arc<SslStorage>) {
        *self.tls.write().expect("tls lock poisoned") = Some(storage);
    }

    /// Read the live TLS storage, if the HTTP server attached one.
    pub fn tls(&self) -> Option<Arc<SslStorage>> {
        self.tls.read().expect("tls lock poisoned").clone()
    }

    /// Replace the running configuration with a new one. The new
    /// configuration is validated first; on failure the running
    /// configuration is left untouched and the error is returned.
    pub fn reload_config(&self, new_config: AppConfig) -> UnifiedResult<()> {
        let problems = new_config.validate();
        if !problems.is_empty() {
            return Err(UnifiedError::bad_request(format!(
                "Configuration validation failed: {}",
                problems.join("; ")
            )));
        }
        {
            let mut config = self.config.write().expect("config lock poisoned");
            *config = new_config;
        }
        self.hooks.emit(
            crate::core::hooks::events::CONFIG_RELOAD,
            serde_json::json!({ "status": "reloaded" }),
        );
        Ok(())
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    pub fn is_maintenance(&self) -> bool {
        self.maintenance.load(Ordering::SeqCst)
    }

    pub fn set_maintenance(&self, maintenance: bool) {
        self.maintenance.store(maintenance, Ordering::SeqCst);
    }
}
