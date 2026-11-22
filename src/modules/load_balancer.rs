// Load Balancer module.
//
// Balancers front a pool of backends and route traffic to them. A
// backend references either a provider-registry server/container
// (`source_kind: "provider"`, `source_id` is the provider connection
// id plus server id), a cluster node (`source_kind: "cluster"`,
// `source_id` is the cluster id plus node id), or a plain address
// (`source_kind: "address"`). The reference is stored as typed
// fields only: this module never imports the provider registry or
// the cluster module. Liveness is resolved through health checks,
// and cluster health is observed through hook events
// (`cluster.node.unhealthy`, `cluster.node.healthy`,
// `cluster.node.removed`) emitted by the cluster module.
//
// Balancing strategies: round-robin, least-connections, weighted,
// IP hash (consistent hashing). Selection is a pure function
// (`select_backend`) so it can be unit-tested without live traffic;
// `POST /{id}/preview` exposes the same selection over HTTP.
//
// Health checks probe each backend (HTTP or TCP) on a background
// sweeper thread. Unhealthy backends leave the rotation and a
// `lb.backend.unhealthy` event is emitted; recovered backends
// re-enter the rotation (`lb.backend.healthy`). When every backend
// of a balancer is unhealthy the balancer enters the degraded state,
// routing answers with a JSON 503 through the standard error
// envelope, and `lb.failover` is emitted.
//
// TLS termination: a balancer with `tls_mode: "terminate"`
// references a domain whose certificate lives in the shared
// `SslStorage` (`crate::http::tls`). This module does not own TLS:
// `tls_certificate_status` performs the lookup against a storage
// handle using the real `SslStorage` API (`lookup`, `cert_expiry`,
// `certificate_names`), and the HTTP handlers validate the domain
// against the configured certificates in `app.ssl.domains`. The
// storage handle is built once in `http::server::run_server`, attached
// to the context with `AppContext::set_tls`, and read back here with
// `AppContext::tls()`; when no handle is attached the TLS endpoint
// falls back to the config-only status.
//
// State is kept in memory (per-process `DashMap` stores). The
// migration SQL and Diesel schema entries for durable persistence
// are documented at the bottom of this file for the integrator;
// until they are applied, balancers, backends, and counters reset
// on restart.
//
// Routes under `/api/v1/load-balancer`:
//
//   Balancers:
//     GET    /                              list (pagination/filter)
//     POST   /                              create
//     GET    /{id}                          get one
//     PUT    /{id}                          update
//     DELETE /{id}                          delete (with its backends)
//     POST   /{id}/enable                   enable
//     POST   /{id}/disable                  disable
//     GET    /{id}/route?client_ip=...      route one request (simulated)
//     POST   /{id}/preview                  preview selection for a client IP
//     GET    /{id}/tls                      TLS termination status
//     POST   /{id}/check-now                probe every backend now
//     GET    /{id}/stats                    balancer + backend stats
//     POST   /{id}/stats/reset              reset balancer + backend stats
//   Backends:
//     GET    /{id}/backends                 list (pagination/filter)
//     POST   /{id}/backends                 create
//     GET    /{id}/backends/{backend_id}     get one
//     PUT    /{id}/backends/{backend_id}     update
//     DELETE /{id}/backends/{backend_id}     delete
//     POST   /{id}/backends/{backend_id}/enable
//     POST   /{id}/backends/{backend_id}/disable
//     POST   /{id}/backends/{backend_id}/drain
//     POST   /{id}/backends/{backend_id}/undrain
//     POST   /{id}/backends/{backend_id}/check-now
//     GET    /{id}/backends/{backend_id}/stats
//     POST   /{id}/backends/{backend_id}/stats/reset
//     POST   /{id}/backends/{backend_id}/connections/release
//
// Hooks emitted: `lb.created`, `lb.updated`, `lb.backend.added`,
// `lb.backend.removed`, `lb.backend.unhealthy`, `lb.backend.healthy`,
// `lb.failover`, `lb.strategy.changed`.
// Hooks observed: `ssl.swap`, `ssl.renew` (TLS status cache),
// `cluster.node.unhealthy`, `cluster.node.healthy`,
// `cluster.node.removed` (cluster backends only).

use crate::core::config::AppConfig;
use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::http::tls::SslStorage;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json as json_value, Value};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Event names.
// ---------------------------------------------------------------------------

pub const LB_CREATED: &str = "lb.created";
pub const LB_UPDATED: &str = "lb.updated";
pub const LB_BACKEND_ADDED: &str = "lb.backend.added";
pub const LB_BACKEND_REMOVED: &str = "lb.backend.removed";
pub const LB_BACKEND_UNHEALTHY: &str = "lb.backend.unhealthy";
pub const LB_BACKEND_HEALTHY: &str = "lb.backend.healthy";
pub const LB_FAILOVER: &str = "lb.failover";
pub const LB_STRATEGY_CHANGED: &str = "lb.strategy.changed";

/// Cluster health events observed by this module. These names are the
/// contract with the cluster module: it emits them when a node changes
/// health or is removed. No cluster event exists in
/// `crate::core::hooks::events` yet, so the names live here until the
/// cluster module (or core) adopts them.
pub const CLUSTER_NODE_UNHEALTHY: &str = "cluster.node.unhealthy";
pub const CLUSTER_NODE_HEALTHY: &str = "cluster.node.healthy";
pub const CLUSTER_NODE_REMOVED: &str = "cluster.node.removed";

// ---------------------------------------------------------------------------
// Defaults.
// ---------------------------------------------------------------------------

const DEFAULT_HEALTH_PATH: &str = "/health";
const DEFAULT_HEALTH_INTERVAL_SECS: u64 = 30;
const DEFAULT_HEALTH_TIMEOUT_SECS: u64 = 5;
const DEFAULT_HEALTHY_THRESHOLD: u32 = 2;
const DEFAULT_UNHEALTHY_THRESHOLD: u32 = 3;
const DEFAULT_WEIGHT: u32 = 1;
const MAX_WEIGHT: u32 = 100;
const MIN_HEALTH_INTERVAL_SECS: u64 = 5;
const MAX_HEALTH_INTERVAL_SECS: u64 = 3600;
const MAX_HEALTH_TIMEOUT_SECS: u64 = 60;
const MAX_THRESHOLD: u32 = 10;
const SWEEP_TICK_SECS: u64 = 1;

fn default_true() -> bool {
    true
}

fn default_weight() -> u32 {
    DEFAULT_WEIGHT
}

// ---------------------------------------------------------------------------
// Enumerations.
// ---------------------------------------------------------------------------

/// TLS handling at the balancer listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    Off,
    Terminate,
    Passthrough,
}

impl Default for TlsMode {
    fn default() -> Self {
        Self::Off
    }
}

/// Backend selection strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    RoundRobin,
    LeastConnections,
    Weighted,
    IpHash,
}

impl Default for Strategy {
    fn default() -> Self {
        Self::RoundRobin
    }
}

/// Effective balancer state. `Degraded` means the balancer is enabled
/// but no backend can currently serve traffic (none configured, or
/// every backend unhealthy, disabled, or draining).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BalancerStatus {
    Active,
    Degraded,
    Disabled,
}

/// Backend health state. `Unknown` means the backend has never been
/// probed; unknown backends are still eligible for routing (optimistic
/// until the first probe proves otherwise).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Unknown,
    Healthy,
    Unhealthy,
}

/// What a backend points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendSourceKind {
    /// A server/container from the provider registry. `source_id`
    /// holds the provider connection id and server id.
    Provider,
    /// A node of a cluster. `source_id` holds the cluster id and
    /// node id.
    Cluster,
    /// A plain `host:port` address with no registry backing.
    Address,
}

/// Health-check transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthCheckKind {
    Http,
    Tcp,
}

impl Default for HealthCheckKind {
    fn default() -> Self {
        Self::Http
    }
}

// ---------------------------------------------------------------------------
// Stored records.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckConfig {
    pub kind: HealthCheckKind,
    pub path: String,
    pub interval_secs: u64,
    pub timeout_secs: u64,
    pub healthy_threshold: u32,
    pub unhealthy_threshold: u32,
    /// Expected HTTP status; `None` means any 2xx status. Ignored
    /// for TCP checks.
    pub expected_status: Option<u16>,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            kind: HealthCheckKind::Http,
            path: DEFAULT_HEALTH_PATH.to_string(),
            interval_secs: DEFAULT_HEALTH_INTERVAL_SECS,
            timeout_secs: DEFAULT_HEALTH_TIMEOUT_SECS,
            healthy_threshold: DEFAULT_HEALTHY_THRESHOLD,
            unhealthy_threshold: DEFAULT_UNHEALTHY_THRESHOLD,
            expected_status: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadBalancer {
    pub id: Uuid,
    pub name: String,
    pub listen_address: String,
    pub listen_port: u16,
    pub tls_mode: TlsMode,
    /// Domain whose certificate terminates TLS (`tls_mode` =
    /// `terminate` only). Must match a certificate in `SslStorage`
    /// / `app.ssl.domains`.
    pub domain: Option<String>,
    /// Optional certificate name override; when empty, `domain` is
    /// used as the lookup name.
    pub certificate_name: Option<String>,
    pub strategy: Strategy,
    pub health_check: HealthCheckConfig,
    pub enabled: bool,
    pub status: BalancerStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backend {
    pub id: Uuid,
    pub balancer_id: Uuid,
    pub name: String,
    pub source_kind: BackendSourceKind,
    /// Provider connection id + server id (`provider`), or cluster
    /// id + node id (`cluster`). Empty for plain addresses.
    /// Format: `<owner_id>/<record_id>` (both parts opaque ids).
    pub source_id: Option<String>,
    /// Dial address (`host:port`) used by health checks and routing.
    pub address: String,
    pub weight: u32,
    pub enabled: bool,
    /// Draining backends finish existing connections but receive no
    /// new traffic.
    pub draining: bool,
    pub health: HealthState,
    pub last_check_at: Option<DateTime<Utc>>,
    pub last_latency_ms: Option<u64>,
    pub consecutive_failures: u32,
    pub consecutive_successes: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Counters for one backend. Kept in a separate map so the `Backend`
/// record stays plain data (cloneable, serializable).
#[derive(Debug, Clone, Default, Serialize)]
pub struct BackendStats {
    pub requests_routed: u64,
    pub errors: u64,
    pub total_latency_ms: u64,
    pub latency_samples: u64,
    pub checks_total: u64,
    pub checks_ok: u64,
}

impl BackendStats {
    pub fn average_latency_ms(&self) -> Option<f64> {
        if self.latency_samples == 0 {
            None
        } else {
            Some(self.total_latency_ms as f64 / self.latency_samples as f64)
        }
    }

    /// Fraction of health checks that succeeded (0.0..=1.0).
    /// `None` when no check has run yet.
    pub fn uptime_ratio(&self) -> Option<f64> {
        if self.checks_total == 0 {
            None
        } else {
            Some(self.checks_ok as f64 / self.checks_total as f64)
        }
    }
}

/// Cached TLS status for one domain, refreshed by `ssl.swap` /
/// `ssl.renew` events and by the TLS status endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct CachedTlsStatus {
    pub domain: String,
    pub present: bool,
    pub expires_at: Option<DateTime<Utc>>,
    pub checked_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// In-memory stores.
// ---------------------------------------------------------------------------

static BALANCERS: OnceLock<DashMap<Uuid, LoadBalancer>> = OnceLock::new();
static BACKENDS: OnceLock<DashMap<Uuid, Backend>> = OnceLock::new();
static BACKEND_STATS: OnceLock<DashMap<Uuid, Mutex<BackendStats>>> = OnceLock::new();
static ROUND_ROBIN: OnceLock<DashMap<Uuid, AtomicU64>> = OnceLock::new();
static ACTIVE_CONNECTIONS: OnceLock<DashMap<Uuid, AtomicU64>> = OnceLock::new();
static TLS_CACHE: OnceLock<DashMap<String, CachedTlsStatus>> = OnceLock::new();
static HOOK_BUS: OnceLock<Arc<HookBus>> = OnceLock::new();
static SWEEPER_STARTED: AtomicBool = AtomicBool::new(false);

fn balancers_store() -> &'static DashMap<Uuid, LoadBalancer> {
    BALANCERS.get_or_init(DashMap::new)
}

fn backends_store() -> &'static DashMap<Uuid, Backend> {
    BACKENDS.get_or_init(DashMap::new)
}

