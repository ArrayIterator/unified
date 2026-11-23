// load_balancer -- In-memory stores.

use unified_core::hooks::HookBus;
use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, OnceLock};
use uuid::Uuid;

use super::types::*;

// ---------------------------------------------------------------------------
// In-memory stores.
// ---------------------------------------------------------------------------

pub(crate) static BALANCERS: OnceLock<DashMap<Uuid, LoadBalancer>> = OnceLock::new();

pub(crate) static BACKENDS: OnceLock<DashMap<Uuid, Backend>> = OnceLock::new();

pub(crate) static BACKEND_STATS: OnceLock<DashMap<Uuid, Mutex<BackendStats>>> = OnceLock::new();

pub(crate) static ROUND_ROBIN: OnceLock<DashMap<Uuid, AtomicU64>> = OnceLock::new();

pub(crate) static ACTIVE_CONNECTIONS: OnceLock<DashMap<Uuid, AtomicU64>> = OnceLock::new();

pub(crate) static TLS_CACHE: OnceLock<DashMap<String, CachedTlsStatus>> = OnceLock::new();

pub(crate) static HOOK_BUS: OnceLock<Arc<HookBus>> = OnceLock::new();

pub(crate) static SWEEPER_STARTED: AtomicBool = AtomicBool::new(false);

pub(crate) fn balancers_store() -> &'static DashMap<Uuid, LoadBalancer> {
    BALANCERS.get_or_init(DashMap::new)
}

pub(crate) fn backends_store() -> &'static DashMap<Uuid, Backend> {
    BACKENDS.get_or_init(DashMap::new)
}

pub(crate) fn stats_store() -> &'static DashMap<Uuid, Mutex<BackendStats>> {
    BACKEND_STATS.get_or_init(DashMap::new)
}

pub(crate) fn round_robin_store() -> &'static DashMap<Uuid, AtomicU64> {
    ROUND_ROBIN.get_or_init(DashMap::new)
}

pub(crate) fn connections_store() -> &'static DashMap<Uuid, AtomicU64> {
    ACTIVE_CONNECTIONS.get_or_init(DashMap::new)
}

pub(crate) fn tls_cache() -> &'static DashMap<String, CachedTlsStatus> {
    TLS_CACHE.get_or_init(DashMap::new)
}

pub(crate) fn backend_stats(id: Uuid) -> BackendStats {
    stats_store()
        .get(&id)
        .map(|entry| entry.lock().expect("backend stats lock poisoned").clone())
        .unwrap_or_default()
}

pub(crate) fn mutate_backend_stats(id: Uuid, update: impl FnOnce(&mut BackendStats)) {
    let entry = stats_store()
        .entry(id)
        .or_insert_with(|| Mutex::new(BackendStats::default()));
    let mut stats = entry.lock().expect("backend stats lock poisoned");
    update(&mut stats);
}

pub(crate) fn active_connections(id: Uuid) -> u64 {
    connections_store()
        .get(&id)
        .map(|entry| entry.load(AtomicOrdering::SeqCst))
        .unwrap_or(0)
}

/// Cumulative requests ever routed to a backend
/// (`BackendStats::requests_routed`). This is a lifetime total, not
/// a live load figure: it only ever grows, and least-connections
/// selection must never use it. The live figure is
/// `active_connections`, which only the real proxy path moves
/// (open on upstream selection, close when the proxied session
/// ends) plus the manual `/connections/release` endpoint.
pub(crate) fn routed_requests(id: Uuid) -> u64 {
    backend_stats(id).requests_routed
}

/// Record a newly opened real connection to a backend. Only the
/// real proxy path may call this: it must call
/// `note_backend_connection_closed` when the connection ends, so
/// the counter tracks live load. The simulated `/route` endpoint
/// deliberately does NOT call this — a simulated request has no
/// close event, so booking it here would turn the counter into a
/// cumulative routed total that only ever grows. Simulated routing
/// counts toward `BackendStats::requests_routed` instead (see
/// `routed_requests`).
pub fn note_backend_connection_open(backend_id: Uuid) {
    connections_store()
        .entry(backend_id)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, AtomicOrdering::SeqCst);
}

pub fn note_backend_connection_closed(backend_id: Uuid) {
    if let Some(entry) = connections_store().get(&backend_id) {
        entry
            .fetch_update(AtomicOrdering::SeqCst, AtomicOrdering::SeqCst, |value| {
                Some(value.saturating_sub(1))
            })
            .ok();
    }
}

pub(crate) fn next_round_robin(balancer_id: Uuid) -> u64 {
    round_robin_store()
        .entry(balancer_id)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, AtomicOrdering::SeqCst)
}
