// Scheduler - In-memory scheduler state.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde_json::Value;
use std::sync::{Arc, RwLock, atomic::AtomicBool};
use std::time::Instant;
use unified_core::cache::Cache;
use unified_core::config::AppConfig;
use unified_core::context::{AppContext, GeoInfo};
use unified_core::db::DbPool;
use unified_core::hooks::HookBus;
use unified_core::modules::ModuleRegistry;
use unified_core::routes::RouteRegistry;
use unified_core::storage::StorageRegistry;
use unified_core::tls::SslStorage;

use super::{Job, JobRun, QueueItem};

// ---------------------------------------------------------------------------
// Context handles
// ---------------------------------------------------------------------------

/// The shareable handles of an `AppContext`, cloned by
/// `Module::init` into `SchedulerState::context`.
///
/// `AppContext` itself is not `Clone` (its `AtomicBool` pause /
/// maintenance flags cannot be cloned), so the state keeps the
/// individual `Arc` handles instead and `app_context` rebuilds
/// the context from them on demand. Every handle is the live one
/// from `init`, so the rebuilt context shares the database pool,
/// configuration, hook bus, cache, storage registry, routes, and
/// TLS storage with the running application. Only `start_time`
/// and the pause / maintenance flags are fresh in the rebuild,
/// and no consumer on the scheduler path reads them: the destroy
/// sweep uses `hooks` (events, tenant lookup) and `db` (the schema
/// drop) only.
#[derive(Clone)]
pub(crate) struct ContextHandles {
    pub(crate) config: Arc<RwLock<AppConfig>>,
    pub(crate) hooks: Arc<HookBus>,
    pub(crate) modules: Arc<ModuleRegistry>,
    pub(crate) cache: Arc<Cache>,
    pub(crate) geo: Arc<RwLock<Option<GeoInfo>>>,
    pub(crate) db: Arc<RwLock<Option<DbPool>>>,
    pub(crate) storage_registry: Arc<StorageRegistry>,
    pub(crate) routes: Arc<RouteRegistry>,
    pub(crate) tls: Arc<RwLock<Option<Arc<SslStorage>>>>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct SchedulerState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    /// Live application-context handles stored by `Module::init`
    /// (see `ContextHandles`); `None` until `init` runs, so a
    /// hand-built state has no context and any dispatch arm that
    /// needs one fails closed instead of running against a
    /// half-built context.
    pub(crate) context: RwLock<Option<ContextHandles>>,
    pub(crate) jobs: DashMap<String, Job>,
    pub(crate) runs: DashMap<String, JobRun>,
    pub(crate) queue: DashMap<String, QueueItem>,
    /// Last dispatch time per subscription for the
    /// `payment.subscription.renew` loop guard (see `dispatch`).
    pub(crate) renew_guard: DashMap<String, std::time::Instant>,
    /// Last fire time per job id, for the ticker (`run_due_jobs`):
    /// an interval job is due again once its interval elapsed since
    /// this timestamp (or since `created_at` when it never fired),
    /// and a cron job must not fire twice in the same minute.
    pub(crate) last_run: DashMap<String, DateTime<Utc>>,
    /// Set once by `start_ticker` so `Module::init` can never start
    /// the background ticker twice.
    pub(crate) ticker_started: AtomicBool,
}

impl SchedulerState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            context: RwLock::new(None),
            jobs: DashMap::new(),
            runs: DashMap::new(),
            queue: DashMap::new(),
            renew_guard: DashMap::new(),
            last_run: DashMap::new(),
            ticker_started: AtomicBool::new(false),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    /// Store the live context's handles. Called once by
    /// `Module::init`; storing again replaces the handles, so a
    /// second `init` keeps the newest live context.
    pub(crate) fn set_context(&self, ctx: &AppContext) {
        *self.context.write().expect("context lock poisoned") = Some(ContextHandles {
            config: ctx.config.clone(),
            hooks: ctx.hooks.clone(),
            modules: ctx.modules.clone(),
            cache: ctx.cache.clone(),
            geo: ctx.geo.clone(),
            db: ctx.db.clone(),
            storage_registry: ctx.storage_registry.clone(),
            routes: ctx.routes.clone(),
            tls: ctx.tls.clone(),
        });
    }

    /// Rebuild the live `AppContext` from the handles stored by
    /// `Module::init`, or `None` when `init` never ran. The
    /// rebuild shares every `Arc` handle with the live context
    /// (see `ContextHandles`); only `start_time` and the pause /
    /// maintenance flags are fresh.
    pub(crate) fn app_context(&self) -> Option<AppContext> {
        let handles = self
            .context
            .read()
            .expect("context lock poisoned")
            .clone()?;
        Some(AppContext {
            config: handles.config,
            hooks: handles.hooks,
            modules: handles.modules,
            cache: handles.cache,
            geo: handles.geo,
            db: handles.db,
            storage_registry: handles.storage_registry,
            routes: handles.routes,
            tls: handles.tls,
            start_time: Instant::now(),
            paused: AtomicBool::new(false),
            maintenance: AtomicBool::new(false),
        })
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }
}