fn stats_store() -> &'static DashMap<Uuid, Mutex<BackendStats>> {
    BACKEND_STATS.get_or_init(DashMap::new)
}

fn round_robin_store() -> &'static DashMap<Uuid, AtomicU64> {
    ROUND_ROBIN.get_or_init(DashMap::new)
}

fn connections_store() -> &'static DashMap<Uuid, AtomicU64> {
    ACTIVE_CONNECTIONS.get_or_init(DashMap::new)
}

fn tls_cache() -> &'static DashMap<String, CachedTlsStatus> {
    TLS_CACHE.get_or_init(DashMap::new)
}

fn backend_stats(id: Uuid) -> BackendStats {
    stats_store()
        .get(&id)
        .map(|entry| entry.lock().expect("backend stats lock poisoned").clone())
        .unwrap_or_default()
}

fn mutate_backend_stats(id: Uuid, update: impl FnOnce(&mut BackendStats)) {
    let entry = stats_store().entry(id).or_insert_with(|| Mutex::new(BackendStats::default()));
    let mut stats = entry.lock().expect("backend stats lock poisoned");
    update(&mut stats);
}

fn active_connections(id: Uuid) -> u64 {
    connections_store()
        .get(&id)
        .map(|entry| entry.load(AtomicOrdering::SeqCst))
        .unwrap_or(0)
}

/// Record a newly routed connection to a backend. A real proxy layer
/// must call `note_backend_connection_closed` when the connection
/// ends; the simulated `/route` endpoint calls the open side only,
/// and operators can release connections through the
/// `/connections/release` endpoint.
pub fn note_backend_connection_open(backend_id: Uuid) {
    connections_store()
        .entry(backend_id)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, AtomicOrdering::SeqCst);
}

pub fn note_backend_connection_closed(backend_id: Uuid) {
    if let Some(entry) = connections_store().get(&backend_id) {
        entry.fetch_update(AtomicOrdering::SeqCst, AtomicOrdering::SeqCst, |value| {
            Some(value.saturating_sub(1))
        }).ok();
    }
}

fn next_round_robin(balancer_id: Uuid) -> u64 {
    round_robin_store()
        .entry(balancer_id)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, AtomicOrdering::SeqCst)
}

// ---------------------------------------------------------------------------
// Validation.
// ---------------------------------------------------------------------------

/// Split a `host:port` (or `[ipv6]:port`) value. Returns
/// `(host, port)` or `None` when the value is malformed.
pub fn parse_host_port(value: &str) -> Option<(String, u16)> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(rest) = value.strip_prefix('[') {
        let (host, port) = rest.split_once("]:")?;
        if host.is_empty() || port.is_empty() {
            return None;
        }
        let port: u16 = port.parse().ok()?;
        if port == 0 {
            return None;
        }
        Some((host.to_string(), port))
    } else {
        let (host, port) = value.rsplit_once(':')?;
        if host.is_empty() || host.contains(':') || port.is_empty() {
            return None;
        }
        let port: u16 = port.parse().ok()?;
        if port == 0 {
            return None;
        }
        Some((host.to_string(), port))
    }
}

fn validate_host_port(value: &str, field: &str) -> UnifiedResult<()> {
    let (host, _port) = parse_host_port(value).ok_or_else(|| {
        UnifiedError::bad_request(format!(
            "{field} must be a valid \"host:port\" address, got \"{value}\""
        ))
    })?;
    if host.chars().any(|c| c.is_whitespace() || c == '/' || c == '?') {
        return Err(UnifiedError::bad_request(format!(
            "{field} host contains an invalid character: \"{value}\""
        )));
    }
    Ok(())
}

fn validate_name(name: &str, field: &str) -> UnifiedResult<String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if name.len() > 200 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most 200 characters"
        )));
    }
    Ok(name)
}

fn validate_domain(domain: &str) -> UnifiedResult<String> {
    let domain = domain.trim().trim_end_matches('.').to_lowercase();
    if domain.is_empty() || domain.len() > 253 {
        return Err(UnifiedError::bad_request(
            "Domain must not be empty and must be at most 253 characters",
        ));
    }
    if !domain
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '*')
        || domain.starts_with('-')
        || domain.starts_with('.')
        || domain.contains("..")
    {
        return Err(UnifiedError::bad_request(format!(
            "Invalid domain name \"{domain}\""
        )));
    }
    Ok(domain)
}

fn validate_health_check(config: &HealthCheckConfig) -> UnifiedResult<()> {
    if config.kind == HealthCheckKind::Http {
        if !config.path.starts_with('/') {
            return Err(UnifiedError::bad_request(
                "Health check path must start with \"/\"",
            ));
        }
        if config.path.len() > 512 {
            return Err(UnifiedError::bad_request(
                "Health check path must be at most 512 characters",
            ));
        }
    }
    if !(MIN_HEALTH_INTERVAL_SECS..=MAX_HEALTH_INTERVAL_SECS).contains(&config.interval_secs) {
        return Err(UnifiedError::bad_request(format!(
            "Health check interval must be between {MIN_HEALTH_INTERVAL_SECS} and {MAX_HEALTH_INTERVAL_SECS} seconds"
        )));
    }
    if config.timeout_secs < 1 || config.timeout_secs > MAX_HEALTH_TIMEOUT_SECS {
        return Err(UnifiedError::bad_request(format!(
            "Health check timeout must be between 1 and {MAX_HEALTH_TIMEOUT_SECS} seconds"
        )));
    }
    if config.timeout_secs >= config.interval_secs {
        return Err(UnifiedError::bad_request(
            "Health check timeout must be smaller than the check interval",
        ));
    }
    if config.healthy_threshold < 1 || config.healthy_threshold > MAX_THRESHOLD {
        return Err(UnifiedError::bad_request(format!(
            "Healthy threshold must be between 1 and {MAX_THRESHOLD}"
        )));
    }
    if config.unhealthy_threshold < 1 || config.unhealthy_threshold > MAX_THRESHOLD {
        return Err(UnifiedError::bad_request(format!(
            "Unhealthy threshold must be between 1 and {MAX_THRESHOLD}"
        )));
    }
    if let Some(status) = config.expected_status {
        if !(100..=599).contains(&status) {
            return Err(UnifiedError::bad_request(
                "Expected HTTP status must be between 100 and 599",
            ));
        }
    }
    Ok(())
}

fn validate_weight(weight: u32) -> UnifiedResult<u32> {
    if !(1..=MAX_WEIGHT).contains(&weight) {
        return Err(UnifiedError::bad_request(format!(
            "Backend weight must be between 1 and {MAX_WEIGHT}"
        )));
    }
    Ok(weight)
}

/// A balancer with TLS termination must reference a domain that has
/// a certificate, unless automatic certificate creation is enabled
/// (then the storage creates a fallback on first handshake).
fn validate_tls_reference(config: &AppConfig, tls_mode: TlsMode, domain: &Option<String>) -> UnifiedResult<()> {
    if tls_mode != TlsMode::Terminate {
        return Ok(());
    }
    let Some(domain) = domain.as_deref() else {
        return Err(UnifiedError::bad_request(
            "TLS mode \"terminate\" requires a domain",
        ));
    };
    if config.app.ssl.domains.contains_key(domain) || config.app.ssl.auto_create {
        return Ok(());
    }
    Err(UnifiedError::unprocessable(format!(
        "No certificate is configured for domain \"{domain}\"; add it to app.ssl.domains or enable app.ssl.auto_create"
    )))
}

// ---------------------------------------------------------------------------
// Backend selection (pure functions, unit-tested below).
// ---------------------------------------------------------------------------

fn hash_u64(data: &[u8]) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    u64::from_be_bytes(digest[0..8].try_into().expect("sha256 digest is 32 bytes"))
}

/// Pick one backend id from the eligible list, following `strategy`.
///
/// - Round-robin: `counter` cycles through the list in order.
/// - Weighted: deterministic weighted round-robin; `counter` walks
///   the cumulative weight ranges.
/// - Least-connections: the backend with the lowest
///   `connections / weight` ratio; ties break on the backend id so
///   the choice is deterministic.
/// - IP hash: consistent hashing over the backend ids; the same
///   client IP always maps to the same backend while the set is
///   unchanged, and only the neighbours of a removed backend move.
///
/// `counter` is the balancer's monotonically increasing selection
/// counter (see `next_round_robin`). `connections` maps backend id
/// to its current active-connection count.
pub fn select_backend(
    backends: &[Backend],
    strategy: Strategy,
    client_ip: Option<&str>,
    counter: u64,
    connections: &HashMap<Uuid, u64>,
) -> Option<Uuid> {
    if backends.is_empty() {
        return None;
    }
    match strategy {
        Strategy::RoundRobin => Some(backends[(counter as usize) % backends.len()].id),
        Strategy::Weighted => {
            let total: u64 = backends.iter().map(|backend| backend.weight as u64).sum();
            if total == 0 {
                return None;
            }
            let mut point = counter % total;
            for backend in backends {
                let weight = backend.weight as u64;
                if point < weight {
                    return Some(backend.id);
                }
                point -= weight;
            }
            Some(backends[backends.len() - 1].id)
        }
        Strategy::LeastConnections => backends
            .iter()
            .min_by(|a, b| {
                let load_a = connections.get(&a.id).copied().unwrap_or(0) as f64
                    / a.weight.max(1) as f64;
                let load_b = connections.get(&b.id).copied().unwrap_or(0) as f64
                    / b.weight.max(1) as f64;
                load_a
                    .partial_cmp(&load_b)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| a.id.cmp(&b.id))
            })
            .map(|backend| backend.id),
        Strategy::IpHash => {
            let ip = client_ip.unwrap_or("0.0.0.0");
            let hash = hash_u64(ip.as_bytes());
            let mut ring: Vec<(u64, Uuid)> = backends
                .iter()
                .map(|backend| (hash_u64(backend.id.as_bytes()), backend.id))
                .collect();
            ring.sort();
            ring
                .iter()
                .find(|(point, _)| *point >= hash)
                .or_else(|| ring.first())
                .map(|(_, id)| *id)
        }
    }
}

/// Backends of one balancer that may receive new traffic: enabled,
/// not draining, and not known-unhealthy. Never-probed backends
/// (`HealthState::Unknown`) are eligible.
pub fn eligible_backends(balancer_id: Uuid) -> Vec<Backend> {
    let mut backends: Vec<Backend> = backends_store()
        .iter()
        .filter(|entry| {
            entry.balancer_id == balancer_id
                && entry.enabled
                && !entry.draining
                && entry.health != HealthState::Unhealthy
        })
        .map(|entry| entry.clone())
        .collect();
    backends.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
    backends
}

fn all_backends(balancer_id: Uuid) -> Vec<Backend> {
    let mut backends: Vec<Backend> = backends_store()
        .iter()
        .filter(|entry| entry.balancer_id == balancer_id)
        .map(|entry| entry.clone())
        .collect();
    backends.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
    backends
}

/// Effective balancer status from its current state and backends.
fn compute_effective_status(balancer: &LoadBalancer) -> BalancerStatus {
    if !balancer.enabled {
        return BalancerStatus::Disabled;
    }
    let backends = all_backends(balancer.id);
    if backends.is_empty() {
        return BalancerStatus::Degraded;
    }
    if eligible_backends(balancer.id).is_empty() {
        return BalancerStatus::Degraded;
    }
    BalancerStatus::Active
}

/// Recompute and store a balancer's status. Emits `lb.failover`
/// when the balancer transitions into the degraded state.
fn refresh_balancer_status(balancer_id: Uuid) {
    let Some(mut balancer) = balancers_store().get_mut(&balancer_id) else {
        return;
    };
    let new_status = compute_effective_status(&balancer);
    if balancer.status == new_status {
        return;
    }
    let was = balancer.status;
    balancer.status = new_status;
    balancer.updated_at = Utc::now();
    let (id, name) = (balancer.id, balancer.name.clone());
    drop(balancer);
    if new_status == BalancerStatus::Degraded && was != BalancerStatus::Degraded {
        emit(
            LB_FAILOVER,
            json_value!({
                "balancer_id": id,
                "balancer_name": name,
                "reason": "all_backends_unhealthy",
                "degraded": true,
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// Hook emission.
// ---------------------------------------------------------------------------

fn emit(event: &str, payload: Value) {
    if let Some(bus) = HOOK_BUS.get() {
        bus.emit(event, payload);
    }
}

fn emit_ctx(ctx: &AppContext, event: &str, payload: Value) {
    ctx.hooks.emit(event, payload);
}

// ---------------------------------------------------------------------------
// TLS termination lookup.
// ---------------------------------------------------------------------------

/// TLS certificate information for one domain.
#[derive(Debug, Clone, Serialize)]
pub struct TlsCertificateInfo {
    pub domain: String,
    /// A certificate is stored for the domain.
    pub present: bool,
    pub expires_at: Option<String>,
    /// DNS / IP names on the stored leaf certificate.
    pub names: Vec<String>,
}

/// Look a domain up in the live TLS storage. This is the lookup path
/// a balancer with `tls_mode: "terminate"` uses: `SslStorage::lookup`
/// resolves exact names first, then wildcard suffixes, with runtime
/// entries winning over configured ones; `cert_expiry` and
/// `certificate_names` read the stored leaf certificate. Callers pass
/// the shared handle from `AppContext::tls()` (attached by
/// `http::server::run_server`); when no handle is attached the
/// handlers fall back to `tls_status_from_config` below.
pub fn tls_certificate_status(storage: &SslStorage, domain: &str) -> TlsCertificateInfo {
    TlsCertificateInfo {
        domain: domain.to_string(),
        present: storage.lookup(domain).is_some(),
        expires_at: storage
            .cert_expiry(domain)
            .map(|expiry| expiry.to_rfc3339()),
        names: storage.certificate_names(domain).unwrap_or_default(),
    }
}

/// TLS status without a storage handle: configured certificates come
/// from `app.ssl.domains`, and expiry / presence details come from
/// the cache refreshed by `ssl.swap` / `ssl.renew` events.
fn tls_status_from_config(domain: &str, config: &AppConfig) -> TlsCertificateInfo {
    let configured = config.app.ssl.domains.contains_key(domain);
    let cached = tls_cache().get(domain).map(|entry| entry.clone());
    TlsCertificateInfo {
        domain: domain.to_string(),
        present: cached.as_ref().map(|entry| entry.present).unwrap_or(configured),
        expires_at: cached
            .and_then(|entry| entry.expires_at)
            .map(|expiry| expiry.to_rfc3339()),
        names: cached
            .map(|entry| vec![entry.domain])
            .unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Health probing.
// ---------------------------------------------------------------------------

fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

pub struct ProbeOutcome {
    pub healthy: bool,
    pub latency_ms: u64,
}

async fn probe_tcp(address: &str, timeout: Duration) -> ProbeOutcome {
    let started = Instant::now();
    let healthy = match tokio::net::lookup_host(address).await {
        Ok(mut addrs) => match addrs.next() {
            Some(socket_addr) => {
                tokio::time::timeout(timeout, tokio::net::TcpStream::connect(socket_addr))
                    .await
                    .is_ok_and(|result| result.is_ok())
            }
            None => false,
        },
        Err(_) => false,
    };
    ProbeOutcome {
        healthy,
        latency_ms: started.elapsed().as_millis() as u64,
    }
}

async fn probe_http(address: &str, check: &HealthCheckConfig) -> ProbeOutcome {
    let started = Instant::now();
    let url = format!("http://{}{}", address, check.path);
    let timeout = Duration::from_secs(check.timeout_secs);
    let result = tokio::time::timeout(timeout, http_client().get(&url).send()).await;
    let healthy = match result {
        Ok(Ok(response)) => match check.expected_status {
            Some(expected) => response.status().as_u16() == expected,
            None => response.status().is_success(),
        },
        _ => false,
    };
    ProbeOutcome {
        healthy,
        latency_ms: started.elapsed().as_millis() as u64,
    }
}

/// Probe one backend with the balancer's health-check configuration.
pub async fn probe_backend(backend: &Backend, check: &HealthCheckConfig) -> ProbeOutcome {
    let timeout = Duration::from_secs(check.timeout_secs);
    match check.kind {
        HealthCheckKind::Tcp => probe_tcp(&backend.address, timeout).await,
        HealthCheckKind::Http => probe_http(&backend.address, check).await,
    }
}

/// Apply one probe result to a backend: consecutive counters, health
/// transitions (with thresholding), stats, and the matching hook
/// events. Called by the background sweeper and by the manual
/// check-now endpoints.
fn apply_probe_result(backend_id: Uuid, outcome: ProbeOutcome) {
    let Some(mut backend) = backends_store().get_mut(&backend_id) else {
        return;
    };
    let balancer_id = backend.balancer_id;
    let balancer = balancers_store().get(&balancer_id).map(|entry| entry.clone());
    let Some(balancer) = balancer else {
        return;
    };
    let check = balancer.health_check.clone();
    let previous_health = backend.health;
    backend.last_check_at = Some(Utc::now());
    backend.last_latency_ms = Some(outcome.latency_ms);
    if outcome.healthy {
        backend.consecutive_successes = backend.consecutive_successes.saturating_add(1);
        backend.consecutive_failures = 0;
        if backend.health == HealthState::Unhealthy
            && backend.consecutive_successes >= check.healthy_threshold
        {
            backend.health = HealthState::Healthy;
        } else if backend.health == HealthState::Unknown {
            backend.health = HealthState::Healthy;
        }
    } else {
        backend.consecutive_failures = backend.consecutive_failures.saturating_add(1);
        backend.consecutive_successes = 0;
        if backend.health != HealthState::Unhealthy
            && backend.consecutive_failures >= check.unhealthy_threshold
        {
            backend.health = HealthState::Unhealthy;
        }
    }
    backend.updated_at = Utc::now();
    let (new_health, name, address) = (backend.health, backend.name.clone(), backend.address.clone());
    drop(backend);
    mutate_backend_stats(backend_id, |stats| {
        stats.checks_total = stats.checks_total.saturating_add(1);
        if outcome.healthy {
            stats.checks_ok = stats.checks_ok.saturating_add(1);
        }
    });
    if new_health != previous_health {
        let payload = json_value!({
            "balancer_id": balancer_id,
            "backend_id": backend_id,
            "backend_name": name,
            "address": address,
            "health": format!("{new_health:?}").to_lowercase(),
            "latency_ms": outcome.latency_ms,
            "consecutive_failures": backends_store().get(&backend_id).map(|entry| entry.consecutive_failures).unwrap_or(0),
            "consecutive_successes": backends_store().get(&backend_id).map(|entry| entry.consecutive_successes).unwrap_or(0),
        });
        if new_health == HealthState::Unhealthy {
            emit(LB_BACKEND_UNHEALTHY, payload);
        } else if new_health == HealthState::Healthy {
            emit(LB_BACKEND_HEALTHY, payload);
        }
    }
    refresh_balancer_status(balancer_id);
}

// ---------------------------------------------------------------------------
// Background health sweeper.
// ---------------------------------------------------------------------------

async fn sweep_once() {
    let now = Utc::now();
    let due: Vec<(Backend, HealthCheckConfig)> = backends_store()
        .iter()
        .filter_map(|entry| {
            let backend = entry.clone();
            let balancer = balancers_store().get(&backend.balancer_id)?;
            if !balancer.enabled || !backend.enabled {
                return None;
            }
            let check = balancer.health_check.clone();
            let due = match backend.last_check_at {
                None => true,
                Some(last) => {
                    now.signed_duration_since(last).num_seconds() >= check.interval_secs as i64
                }
            };
            if due {
                Some((backend, check))
            } else {
                None
            }
        })
        .collect();
    for (backend, check) in due {
        let outcome = probe_backend(&backend, &check).await;
        apply_probe_result(backend.id, outcome);
    }
}

fn start_health_sweeper() {
    if SWEEPER_STARTED.swap(true, AtomicOrdering::SeqCst) {
        return;
    }
    let started = std::thread::Builder::new()
        .name("lb-health-sweeper".to_string())
        .spawn(|| {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    log::error!("Load balancer health sweeper cannot start a runtime: {error}");
                    return;
                }
            };
            loop {
                std::thread::sleep(Duration::from_secs(SWEEP_TICK_SECS));
                runtime.block_on(sweep_once());
            }
        });
    if let Err(error) = started {
        log::error!("Load balancer health sweeper thread failed to start: {error}");
        SWEEPER_STARTED.store(false, AtomicOrdering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Hook listeners (registered through `register_hooks`).
// ---------------------------------------------------------------------------

/// A completed renewal / swap refreshes the cached TLS status for
/// the domain so the TLS endpoint reports the new expiry without a
/// storage handle. Request payloads (no expiry, no completion flag)
/// are ignored.
fn on_ssl_event(context: &mut HookContext) {
    let Some(domain) = context
        .get("domain")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        return;
    };
    let completed = context.get("completed").is_some()
        || context.get("ssl_expires_at").is_some()
        || context.get("renewed").is_some();
    if !completed {
        return;
    }
    let expires_at = context
        .get("ssl_expires_at")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<DateTime<Utc>>().ok());
    tls_cache().insert(
        domain.clone(),
        CachedTlsStatus {
            domain,
            present: true,
            expires_at,
            checked_at: Utc::now(),
        },
    );
}

/// Cluster node health events mark every backend that references
/// that node. `source_id` is `<cluster_id>/<node_id>`; the event
/// payload carries `cluster_id` and `node_id`, so the match is exact
/// and can never mark a backend of another cluster.
fn cluster_node_id(context: &HookContext) -> Option<String> {
    let cluster_id = context.get("cluster_id")?.as_str()?;
    let node_id = context.get("node_id")?.as_str()?;
    Some(format!("{cluster_id}/{node_id}"))
}

fn on_cluster_node_unhealthy(context: &mut HookContext) {
    let Some(source_id) = cluster_node_id(context) else {
        return;
    };
    let mut affected: Vec<Uuid> = Vec::new();
    for mut entry in backends_store().iter_mut() {
        if entry.source_kind == BackendSourceKind::Cluster
            && entry.source_id.as_deref() == Some(source_id.as_str())
            && entry.health != HealthState::Unhealthy
        {
            entry.health = HealthState::Unhealthy;
            entry.consecutive_failures = entry.consecutive_failures.max(1);
            entry.updated_at = Utc::now();
            affected.push(entry.id);
        }
    }
    for backend_id in affected {
        if let Some(backend) = backends_store().get(&backend_id) {
            let payload = json_value!({
                "balancer_id": backend.balancer_id,
                "backend_id": backend.id,
                "backend_name": backend.name,
                "address": backend.address,
                "health": "unhealthy",
                "source": "cluster_event",
            });
            emit(LB_BACKEND_UNHEALTHY, payload);
            refresh_balancer_status(backend.balancer_id);
        }
    }
}

fn on_cluster_node_healthy(context: &mut HookContext) {
    let Some(source_id) = cluster_node_id(context) else {
        return;
    };
    let mut affected: Vec<(Uuid, Uuid)> = Vec::new();
    for mut entry in backends_store().iter_mut() {
        if entry.source_kind == BackendSourceKind::Cluster
            && entry.source_id.as_deref() == Some(source_id.as_str())
            && entry.health == HealthState::Unhealthy
        {
            entry.health = HealthState::Healthy;
            entry.consecutive_failures = 0;
            entry.consecutive_successes = entry.consecutive_successes.max(1);
            entry.updated_at = Utc::now();
            affected.push((entry.balancer_id, entry.id));
        }
    }
    for (balancer_id, backend_id) in affected {
        if let Some(backend) = backends_store().get(&backend_id) {
            emit(
                LB_BACKEND_HEALTHY,
                json_value!({
                    "balancer_id": balancer_id,
                    "backend_id": backend.id,
                    "backend_name": backend.name,
                    "address": backend.address,
                    "health": "healthy",
                    "source": "cluster_event",
                }),
            );
            refresh_balancer_status(balancer_id);
        }
    }
}

fn on_cluster_node_removed(context: &mut HookContext) {
    let Some(source_id) = cluster_node_id(context) else {
        return;
    };
    let removed: Vec<Backend> = backends_store()
        .iter()
        .filter(|entry| {
            entry.source_kind == BackendSourceKind::Cluster
                && entry.source_id.as_deref() == Some(source_id.as_str())
        })
        .map(|entry| entry.clone())
        .collect();
    let mut balancers: HashSet<Uuid> = HashSet::new();
    for backend in removed {
        backends_store().remove(&backend.id);
        stats_store().remove(&backend.id);
        connections_store().remove(&backend.id);
        balancers.insert(backend.balancer_id);
        emit(
            LB_BACKEND_REMOVED,
            json_value!({
                "balancer_id": backend.balancer_id,
                "backend_id": backend.id,
                "backend_name": backend.name,
                "reason": "cluster_node_removed",
            }),
        );
    }
    for balancer_id in balancers {
        refresh_balancer_status(balancer_id);
    }
}

// ---------------------------------------------------------------------------
// Views.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct BalancerView {
    id: Uuid,
    name: String,
    listen_address: String,
    listen_port: u16,
    tls_mode: TlsMode,
    domain: Option<String>,
    certificate_name: Option<String>,
    strategy: Strategy,
    health_check: HealthCheckConfig,
    enabled: bool,
    status: BalancerStatus,
    backend_count: usize,
    healthy_backend_count: usize,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn balancer_view(balancer: &LoadBalancer) -> BalancerView {
    let backends = all_backends(balancer.id);
    BalancerView {
        id: balancer.id,
        name: balancer.name.clone(),
        listen_address: balancer.listen_address.clone(),
        listen_port: balancer.listen_port,
        tls_mode: balancer.tls_mode,
        domain: balancer.domain.clone(),
        certificate_name: balancer.certificate_name.clone(),
        strategy: balancer.strategy,
        health_check: balancer.health_check.clone(),
        enabled: balancer.enabled,
        status: balancer.status,
        backend_count: backends.len(),
        healthy_backend_count: backends
            .iter()
            .filter(|backend| backend.health == HealthState::Healthy)
            .count(),
        created_at: balancer.created_at,
        updated_at: balancer.updated_at,
    }
}

#[derive(Debug, Clone, Serialize)]
struct BackendView {
    id: Uuid,
    balancer_id: Uuid,
    name: String,
    source_kind: BackendSourceKind,
    source_id: Option<String>,
    address: String,
    weight: u32,
    enabled: bool,
    draining: bool,
    health: HealthState,
    last_check_at: Option<DateTime<Utc>>,
    last_latency_ms: Option<u64>,
    consecutive_failures: u32,
    consecutive_successes: u32,
    active_connections: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn backend_view(backend: &Backend) -> BackendView {
    BackendView {
        id: backend.id,
        balancer_id: backend.balancer_id,
        name: backend.name.clone(),
        source_kind: backend.source_kind,
        source_id: backend.source_id.clone(),
        address: backend.address.clone(),
        weight: backend.weight,
        enabled: backend.enabled,
        draining: backend.draining,
        health: backend.health,
        last_check_at: backend.last_check_at,
        last_latency_ms: backend.last_latency_ms,
        consecutive_failures: backend.consecutive_failures,
        consecutive_successes: backend.consecutive_successes,
        active_connections: active_connections(backend.id),
        created_at: backend.created_at,
        updated_at: backend.updated_at,
    }
}

#[derive(Debug, Clone, Serialize)]
struct BackendStatsView {
    backend_id: Uuid,
    backend_name: String,
    address: String,
    health: HealthState,
    requests_routed: u64,
    errors: u64,
    average_latency_ms: Option<f64>,
    uptime_ratio: Option<f64>,
    checks_total: u64,
    checks_ok: u64,
    active_connections: u64,
}

fn backend_stats_view(backend: &Backend) -> BackendStatsView {
    let stats = backend_stats(backend.id);
    BackendStatsView {
        backend_id: backend.id,
        backend_name: backend.name.clone(),
        address: backend.address.clone(),
        health: backend.health,
        requests_routed: stats.requests_routed,
        errors: stats.errors,
        average_latency_ms: stats.average_latency_ms(),
        uptime_ratio: stats.uptime_ratio(),
        checks_total: stats.checks_total,
        checks_ok: stats.checks_ok,
        active_connections: active_connections(backend.id),
    }
}

#[derive(Debug, Clone, Serialize)]
struct BalancerStatsView {
    balancer_id: Uuid,
    balancer_name: String,
    status: BalancerStatus,
    total_backends: usize,
    healthy_backends: usize,
    requests_routed: u64,
    errors: u64,
    average_latency_ms: Option<f64>,
    uptime_ratio: Option<f64>,
    backends: Vec<BackendStatsView>,
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct HealthCheckInput {
    kind: Option<HealthCheckKind>,
    path: Option<String>,
    interval_secs: Option<u64>,
    timeout_secs: Option<u64>,
    healthy_threshold: Option<u32>,
    unhealthy_threshold: Option<u32>,
    expected_status: Option<u16>,
}

impl HealthCheckInput {
    fn merged(&self, base: &HealthCheckConfig) -> HealthCheckConfig {
        HealthCheckConfig {
            kind: self.kind.unwrap_or(base.kind),
            path: self.path.clone().unwrap_or_else(|| base.path.clone()),
            interval_secs: self.interval_secs.unwrap_or(base.interval_secs),
            timeout_secs: self.timeout_secs.unwrap_or(base.timeout_secs),
            healthy_threshold: self.healthy_threshold.unwrap_or(base.healthy_threshold),
            unhealthy_threshold: self
                .unhealthy_threshold
                .unwrap_or(base.unhealthy_threshold),
            expected_status: self.expected_status.or(base.expected_status),
        }
    }
}

#[derive(Debug, Deserialize)]
struct CreateBalancerRequest {
    name: String,
    listen_address: String,
    listen_port: u16,
    #[serde(default)]
    tls_mode: TlsMode,
    domain: Option<String>,
    certificate_name: Option<String>,
    #[serde(default)]
    strategy: Strategy,
    health_check: Option<HealthCheckInput>,
    #[serde(default = "default_true")]
    enabled: bool,
}

#[derive(Debug, Deserialize)]
struct UpdateBalancerRequest {
    name: Option<String>,
    listen_address: Option<String>,
    listen_port: Option<u16>,
    tls_mode: Option<TlsMode>,
    domain: Option<String>,
    certificate_name: Option<String>,
    strategy: Option<Strategy>,
    health_check: Option<HealthCheckInput>,
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CreateBackendRequest {
    name: String,
    source_kind: BackendSourceKind,
    source_id: Option<String>,
    address: String,
    #[serde(default = "default_weight")]
    weight: u32,
    #[serde(default = "default_true")]
    enabled: bool,
}

#[derive(Debug, Deserialize)]
struct UpdateBackendRequest {
    name: Option<String>,
    source_kind: Option<BackendSourceKind>,
    source_id: Option<String>,
    address: Option<String>,
    weight: Option<u32>,
    enabled: Option<bool>,
    draining: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct PreviewRequest {
    client_ip: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RouteQuery {
    client_ip: Option<String>,
}

fn parse_id(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    raw.parse::<Uuid>().map_err(|_| {
        UnifiedError::bad_request(format!("Invalid {what} id \"{raw}\": expected a UUID"))
    })
}

fn get_balancer(id: Uuid) -> UnifiedResult<LoadBalancer> {
    balancers_store()
        .get(&id)
        .map(|entry| entry.clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Load balancer \"{id}\" not found")))
}

fn get_backend(balancer_id: Uuid, backend_id: Uuid) -> UnifiedResult<Backend> {
    backends_store()
        .get(&backend_id)
        .filter(|entry| entry.balancer_id == balancer_id)
        .map(|entry| entry.clone())
        .ok_or_else(|| {
            UnifiedError::not_found(format!(
                "Backend \"{backend_id}\" not found on load balancer \"{balancer_id}\""
            ))
        })
}

fn apply_list_params<T, F>(
    items: Vec<T>,
    query: &PaginationParams,
    sort_allowed: &[&str],
    default_sort: &str,
    mut sort_key: F,
) -> PageResult<T>
where
    T: Serialize,
    F: FnMut(&T, &str) -> String,
{
    let pagination = query.normalized();
    let sort = query.allowed_sort(sort_allowed, default_sort);
    let mut items = items;
    let order_desc = pagination.order == "desc";
    items.sort_by(|a, b| {
        let ordering = sort_key(a, &sort).cmp(&sort_key(b, &sort));
        if order_desc {
            ordering.reverse()
        } else {
            ordering
        }
    });
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items.into_iter().skip(start).take(pagination.per_page as usize).collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, search: &Option<String>) -> bool {
    match search {
        None => true,
        Some(term) => haystack.to_lowercase().contains(&term.to_lowercase()),
    }
}

// ---------------------------------------------------------------------------
// Balancer handlers.
// ---------------------------------------------------------------------------

async fn list_balancers(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let pagination = query.normalized();
        let mut items: Vec<BalancerView> = balancers_store()
            .iter()
            .map(|entry| balancer_view(&entry))
            .filter(|view| {
                matches_search(&view.name, &pagination.search)
                    && pagination
                        .filters
                        .get("status")
                        .is_none_or(|value| format!("{:?}", view.status).to_lowercase() == *value)
                    && pagination.filters.get("strategy").is_none_or(|value| {
                        format!("{:?}", view.strategy).to_lowercase() == *value
                            || serde_json::to_string(&view.strategy)
                                .unwrap_or_default()
                                .trim_matches('"')
                                == value
                    })
                    && pagination.filters.get("tls_mode").is_none_or(|value| {
                        serde_json::to_string(&view.tls_mode)
                            .unwrap_or_default()
                            .trim_matches('"')
                            == value
                    })
                    && pagination.filters.get("enabled").is_none_or(|value| {
                        view.enabled == (value == "true" || value == "1")
                    })
            })
            .collect();
        let page = apply_list_params(
            std::mem::take(&mut items),
            &query,
            &["name", "listen_port", "created_at"],
            "name",
            |view, sort| match sort {
                "listen_port" => format!("{:05}", view.listen_port),
                "created_at" => view.created_at.to_rfc3339(),
                _ => view.name.clone(),
            },
        );
        Ok(page.to_json())
    })();
    json::respond(result, debug)
}

async fn create_balancer(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateBalancerRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BalancerView> = (|| {
        let config = ctx.config.read().expect("config lock poisoned").clone();
        let name = validate_name(&body.name, "Balancer name")?;
        validate_host_port(&body.listen_address, "Listen address")?;
        if body.listen_port == 0 {
            return Err(UnifiedError::bad_request("Listen port must be between 1 and 65535"));
        }
        let domain = match &body.domain {
            Some(domain) => Some(validate_domain(domain)?),
            None => None,
        };
        let health_check = match &body.health_check {
            Some(input) => {
                let merged = input.merged(&HealthCheckConfig::default());
                validate_health_check(&merged)?;
                merged
            }
            None => HealthCheckConfig::default(),
        };
        validate_tls_reference(&config, body.tls_mode, &domain)?;
        if balancers_store().iter().any(|entry| {
            entry.listen_address == body.listen_address && entry.listen_port == body.listen_port
        }) {
            return Err(UnifiedError::conflict(format!(
                "Another load balancer already listens on {}:{}",
                body.listen_address, body.listen_port
            )));
        }
        let now = Utc::now();
        let balancer = LoadBalancer {
            id: Uuid::new_v4(),
            name,
            listen_address: body.listen_address.trim().to_string(),
            listen_port: body.listen_port,
            tls_mode: body.tls_mode,
            domain: domain.clone(),
            certificate_name: body.certificate_name.as_ref().map(|value| value.trim().to_string()),
            strategy: body.strategy,
            health_check,
            enabled: body.enabled,
            status: if body.enabled {
                BalancerStatus::Degraded
            } else {
                BalancerStatus::Disabled
            },
            created_at: now,
            updated_at: now,
        };
        let view = balancer_view(&balancer);
        balancers_store().insert(balancer.id, balancer.clone());
        emit_ctx(
            &ctx,
            LB_CREATED,
            json_value!({
                "balancer_id": balancer.id,
                "balancer_name": balancer.name,
                "listen_address": balancer.listen_address,
                "listen_port": balancer.listen_port,
                "tls_mode": balancer.tls_mode,
                "strategy": balancer.strategy,
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

async fn get_balancer_handler(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BalancerView> = (|| {
        let id = parse_id(&id, "load balancer")?;
        Ok(balancer_view(&get_balancer(id)?))
    })();
    json::respond(result, debug)
}

async fn update_balancer(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
    body: web::Json<UpdateBalancerRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BalancerView> = (|| {
        let id = parse_id(&id, "load balancer")?;
        let config = ctx.config.read().expect("config lock poisoned").clone();
        let mut balancer = get_balancer(id)?;
        let old_strategy = balancer.strategy;
        let old_tls_mode = balancer.tls_mode;
        let old_domain = balancer.domain.clone();
        if let Some(name) = &body.name {
            balancer.name = validate_name(name, "Balancer name")?;
        }
        if let Some(address) = &body.listen_address {
            validate_host_port(address, "Listen address")?;
            balancer.listen_address = address.trim().to_string();
        }
        if let Some(port) = body.listen_port {
            if port == 0 {
                return Err(UnifiedError::bad_request(
                    "Listen port must be between 1 and 65535",
                ));
            }
            balancer.listen_port = port;
        }
        if let Some(tls_mode) = body.tls_mode {
            balancer.tls_mode = tls_mode;
        }
        if let Some(domain) = &body.domain {
            balancer.domain = Some(validate_domain(domain)?);
        }
        if let Some(certificate_name) = &body.certificate_name {
            balancer.certificate_name = Some(certificate_name.trim().to_string());
        }
        if let Some(strategy) = body.strategy {
            balancer.strategy = strategy;
        }
        if let Some(input) = &body.health_check {
            let merged = input.merged(&balancer.health_check);
            validate_health_check(&merged)?;
            balancer.health_check = merged;
        }
        if let Some(enabled) = body.enabled {
            balancer.enabled = enabled;
        }
        validate_tls_reference(&config, balancer.tls_mode, &balancer.domain)?;
        if balancers_store().iter().any(|entry| {
            entry.id != id
                && entry.listen_address == balancer.listen_address
                && entry.listen_port == balancer.listen_port
        }) {
            return Err(UnifiedError::conflict(format!(
                "Another load balancer already listens on {}:{}",
                balancer.listen_address, balancer.listen_port
            )));
        }
        balancer.updated_at = Utc::now();
        balancer.status = compute_effective_status(&balancer);
        let view = balancer_view(&balancer);
        let (strategy_changed, tls_changed) = (
            balancer.strategy != old_strategy,
            balancer.tls_mode != old_tls_mode || balancer.domain != old_domain,
        );
        balancers_store().insert(id, balancer.clone());
        emit_ctx(
            &ctx,
            LB_UPDATED,
            json_value!({
                "balancer_id": balancer.id,
                "balancer_name": balancer.name,
                "strategy_changed": strategy_changed,
                "tls_changed": tls_changed,
            }),
        );
        if strategy_changed {
            emit_ctx(
                &ctx,
                LB_STRATEGY_CHANGED,
                json_value!({
                    "balancer_id": balancer.id,
                    "balancer_name": balancer.name,
                    "old_strategy": old_strategy,
                    "strategy": balancer.strategy,
                }),
            );
        }
        Ok(view)
    })();
    json::respond(result, debug)
}

async fn delete_balancer(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = parse_id(&id, "load balancer")?;
        let balancer = get_balancer(id)?;
        let backend_ids: Vec<Uuid> = backends_store()
            .iter()
            .filter(|entry| entry.balancer_id == id)
            .map(|entry| entry.id)
            .collect();
        for backend_id in backend_ids {
            backends_store().remove(&backend_id);
            stats_store().remove(&backend_id);
            connections_store().remove(&backend_id);
        }
        round_robin_store().remove(&id);
        balancers_store().remove(&id);
        emit_ctx(
            &ctx,
            LB_UPDATED,
            json_value!({
                "balancer_id": id,
                "balancer_name": balancer.name,
                "deleted": true,
            }),
        );
        Ok(json_value!({ "id": id, "deleted": true }))
    })();
    json::respond(result, debug)
}

async fn set_balancer_enabled(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
    enabled: bool,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BalancerView> = (|| {
        let id = parse_id(&id, "load balancer")?;
        let mut balancer = get_balancer(id)?;
        balancer.enabled = enabled;
        balancer.updated_at = Utc::now();
        balancer.status = compute_effective_status(&balancer);
        let view = balancer_view(&balancer);
        balancers_store().insert(id, balancer.clone());
        emit_ctx(
            &ctx,
            LB_UPDATED,
            json_value!({
                "balancer_id": balancer.id,
                "balancer_name": balancer.name,
                "enabled": enabled,
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

async fn enable_balancer(ctx: web::Data<Arc<AppContext>>, id: web::Path<String>) -> HttpResponse {
    set_balancer_enabled(ctx, id, true).await
}

async fn disable_balancer(ctx: web::Data<Arc<AppContext>>, id: web::Path<String>) -> HttpResponse {
    set_balancer_enabled(ctx, id, false).await
}

async fn get_tls_status(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = parse_id(&id, "load balancer")?;
        let balancer = get_balancer(id)?;
        let config = ctx.config.read().expect("config lock poisoned").clone();
        let certificate_name = balancer
            .certificate_name
            .clone()
            .or_else(|| balancer.domain.clone());
        // Resolve through the one shared storage the HTTP server
        // attached to the context (the same storage the HTTPS listener
        // and the gateway serve from), so renewals and swaps are
        // visible here without a restart. When no handle is attached
        // yet (no TLS listener, or a non-server context such as tests)
        // fall back to the config-only status: the endpoint never
        // fails or panics over a missing handle.
        let storage = ctx.tls();
        let storage_attached = storage.is_some();
        let (terminated, info) = match (balancer.tls_mode, certificate_name) {
            (TlsMode::Terminate, Some(name)) => (
                true,
                Some(match storage.as_ref() {
                    Some(storage) => tls_certificate_status(storage, &name),
                    None => tls_status_from_config(&name, &config),
                }),
            ),
            _ => (false, None),
        };
        Ok(json_value!({
            "balancer_id": balancer.id,
            "tls_mode": balancer.tls_mode,
            "domain": balancer.domain,
            "certificate_name": balancer.certificate_name,
            "terminated": terminated,
            "certificate": info,
            "storage_attached": storage_attached,
            "note": if storage_attached {
                "Live SslStorage lookup through the shared storage attached by http::server::run_server."
            } else {
                "No shared SslStorage handle is attached to the context (no TLS listener, or a non-server context); this endpoint reports configured certificates (app.ssl.domains) plus the cache refreshed by ssl.swap / ssl.renew events."
            },
        }))
    })();
    json::respond(result, debug)
}

async fn preview_selection(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
    body: web::Json<PreviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = parse_id(&id, "load balancer")?;
        let balancer = get_balancer(id)?;
        let client_ip = body.client_ip.as_deref().map(str::trim).filter(|value| !value.is_empty());
        if let Some(ip) = client_ip {
            ip.parse::<std::net::IpAddr>().map_err(|_| {
                UnifiedError::bad_request(format!("Invalid client IP address \"{ip}\""))
            })?;
        }
        let eligible = eligible_backends(id);
        let connections: HashMap<Uuid, u64> = eligible
            .iter()
            .map(|backend| (backend.id, active_connections(backend.id)))
            .collect();
        let counter = round_robin_store()
            .get(&id)
            .map(|entry| entry.load(AtomicOrdering::SeqCst))
            .unwrap_or(0);
        let selected = select_backend(&eligible, balancer.strategy, client_ip, counter, &connections)
            .and_then(|backend_id| eligible.iter().find(|backend| backend.id == backend_id));
        Ok(json_value!({
            "balancer_id": balancer.id,
            "strategy": balancer.strategy,
            "status": balancer.status,
            "degraded": balancer.status == BalancerStatus::Degraded,
            "client_ip": client_ip,
            "eligible_backends": eligible.len(),
            "backend": selected.map(backend_view),
        }))
    })();
    json::respond(result, debug)
}

async fn route_request(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
    query: web::Query<RouteQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = parse_id(&id, "load balancer")?;
        let balancer = get_balancer(id)?;
        let client_ip = query.client_ip.as_deref().map(str::trim).filter(|value| !value.is_empty());
        if let Some(ip) = client_ip {
            ip.parse::<std::net::IpAddr>().map_err(|_| {
                UnifiedError::bad_request(format!("Invalid client IP address \"{ip}\""))
            })?;
        }
        if !balancer.enabled {
            return Err(UnifiedError::new(
                format!("Load balancer \"{}\" is disabled", balancer.name),
                503,
            )
            .with_metadata("balancer_id", json_value!(balancer.id))
            .with_metadata("reason", json_value!("balancer_disabled")));
        }
        let started = Instant::now();
        let eligible = eligible_backends(id);
        if eligible.is_empty() {
            refresh_balancer_status(id);
            emit_ctx(
                &ctx,
                LB_FAILOVER,
                json_value!({
                    "balancer_id": balancer.id,
                    "balancer_name": balancer.name,
                    "reason": "all_backends_unhealthy",
                    "degraded": true,
                }),
            );
            return Err(UnifiedError::new(
                format!(
                    "Load balancer \"{}\" has no healthy backend available",
                    balancer.name
                ),
                503,
            )
            .with_metadata("balancer_id", json_value!(balancer.id))
            .with_metadata("reason", json_value!("all_backends_unhealthy")));
        }
        let connections: HashMap<Uuid, u64> = eligible
            .iter()
            .map(|backend| (backend.id, active_connections(backend.id)))
            .collect();
        let counter = next_round_robin(id);
        let backend_id = select_backend(&eligible, balancer.strategy, client_ip, counter, &connections)
            .ok_or_else(|| UnifiedError::internal("Backend selection returned no backend"))?;
        let backend = eligible
            .into_iter()
            .find(|candidate| candidate.id == backend_id)
            .expect("selected backend is in the eligible list");
        let latency_ms = started.elapsed().as_millis() as u64;
        note_backend_connection_open(backend.id);
        mutate_backend_stats(backend.id, |stats| {
            stats.requests_routed = stats.requests_routed.saturating_add(1);
            stats.total_latency_ms = stats.total_latency_ms.saturating_add(latency_ms);
            stats.latency_samples = stats.latency_samples.saturating_add(1);
        });
        Ok(json_value!({
            "balancer_id": balancer.id,
            "balancer_name": balancer.name,
            "strategy": balancer.strategy,
            "client_ip": client_ip,
            "backend": backend_view(&backend),
            "selection_latency_ms": latency_ms,
        }))
    })();
    json::respond(result, debug)
}

async fn check_now_balancer(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = parse_id(&id, "load balancer");
    let result: UnifiedResult<Value> = (|| async {
        let id = id?;
        let balancer = get_balancer(id)?;
        let backends = all_backends(id);
        let check = balancer.health_check.clone();
        let probes = futures_util::future::join_all(
            backends.iter().map(|backend| probe_backend(backend, &check)),
        )
        .await;
        let mut results = Vec::new();
        for (backend, outcome) in backends.iter().zip(probes.into_iter()) {
            let healthy = outcome.healthy;
            let latency_ms = outcome.latency_ms;
            apply_probe_result(backend.id, outcome);
            results.push(json_value!({
                "backend_id": backend.id,
                "backend_name": backend.name,
                "address": backend.address,
                "healthy": healthy,
                "latency_ms": latency_ms,
                "health": backends_store().get(&backend.id).map(|entry| entry.health),
            }));
        }
        refresh_balancer_status(id);
        Ok(json_value!({
            "balancer_id": id,
            "checked": results.len(),
            "results": results,
        }))
    })()
    .await;
    json::respond(result, debug)
}

async fn balancer_stats(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BalancerStatsView> = (|| {
        let id = parse_id(&id, "load balancer")?;
        let balancer = get_balancer(id)?;
        let backends = all_backends(id);
        let views: Vec<BackendStatsView> =
            backends.iter().map(backend_stats_view).collect();
        let requests_routed: u64 = views.iter().map(|view| view.requests_routed).sum();
        let errors: u64 = views.iter().map(|view| view.errors).sum();
        let (latency_total, latency_samples): (u64, u64) = backends.iter().fold(
            (0, 0),
            |(total, samples), backend| {
                let stats = backend_stats(backend.id);
                (
                    total.saturating_add(stats.total_latency_ms),
                    samples.saturating_add(stats.latency_samples),
                )
            },
        );
        let (checks_total, checks_ok): (u64, u64) = backends.iter().fold(
            (0, 0),
            |(total, ok), backend| {
                let stats = backend_stats(backend.id);
                (
                    total.saturating_add(stats.checks_total),
                    ok.saturating_add(stats.checks_ok),
                )
            },
        );
        Ok(BalancerStatsView {
            balancer_id: balancer.id,
            balancer_name: balancer.name.clone(),
            status: balancer.status,
            total_backends: backends.len(),
            healthy_backends: backends
                .iter()
                .filter(|backend| backend.health == HealthState::Healthy)
                .count(),
            requests_routed,
            errors,
            average_latency_ms: if latency_samples == 0 {
                None
            } else {
                Some(latency_total as f64 / latency_samples as f64)
            },
            uptime_ratio: if checks_total == 0 {
                None
            } else {
                Some(checks_ok as f64 / checks_total as f64)
            },
            backends: views,
        })
    })();
    json::respond(result, debug)
}

async fn reset_balancer_stats(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = parse_id(&id, "load balancer")?;
        let balancer = get_balancer(id)?;
        for backend in all_backends(id) {
            stats_store().remove(&backend.id);
        }
        Ok(json_value!({ "balancer_id": balancer.id, "reset": true }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Backend handlers.
// ---------------------------------------------------------------------------

async fn list_backends(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = parse_id(&id, "load balancer")?;
        let _balancer = get_balancer(id)?;
        let pagination = query.normalized();
        let mut items: Vec<BackendView> = all_backends(id)
            .iter()
            .map(backend_view)
            .filter(|view| {
                matches_search(&view.name, &pagination.search)
                    && pagination.filters.get("health").is_none_or(|value| {
                        serde_json::to_string(&view.health)
                            .unwrap_or_default()
                            .trim_matches('"')
                            == value
                    })
                    && pagination.filters.get("source_kind").is_none_or(|value| {
                        serde_json::to_string(&view.source_kind)
                            .unwrap_or_default()
                            .trim_matches('"')
                            == value
                    })
                    && pagination.filters.get("enabled").is_none_or(|value| {
                        view.enabled == (value == "true" || value == "1")
                    })
                    && pagination.filters.get("draining").is_none_or(|value| {
                        view.draining == (value == "true" || value == "1")
                    })
            })
            .collect();
        let page = apply_list_params(
            std::mem::take(&mut items),
            &query,
            &["name", "address", "created_at"],
            "name",
            |view, sort| match sort {
                "address" => view.address.clone(),
                "created_at" => view.created_at.to_rfc3339(),
                _ => view.name.clone(),
            },
        );
        Ok(page.to_json())
    })();
    json::respond(result, debug)
}

async fn create_backend(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
    body: web::Json<CreateBackendRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackendView> = (|| {
        let id = parse_id(&id, "load balancer")?;
        let balancer = get_balancer(id)?;
        let name = validate_name(&body.name, "Backend name")?;
        validate_host_port(&body.address, "Backend address")?;
        let weight = validate_weight(body.weight)?;
        let source_id = match body.source_kind {
            BackendSourceKind::Address => {
                if body.source_id.as_deref().is_some_and(|value| !value.trim().is_empty()) {
                    return Err(UnifiedError::bad_request(
                        "source_id must be empty for plain address backends",
                    ));
                }
                None
            }
            BackendSourceKind::Provider | BackendSourceKind::Cluster => {
                let raw = body.source_id.as_deref().unwrap_or("").trim();
                if raw.is_empty() {
                    return Err(UnifiedError::bad_request(
                        "source_id is required for provider and cluster backends (format \"<owner_id>/<record_id>\")",
                    ));
                }
                if raw.split('/').count() != 2
                    || raw.split('/').any(|part| part.trim().is_empty())
                {
                    return Err(UnifiedError::bad_request(
                        "source_id must have the format \"<owner_id>/<record_id>\"",
                    ));
                }
                Some(raw.to_string())
            }
        };
        let now = Utc::now();
        let backend = Backend {
            id: Uuid::new_v4(),
            balancer_id: id,
            name,
            source_kind: body.source_kind,
            source_id,
            address: body.address.trim().to_string(),
            weight,
            enabled: body.enabled,
            draining: false,
            health: HealthState::Unknown,
            last_check_at: None,
            last_latency_ms: None,
            consecutive_failures: 0,
            consecutive_successes: 0,
            created_at: now,
            updated_at: now,
        };
        let view = backend_view(&backend);
        backends_store().insert(backend.id, backend.clone());
        refresh_balancer_status(id);
        emit_ctx(
            &ctx,
            LB_BACKEND_ADDED,
            json_value!({
                "balancer_id": id,
                "balancer_name": balancer.name,
                "backend_id": backend.id,
                "backend_name": backend.name,
                "source_kind": backend.source_kind,
                "source_id": backend.source_id,
                "address": backend.address,
                "weight": backend.weight,
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

async fn get_backend_handler(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackendView> = (|| {
        let (balancer_id, backend_id) = path.into_inner();
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        Ok(backend_view(&get_backend(balancer_id, backend_id)?))
    })();
    json::respond(result, debug)
}

async fn update_backend(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateBackendRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackendView> = (|| {
        let (balancer_id, backend_id) = path.into_inner();
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        let mut backend = get_backend(balancer_id, backend_id)?;
        if let Some(name) = &body.name {
            backend.name = validate_name(name, "Backend name")?;
        }
        if let Some(address) = &body.address {
            validate_host_port(address, "Backend address")?;
            backend.address = address.trim().to_string();
        }
        if let Some(weight) = body.weight {
            backend.weight = validate_weight(weight)?;
        }
        if let Some(enabled) = body.enabled {
            backend.enabled = enabled;
        }
        if let Some(draining) = body.draining {
            backend.draining = draining;
        }
        if let Some(source_kind) = body.source_kind {
            backend.source_kind = source_kind;
        }
        if let Some(source_id) = &body.source_id {
            backend.source_id = if source_id.trim().is_empty() {
                None
            } else {
                Some(source_id.trim().to_string())
            };
        }
        if backend.source_kind == BackendSourceKind::Address
            && backend.source_id.is_some()
        {
            return Err(UnifiedError::bad_request(
                "source_id must be empty for plain address backends",
            ));
        }
        if backend.source_kind != BackendSourceKind::Address
            && backend.source_id.as_deref().unwrap_or("").is_empty()
        {
            return Err(UnifiedError::bad_request(
                "source_id is required for provider and cluster backends",
            ));
        }
        backend.updated_at = Utc::now();
        let view = backend_view(&backend);
        backends_store().insert(backend_id, backend.clone());
        refresh_balancer_status(balancer_id);
        emit_ctx(
            &ctx,
            LB_UPDATED,
            json_value!({
                "balancer_id": balancer_id,
                "backend_id": backend.id,
                "backend_name": backend.name,
                "draining": backend.draining,
                "enabled": backend.enabled,
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

async fn delete_backend(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let (balancer_id, backend_id) = path.into_inner();
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        let backend = get_backend(balancer_id, backend_id)?;
        backends_store().remove(&backend_id);
        stats_store().remove(&backend_id);
        connections_store().remove(&backend_id);
        refresh_balancer_status(balancer_id);
        emit_ctx(
            &ctx,
            LB_BACKEND_REMOVED,
            json_value!({
                "balancer_id": balancer_id,
                "backend_id": backend.id,
                "backend_name": backend.name,
            }),
        );
        Ok(json_value!({ "id": backend_id, "deleted": true }))
    })();
    json::respond(result, debug)
}

async fn set_backend_enabled(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
    enabled: bool,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackendView> = (|| {
        let (balancer_id, backend_id) = path.into_inner();
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        let mut backend = get_backend(balancer_id, backend_id)?;
        backend.enabled = enabled;
        backend.updated_at = Utc::now();
        let view = backend_view(&backend);
        backends_store().insert(backend_id, backend.clone());
        refresh_balancer_status(balancer_id);
        Ok(view)
    })();
    json::respond(result, debug)
}

async fn enable_backend(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    set_backend_enabled(ctx, path, true).await
}

async fn disable_backend(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    set_backend_enabled(ctx, path, false).await
}

async fn set_backend_draining(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
    draining: bool,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackendView> = (|| {
        let (balancer_id, backend_id) = path.into_inner();
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        let mut backend = get_backend(balancer_id, backend_id)?;
        backend.draining = draining;
        backend.updated_at = Utc::now();
        let view = backend_view(&backend);
        backends_store().insert(backend_id, backend.clone());
        refresh_balancer_status(balancer_id);
        emit_ctx(
            &ctx,
            LB_UPDATED,
            json_value!({
                "balancer_id": balancer_id,
                "backend_id": backend.id,
                "backend_name": backend.name,
                "draining": draining,
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

async fn drain_backend(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    set_backend_draining(ctx, path, true).await
}

async fn undrain_backend(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    set_backend_draining(ctx, path, false).await
}

async fn check_now_backend(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (balancer_id, backend_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| async {
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        let balancer = get_balancer(balancer_id)?;
        let backend = get_backend(balancer_id, backend_id)?;
        let outcome = probe_backend(&backend, &balancer.health_check).await;
        let healthy = outcome.healthy;
        let latency_ms = outcome.latency_ms;
        apply_probe_result(backend_id, outcome);
        let updated = get_backend(balancer_id, backend_id)?;
        Ok(json_value!({
            "balancer_id": balancer_id,
            "backend_id": backend_id,
            "healthy": healthy,
            "latency_ms": latency_ms,
            "health": updated.health,
            "last_check_at": updated.last_check_at,
        }))
    })()
    .await;
    json::respond(result, debug)
}

async fn backend_stats_handler(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackendStatsView> = (|| {
        let (balancer_id, backend_id) = path.into_inner();
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        Ok(backend_stats_view(&get_backend(balancer_id, backend_id)?))
    })();
    json::respond(result, debug)
}

async fn reset_backend_stats(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let (balancer_id, backend_id) = path.into_inner();
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        let backend = get_backend(balancer_id, backend_id)?;
        stats_store().remove(&backend_id);
        Ok(json_value!({ "backend_id": backend.id, "reset": true }))
    })();
    json::respond(result, debug)
}

async fn release_connection(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let (balancer_id, backend_id) = path.into_inner();
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        let backend = get_backend(balancer_id, backend_id)?;
        note_backend_connection_closed(backend_id);
        Ok(json_value!({
            "backend_id": backend.id,
            "active_connections": active_connections(backend_id),
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct LoadBalancerModule;

impl LoadBalancerModule {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LoadBalancerModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for LoadBalancerModule {
    fn id(&self) -> &'static str {
        "load_balancer"
    }

    fn name(&self) -> &'static str {
        "Load Balancer"
    }

    fn description(&self) -> &'static str {
        "Load balancers over provider, cluster, or plain-address backends: round-robin, least-connections, weighted, and IP-hash strategies, HTTP/TCP health checks with automatic failover, TLS termination through the shared certificate storage, and per-backend routing statistics."
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["reqwest", "dashmap", "sha2"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let _ = HOOK_BUS.set(ctx.hooks.clone());
        start_health_sweeper();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        for event in [events::SSL_RENEW, events::SSL_SWAP] {
            bus.register(event, "load_balancer", Arc::new(on_ssl_event));
        }
        bus.register(
            CLUSTER_NODE_UNHEALTHY,
            "load_balancer",
            Arc::new(on_cluster_node_unhealthy),
        );
        bus.register(
            CLUSTER_NODE_HEALTHY,
            "load_balancer",
            Arc::new(on_cluster_node_healthy),
        );
        bus.register(
            CLUSTER_NODE_REMOVED,
            "load_balancer",
            Arc::new(on_cluster_node_removed),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/load-balancer")
                .route("", web::get().to(list_balancers))
                .route("", web::post().to(create_balancer))
                .route("/{id}", web::get().to(get_balancer_handler))
                .route("/{id}", web::put().to(update_balancer))
                .route("/{id}", web::delete().to(delete_balancer))
                .route("/{id}/enable", web::post().to(enable_balancer))
                .route("/{id}/disable", web::post().to(disable_balancer))
                .route("/{id}/route", web::get().to(route_request))
                .route("/{id}/preview", web::post().to(preview_selection))
                .route("/{id}/tls", web::get().to(get_tls_status))
                .route("/{id}/check-now", web::post().to(check_now_balancer))
                .route("/{id}/stats", web::get().to(balancer_stats))
                .route("/{id}/stats/reset", web::post().to(reset_balancer_stats))
                .route("/{id}/backends", web::get().to(list_backends))
                .route("/{id}/backends", web::post().to(create_backend))
                .route("/{id}/backends/{backend_id}", web::get().to(get_backend_handler))
                .route("/{id}/backends/{backend_id}", web::put().to(update_backend))
                .route("/{id}/backends/{backend_id}", web::delete().to(delete_backend))
                .route("/{id}/backends/{backend_id}/enable", web::post().to(enable_backend))
                .route("/{id}/backends/{backend_id}/disable", web::post().to(disable_backend))
                .route("/{id}/backends/{backend_id}/drain", web::post().to(drain_backend))
                .route("/{id}/backends/{backend_id}/undrain", web::post().to(undrain_backend))
                .route("/{id}/backends/{backend_id}/check-now", web::post().to(check_now_backend))
                .route("/{id}/backends/{backend_id}/stats", web::get().to(backend_stats_handler))
                .route("/{id}/backends/{backend_id}/stats/reset", web::post().to(reset_backend_stats))
                .route(
                    "/{id}/backends/{backend_id}/connections/release",
                    web::post().to(release_connection),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        let routes = [
            ("GET", "/api/v1/load-balancer", "List load balancers (pagination/filter)"),
            ("POST", "/api/v1/load-balancer", "Create a load balancer"),
            ("GET", "/api/v1/load-balancer/{id}", "Get one load balancer"),
            ("PUT", "/api/v1/load-balancer/{id}", "Update a load balancer"),
            ("DELETE", "/api/v1/load-balancer/{id}", "Delete a load balancer and its backends"),
            ("POST", "/api/v1/load-balancer/{id}/enable", "Enable a load balancer"),
            ("POST", "/api/v1/load-balancer/{id}/disable", "Disable a load balancer"),
            ("GET", "/api/v1/load-balancer/{id}/route", "Route one request (simulated)"),
            ("POST", "/api/v1/load-balancer/{id}/preview", "Preview backend selection for a client IP"),
            ("GET", "/api/v1/load-balancer/{id}/tls", "TLS termination status"),
            ("POST", "/api/v1/load-balancer/{id}/check-now", "Probe every backend now"),
            ("GET", "/api/v1/load-balancer/{id}/stats", "Balancer and backend statistics"),
            ("POST", "/api/v1/load-balancer/{id}/stats/reset", "Reset balancer and backend statistics"),
            ("GET", "/api/v1/load-balancer/{id}/backends", "List backends (pagination/filter)"),
            ("POST", "/api/v1/load-balancer/{id}/backends", "Add a backend"),
            ("GET", "/api/v1/load-balancer/{id}/backends/{backend_id}", "Get one backend"),
            ("PUT", "/api/v1/load-balancer/{id}/backends/{backend_id}", "Update a backend"),
            ("DELETE", "/api/v1/load-balancer/{id}/backends/{backend_id}", "Delete a backend"),
            ("POST", "/api/v1/load-balancer/{id}/backends/{backend_id}/enable", "Enable a backend"),
            ("POST", "/api/v1/load-balancer/{id}/backends/{backend_id}/disable", "Disable a backend"),
            ("POST", "/api/v1/load-balancer/{id}/backends/{backend_id}/drain", "Drain a backend (no new traffic)"),
            ("POST", "/api/v1/load-balancer/{id}/backends/{backend_id}/undrain", "Stop draining a backend"),
            ("POST", "/api/v1/load-balancer/{id}/backends/{backend_id}/check-now", "Probe one backend now"),
            ("GET", "/api/v1/load-balancer/{id}/backends/{backend_id}/stats", "Backend statistics"),
            ("POST", "/api/v1/load-balancer/{id}/backends/{backend_id}/stats/reset", "Reset backend statistics"),
            ("POST", "/api/v1/load-balancer/{id}/backends/{backend_id}/connections/release", "Release one active connection"),
        ];
        routes
            .iter()
            .map(|(method, path, description)| RouteInfo {
                method,
                path,
                owner: "load_balancer",
                description,
            })
            .collect()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(LoadBalancerModule::new()));
}

// ---------------------------------------------------------------------------
// Unit tests (pure logic only; no network, no database).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_backend(id: u128, weight: u32) -> Backend {
        let now = Utc::now();
        Backend {
            id: Uuid::from_u128(id),
            balancer_id: Uuid::from_u128(99),
            name: format!("backend-{id}"),
            source_kind: BackendSourceKind::Address,
            source_id: None,
            address: "127.0.0.1:8080".to_string(),
            weight,
            enabled: true,
            draining: false,
            health: HealthState::Healthy,
            last_check_at: None,
            last_latency_ms: None,
            consecutive_failures: 0,
            consecutive_successes: 0,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn round_robin_cycles_in_order() {
        let backends = vec![test_backend(1, 1), test_backend(2, 1), test_backend(3, 1)];
        let connections = HashMap::new();
        let picked: Vec<Uuid> = (0..6)
            .map(|counter| {
                select_backend(&backends, Strategy::RoundRobin, None, counter, &connections)
                    .expect("a backend is picked")
            })
            .collect();
        assert_eq!(
            picked,
            vec![
                backends[0].id,
                backends[1].id,
                backends[2].id,
                backends[0].id,
                backends[1].id,
                backends[2].id,
            ]
        );
    }

    #[test]
    fn weighted_selection_follows_weights() {
        let backends = vec![test_backend(1, 3), test_backend(2, 1)];
        let connections = HashMap::new();
        let picked: Vec<Uuid> = (0..8)
            .map(|counter| {
                select_backend(&backends, Strategy::Weighted, None, counter, &connections)
                    .expect("a backend is picked")
            })
            .collect();
        assert_eq!(
            picked,
            vec![
                backends[0].id,
                backends[0].id,
                backends[0].id,
                backends[1].id,
                backends[0].id,
                backends[0].id,
                backends[0].id,
                backends[1].id,
            ]
        );
    }

    #[test]
    fn least_connections_picks_lightest_backend() {
        let backends = vec![test_backend(1, 1), test_backend(2, 1), test_backend(3, 1)];
        let mut connections = HashMap::new();
        connections.insert(backends[0].id, 5);
        connections.insert(backends[1].id, 1);
        connections.insert(backends[2].id, 3);
        let picked = select_backend(
            &backends,
            Strategy::LeastConnections,
            None,
            0,
            &connections,
        );
        assert_eq!(picked, Some(backends[1].id));
    }

    #[test]
    fn least_connections_accounts_for_weight() {
        let backends = vec![test_backend(1, 1), test_backend(2, 4)];
        let mut connections = HashMap::new();
        connections.insert(backends[0].id, 1);
        connections.insert(backends[1].id, 2);
        // Load ratios: 1/1 = 1.0 vs 2/4 = 0.5, so the heavier backend wins.
        let picked = select_backend(
            &backends,
            Strategy::LeastConnections,
            None,
            0,
            &connections,
        );
        assert_eq!(picked, Some(backends[1].id));
    }

    #[test]
    fn ip_hash_is_stable_and_spreads() {
        let backends = vec![test_backend(1, 1), test_backend(2, 1), test_backend(3, 1)];
        let connections = HashMap::new();
        let first = select_backend(
            &backends,
            Strategy::IpHash,
            Some("203.0.113.7"),
            0,
            &connections,
        );
        let second = select_backend(
            &backends,
            Strategy::IpHash,
            Some("203.0.113.7"),
            42,
            &connections,
        );
        assert_eq!(first, second);
        let mut seen = HashSet::new();
        for octet in 1..=50 {
            let picked = select_backend(
                &backends,
                Strategy::IpHash,
                Some(&format!("203.0.113.{octet}")),
                0,
                &connections,
            );
            seen.insert(picked);
        }
        assert!(seen.len() > 1, "IP hash should spread clients across backends");
    }

    #[test]
    fn empty_backend_list_selects_nothing() {
        let connections = HashMap::new();
        assert_eq!(
            select_backend(&[], Strategy::RoundRobin, None, 0, &connections),
            None
        );
    }

    #[test]
    fn host_port_parsing() {
        assert_eq!(
            parse_host_port("example.com:8080"),
            Some(("example.com".to_string(), 8080))
        );
        assert_eq!(
            parse_host_port("[::1]:8443"),
            Some(("::1".to_string(), 8443))
        );
        assert_eq!(parse_host_port("example.com:0"), None);
        assert_eq!(parse_host_port("example.com"), None);
        assert_eq!(parse_host_port(":8080"), None);
        assert_eq!(parse_host_port("example.com:notaport"), None);
    }

    #[test]
    fn health_check_validation_rejects_bad_timeout() {
        let mut config = HealthCheckConfig::default();
        config.timeout_secs = 30;
        config.interval_secs = 10;
        assert!(validate_health_check(&config).is_err());
        let mut config = HealthCheckConfig::default();
        config.path = "health".to_string();
        assert!(validate_health_check(&config).is_err());
    }

    #[test]
    fn stats_average_and_uptime() {
        let stats = BackendStats {
            requests_routed: 4,
            errors: 0,
            total_latency_ms: 400,
            latency_samples: 4,
            checks_total: 10,
            checks_ok: 9,
            ..BackendStats::default()
        };
        assert_eq!(stats.average_latency_ms(), Some(100.0));
        assert_eq!(stats.uptime_ratio(), Some(0.9));
        assert_eq!(BackendStats::default().uptime_ratio(), None);
    }
}

// ---------------------------------------------------------------------------
// INTEGRATOR NOTES — snippets for shared files (do not apply here).
//
// 1. `src/modules/mod.rs` — declare and register the module:
//
//        pub mod load_balancer;
//        // ... in register_all(), Operations section, after
//        // network::register(registry):
//        load_balancer::register(registry);
//
// 2. `src/core/config.rs` — optional defaults section (all fields
//    have defaults; balancers also carry per-balancer overrides):
//
//        #[derive(Debug, Clone, Serialize, Deserialize)]
//        #[serde(default)]
//        pub struct LoadBalancerSection {
//            /// Default health-check interval in seconds (5..=3600).
//            pub health_check_interval_secs: u64,
//            /// Default health-check timeout in seconds (1..=60).
//            pub health_check_timeout_secs: u64,
//            /// Default health-check path for HTTP checks.
//            pub health_check_path: String,
//            /// Default strategy: round_robin | least_connections |
//            /// weighted | ip_hash.
//            pub default_strategy: String,
//            /// Maximum backends per balancer (1..=1000).
//            pub max_backends_per_balancer: usize,
//        }
//        // Add `pub load_balancer: LoadBalancerSection` to `AppConfig`
//        // (with `load_balancer: LoadBalancerSection::default()` in
//        // `AppConfig::default()`), plus a `Default` impl with
//        // interval 30, timeout 5, path "/health", strategy
//        // "round_robin", max backends 100. YAML example:
//        //
//        //   load_balancer:
//        //     health_check_interval_secs: 30
//        //     health_check_timeout_secs: 5
//        //     health_check_path: /health
//        //     default_strategy: round_robin
//        //     max_backends_per_balancer: 100
//
// 3. `src/core/context.rs` + `src/http/server.rs` — TLS storage
//    wiring. DONE: the context field, the setter, the `set_tls` call
//    in `run_server`, and the `get_tls_status` switch to `ctx.tls()`
//    (with the config-only fallback) are all in place; the snippet
//    below is kept for reference. Without the attached handle the
//    endpoint reports configured certificates from `app.ssl.domains`
//    plus the `ssl.swap` / `ssl.renew` cache only:
//
//        // context.rs — add to `AppContext`:
//        pub tls: Arc<RwLock<Option<Arc<crate::http::tls::SslStorage>>>>,
//        // ... and in `AppContext::new`:
//        tls: Arc::new(RwLock::new(None)),
//        // ... plus a setter:
//        pub fn set_tls(&self, storage: Arc<crate::http::tls::SslStorage>) {
//            *self.tls.write().expect("tls lock poisoned") = Some(storage);
//        }
//
//        // server.rs — in `run_server`, after the TLS storage is
//        // built (`let storage = crate::http::tls::SslStorage::from_config(&ssl)?;`):
//        ctx.set_tls(storage.clone());
//        // ... and in this module's `get_tls_status`, replace the
//        // config-only lookup with:
//        let info = match ctx.tls.read().expect("tls lock poisoned").as_ref() {
//            Some(storage) => Some(tls_certificate_status(storage, &name)),
//            None => Some(tls_status_from_config(&name, &config)),
//        };
//        // (and set `"storage_attached"` from whether the handle is
//        // present). The `SslStorage` is also the resolver behind
//        // `tls::server_config`, so a balancer with
//        // `tls_mode: "terminate"` is served by the same storage the
//        // HTTPS listener already uses; no second TLS stack is built.
//        // Per-balancer listeners (one `bind_rustls_0_23` per
//        // `listen_address:listen_port`) are a server.rs concern:
//        // for each enabled balancer with `tls_mode == terminate`,
//        // bind its listen address with `tls::server_config(&storage)`
//        // and route through the same Actix app, dispatching to the
//        // selected backend. That listener work is intentionally not
//        // in this module: the module owns selection, health, and
//        // stats; the server owns sockets.
//
// 4. Database persistence (optional; the module currently stores
//    everything in memory). Migration
//    `migrations/<timestamp>_load_balancer/up.sql`:
//
//        CREATE TABLE load_balancers (
//            id UUID PRIMARY KEY,
//            name VARCHAR(200) NOT NULL,
//            listen_address VARCHAR(255) NOT NULL,
//            listen_port INTEGER NOT NULL CHECK (listen_port BETWEEN 1 AND 65535),
//            tls_mode VARCHAR(16) NOT NULL DEFAULT 'off'
//                CHECK (tls_mode IN ('off', 'terminate', 'passthrough')),
//            domain VARCHAR(253),
//            certificate_name VARCHAR(253),
//            strategy VARCHAR(32) NOT NULL DEFAULT 'round_robin'
//                CHECK (strategy IN ('round_robin', 'least_connections', 'weighted', 'ip_hash')),
//            health_check JSONB NOT NULL DEFAULT '{}'::jsonb,
//            enabled BOOLEAN NOT NULL DEFAULT TRUE,
//            status VARCHAR(16) NOT NULL DEFAULT 'active'
//                CHECK (status IN ('active', 'degraded', 'disabled')),
//            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
//            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
//            UNIQUE (listen_address, listen_port)
//        );
//        CREATE TABLE lb_backends (
//            id UUID PRIMARY KEY,
//            balancer_id UUID NOT NULL REFERENCES load_balancers(id) ON DELETE CASCADE,
//            name VARCHAR(200) NOT NULL,
//            source_kind VARCHAR(16) NOT NULL DEFAULT 'address'
//                CHECK (source_kind IN ('provider', 'cluster', 'address')),
//            source_id VARCHAR(512),
//            address VARCHAR(255) NOT NULL,
//            weight INTEGER NOT NULL DEFAULT 1 CHECK (weight BETWEEN 1 AND 100),
//            enabled BOOLEAN NOT NULL DEFAULT TRUE,
//            draining BOOLEAN NOT NULL DEFAULT FALSE,
//            health VARCHAR(16) NOT NULL DEFAULT 'unknown'
//                CHECK (health IN ('unknown', 'healthy', 'unhealthy')),
//            last_check_at TIMESTAMPTZ,
//            last_latency_ms BIGINT,
//            consecutive_failures INTEGER NOT NULL DEFAULT 0,
//            consecutive_successes INTEGER NOT NULL DEFAULT 0,
//            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
//            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
//        );
//        CREATE INDEX lb_backends_balancer_id_idx ON lb_backends (balancer_id);
//        CREATE TABLE lb_backend_stats (
//            backend_id UUID PRIMARY KEY REFERENCES lb_backends(id) ON DELETE CASCADE,
//            requests_routed BIGINT NOT NULL DEFAULT 0,
//            errors BIGINT NOT NULL DEFAULT 0,
//            total_latency_ms BIGINT NOT NULL DEFAULT 0,
//            latency_samples BIGINT NOT NULL DEFAULT 0,
//            checks_total BIGINT NOT NULL DEFAULT 0,
//            checks_ok BIGINT NOT NULL DEFAULT 0,
//            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
//        );
//
//    `src/db/schema.rs` — matching Diesel entries:
//
//        diesel::table! {
//            load_balancers (id) {
//                id -> Uuid,
//                name -> Varchar,
//                listen_address -> Varchar,
//                listen_port -> Int4,
//                tls_mode -> Varchar,
//                domain -> Nullable<Varchar>,
//                certificate_name -> Nullable<Varchar>,
//                strategy -> Varchar,
//                health_check -> Jsonb,
//                enabled -> Bool,
//                status -> Varchar,
//                created_at -> Timestamptz,
//                updated_at -> Timestamptz,
//            }
//        }
//        diesel::table! {
//            lb_backends (id) {
//                id -> Uuid,
//                balancer_id -> Uuid,
//                name -> Varchar,
//                source_kind -> Varchar,
//                source_id -> Nullable<Varchar>,
//                address -> Varchar,
//                weight -> Int4,
//                enabled -> Bool,
//                draining -> Bool,
//                health -> Varchar,
//                last_check_at -> Nullable<Timestamptz>,
//                last_latency_ms -> Nullable<Int8>,
//                consecutive_failures -> Int4,
//                consecutive_successes -> Int4,
//                created_at -> Timestamptz,
//                updated_at -> Timestamptz,
//            }
//        }
//        diesel::table! {
//            lb_backend_stats (backend_id) {
//                backend_id -> Uuid,
//                requests_routed -> Int8,
//                errors -> Int8,
//                total_latency_ms -> Int8,
//                latency_samples -> Int8,
//                checks_total -> Int8,
//                checks_ok -> Int8,
//                updated_at -> Timestamptz,
//            }
//        }
//
// 5. Cluster module contract (for `clusters.rs`): emit these hook
//    events with `{"cluster_id", "node_id"}` payloads and this
//    module reacts without importing cluster code:
//
//        hooks.emit("cluster.node.unhealthy", json!({"cluster_id": id, "node_id": id}));
//        hooks.emit("cluster.node.healthy", json!({"cluster_id": id, "node_id": id}));
//        hooks.emit("cluster.node.removed", json!({"cluster_id": id, "node_id": id}));
//
//    Backend `source_id` for cluster backends is
//    `"<cluster_id>/<node_id>"`, matching the event payload.
//
// 6. README.md / TODO.md — suggested lines:
//
//        - Load Balancer (`load_balancer`): balancers over provider,
//          cluster, or plain-address backends; round-robin,
//          least-connections, weighted, and IP-hash strategies;
//          HTTP/TCP health checks with automatic failover and a
//          degraded 503 state; TLS termination through the shared
//          certificate storage.
//        - TODO: persist balancers/backends/stats in PostgreSQL
//          (migration + schema in the integrator notes of
//          `src/modules/load_balancer.rs`); attach the live
//          `SslStorage` handle to `AppContext` for the TLS endpoint;
//          bind per-balancer listeners in `http::server` for real
//          traffic proxying (selection, health, and stats already
//          live in the module).
