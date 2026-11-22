// Pingora gateway proxy.
//
// The application can act as a reverse proxy / gateway in front of
// itself: a Pingora service listens on the gateway addresses, matches
// each request against the routing rules, picks an upstream through the
// load-balancer selection logic, and forwards the request. TLS is
// terminated with the certificates already held by the shared
// `SslStorage` (`crate::http::tls`); the gateway never parses or
// stores certificates itself.
//
// Feature gate: the Pingora dependency is optional. With the
// `gateway` Cargo feature disabled this module still compiles, but it
// exposes no proxy service and every control operation fails with a
// clear "gateway feature not enabled" error (see
// `require_feature_enabled`). No behaviour here is a placeholder: the
// disabled path is a real, explicit refusal, and the enabled path is
// the real Pingora service in `proxy.rs`.
//
// INTEGRATOR PREREQUISITES (shared files this module reads but may
// not edit):
//   - `src/main.rs`: add `mod gateway;` next to the other top-level
//     modules (see the snippet in the coordinator report).
//   - `src/modules/mod.rs`: declare `pub mod load_balancer;` and
//     `pub mod clusters;`, and register the gateway conditionally
//     (see the snippet in the coordinator report). The gateway reads
//     the load-balancer module's public selection API
//     (`eligible_backends`, `select_backend`, `probe_backend`); those
//     paths only resolve once the `load_balancer` module is declared.
//   - `src/core/config.rs`: add the `GatewaySection` config section
//     (see the snippet in the coordinator report) and copy its values
//     into `GatewayState` at startup. Until then the gateway runs on
//     `GatewayConfig::default()` (disabled) and rules are managed
//     through the management API only.

use crate::core::config::AppConfig;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::http::tls::SslStorage;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use uuid::Uuid;

pub mod health;
pub mod manage;
pub mod routing;
pub mod tls;

/// The Pingora reverse proxy service. Only compiled when the
/// `gateway` feature is enabled.
#[cfg(feature = "gateway")]
pub mod proxy;

#[cfg(feature = "gateway")]
pub use proxy::{GatewayProxy, GatewayService, GatewayServiceHandle, UpstreamChoice};
pub use health::{UpstreamHealth, UpstreamHealthState};
#[cfg(feature = "gateway")]
pub use manage::register;
pub use manage::GatewayModule;
pub use routing::{
    rule_matches, select_rule, HeaderMatch, PoolAddress, RoutingRule, UpstreamRef,
};
pub use tls::{certificate_status, TlsStatusView};

/// Hook events emitted by the gateway.
pub const GATEWAY_STARTED: &str = "gateway.started";
pub const GATEWAY_STOPPED: &str = "gateway.stopped";
pub const GATEWAY_RELOADED: &str = "gateway.reloaded";
pub const GATEWAY_UPSTREAM_UNHEALTHY: &str = "gateway.upstream.unhealthy";
pub const GATEWAY_UPSTREAM_HEALTHY: &str = "gateway.upstream.healthy";
pub const GATEWAY_TLS_CHANGED: &str = "gateway.tls.changed";

/// Maximum request-log entries kept in memory (hard cap, independent
/// of the configured capacity, so a bad configuration cannot grow the
/// log without bound).
pub const MAX_REQUEST_LOG_ENTRIES: usize = 10_000;
/// Default number of request-log entries kept.
pub const DEFAULT_REQUEST_LOG_CAPACITY: usize = 500;
/// Default health-check sweep interval in seconds.
pub const DEFAULT_HEALTH_INTERVAL_SECS: u64 = 30;
/// Default upstream connect timeout in seconds.
pub const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 5;

// ---------------------------------------------------------------------------
// Feature gate.
// ---------------------------------------------------------------------------

/// True when this binary was built with the `gateway` Cargo feature
/// (the Pingora proxy service is linked in).
pub const fn feature_enabled() -> bool {
    cfg!(feature = "gateway")
}

/// Refuse an operation when the `gateway` feature is not compiled in.
/// This is the single "feature not enabled" check used by every
/// gateway control path; it returns a real error, never a placeholder.
pub fn require_feature_enabled() -> UnifiedResult<()> {
    if feature_enabled() {
        Ok(())
    } else {
        Err(UnifiedError::unprocessable(
            "Gateway feature not enabled: rebuild with the \"gateway\" Cargo feature enabled",
        )
        .with_metadata("feature", serde_json::json!("gateway")))
    }
}

// ---------------------------------------------------------------------------
// Gateway configuration (runtime view).
//
// The durable YAML section lives in `crate::core::config`
// (`GatewaySection`, added by the integrator); this struct is the
// runtime copy the gateway reads. It is deliberately independent of
// `AppConfig` so the gateway module compiles and can be unit-tested
// without the full configuration tree.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GatewayConfig {
    /// Master switch. The proxy listener only starts when this is true
    /// and the `gateway` feature is compiled in.
    pub enabled: bool,
    /// Plain-HTTP listen addresses, for example `0.0.0.0:8081`.
    pub listen: Vec<String>,
    /// TLS listen addresses, for example `0.0.0.0:8444`. Empty means
    /// no TLS listener is bound.
    pub tls_listen: Vec<String>,
    /// Default upstream used when no routing rule matches: a load
    /// balancer id (UUID) or a plain `host:port` address. `None`
    /// means unmatched requests are rejected.
    pub default_upstream: Option<String>,
    /// Request-log ring buffer capacity (1..=MAX_REQUEST_LOG_ENTRIES).
    pub request_log_capacity: usize,
    /// Health-check sweep interval in seconds (5..=3600).
    pub health_check_interval_secs: u64,
    /// Upstream connect timeout in seconds (1..=60).
    pub connect_timeout_secs: u64,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: vec!["127.0.0.1:8081".to_string()],
            tls_listen: vec!["127.0.0.1:8444".to_string()],
            default_upstream: None,
            request_log_capacity: DEFAULT_REQUEST_LOG_CAPACITY,
            health_check_interval_secs: DEFAULT_HEALTH_INTERVAL_SECS,
            connect_timeout_secs: DEFAULT_CONNECT_TIMEOUT_SECS,
        }
    }
}

impl GatewayConfig {
    /// Validate the configuration. Returns every problem found; an
    /// empty list means the configuration is valid. Mirrors the
    /// listen-address validation in `crate::core::config` (that
    /// helper is private to the config module, so the check is
    /// repeated here rather than imported).
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.listen.is_empty() && self.tls_listen.is_empty() {
            problems.push(
                "gateway.listen and gateway.tls_listen must not both be empty".to_string(),
            );
        }
        for address in self.listen.iter().chain(self.tls_listen.iter()) {
            if let Err(message) = validate_listen_address(address) {
                problems.push(format!("gateway listen address: {message}"));
            }
        }
        if !(1..=MAX_REQUEST_LOG_ENTRIES).contains(&self.request_log_capacity) {
            problems.push(format!(
                "gateway.request_log_capacity must be between 1 and {MAX_REQUEST_LOG_ENTRIES}, got {}",
                self.request_log_capacity
            ));
        }
        if !(5..=3600).contains(&self.health_check_interval_secs) {
            problems.push(format!(
                "gateway.health_check_interval_secs must be between 5 and 3600, got {}",
                self.health_check_interval_secs
            ));
        }
        if !(1..=60).contains(&self.connect_timeout_secs) {
            problems.push(format!(
                "gateway.connect_timeout_secs must be between 1 and 60, got {}",
                self.connect_timeout_secs
            ));
        }
        if let Some(upstream) = &self.default_upstream {
            let trimmed = upstream.trim();
            if trimmed.is_empty() {
                problems.push("gateway.default_upstream must not be empty".to_string());
            } else if trimmed.parse::<Uuid>().is_err()
                && crate::modules::load_balancer::parse_host_port(trimmed).is_none()
            {
                problems.push(format!(
                    "gateway.default_upstream must be a load balancer UUID or a \"host:port\" address, got \"{trimmed}\""
                ));
            }
        }
        problems
    }

    /// Check that the gateway may run: the feature must be compiled
    /// in, the configuration must be enabled, and the configuration
    /// must validate. The running configuration is never replaced by
    /// an invalid one.
    pub fn check(&self) -> UnifiedResult<()> {
        require_feature_enabled()?;
        if !self.enabled {
            return Err(UnifiedError::unprocessable(
                "Gateway is disabled in configuration (gateway.enabled is false)",
            ));
        }
        let problems = self.validate();
        if problems.is_empty() {
            Ok(())
        } else {
            Err(UnifiedError::bad_request(format!(
                "Invalid gateway configuration: {}",
                problems.join("; ")
            )))
        }
    }
}

/// Validate one listen address (`host:port`). Accepts IP addresses
/// and `localhost`, mirroring `crate::core::config`.
fn validate_listen_address(address: &str) -> Result<(), String> {
    match address.parse::<SocketAddr>() {
        Ok(addr) => {
            if addr.port() == 0 {
                return Err(format!("port must not be 0 in \"{address}\""));
            }
            Ok(())
        }
        Err(_) => {
            if let Some((host, port)) = address.rsplit_once(':') {
                if host == "localhost" && port.parse::<u16>().is_ok_and(|p| p != 0) {
                    return Ok(());
                }
            }
            Err(format!(
                "\"{address}\" is not a valid listen address (expected host:port)"
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Request log.
// ---------------------------------------------------------------------------

/// One proxied request, as kept in the in-memory ring buffer. Method,
/// host, path, status, duration, and upstream only: request and
/// response bodies are never logged, and neither are authorization
/// headers (they are never read into this record at all).
#[derive(Debug, Clone, Serialize)]
pub struct RequestLogEntry {
    pub id: Uuid,
    pub request_id: String,
    pub method: String,
    pub host: String,
    pub path: String,
    pub status: u16,
    pub duration_ms: u64,
    /// Dial address of the upstream that served the request, when one
    /// was selected.
    pub upstream: Option<String>,
    pub backend_id: Option<Uuid>,
    pub recorded_at: DateTime<Utc>,
}

impl RequestLogEntry {
    pub fn new(
        request_id: String,
        method: String,
        host: String,
        path: String,
        status: u16,
        duration_ms: u64,
        upstream: Option<String>,
        backend_id: Option<Uuid>,
    ) -> Self {
        Self {
            id: Uuid::now_v7(),
            request_id,
            method,
            host,
            path,
            status,
            duration_ms,
            upstream,
            backend_id,
            recorded_at: Utc::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// Shared gateway state.
// ---------------------------------------------------------------------------

/// State shared by the Pingora service, the health sweeper, and the
/// management module. Routing rules live in `routing.rs` (a process
/// wide store, like the load balancer's balancers); everything else
/// the proxy needs at request time lives here.
pub struct GatewayState {
    /// Runtime gateway configuration.
    pub config: RwLock<GatewayConfig>,
    /// Handle to the application configuration, set by
    /// `GatewayModule::init`. Used for the config-only TLS fallback
    /// when no `SslStorage` handle is attached yet.
    pub app_config: RwLock<Option<Arc<RwLock<AppConfig>>>>,
    /// Live TLS storage, attached by the HTTP server once it builds
    /// the storage (see the integrator snippet for
    /// `src/http/server.rs`). Until it is attached, TLS status is
    /// reported from `app.ssl.domains` only.
    pub tls_storage: RwLock<Option<Arc<SslStorage>>>,
    /// Hook bus, set by `GatewayModule::init`. Used to emit gateway
    /// lifecycle and upstream-health events.
    pub hooks: RwLock<Option<Arc<HookBus>>>,
    /// Upstream health, keyed by `health::backend_health_key` or
    /// `health::address_health_key`.
    pub health: DashMap<String, UpstreamHealth>,
    /// Proxied-request ring buffer (newest entries at the back).
    pub request_log: Mutex<VecDeque<RequestLogEntry>>,
    /// True while the Pingora service thread is serving traffic.
    pub running: AtomicBool,
    /// True once a service thread has ever been started in this
    /// process. A stopped service cannot rebind its listen addresses
    /// in this process (see `proxy::GatewayService::stop`), so a
    /// restart after a stop requires a daemon restart.
    pub ever_started: AtomicBool,
    /// True once a stop (drain) has been requested.
    pub stop_requested: AtomicBool,
    /// When the current service run started.
    pub started_at: RwLock<Option<DateTime<Utc>>>,
    /// Per-upstream selection counters, keyed by load balancer id
    /// (balancer pools) or rule id (direct address pools). The
    /// load-balancer module's own round-robin counter is private, so
    /// the gateway keeps its own counter and passes it to the pure
    /// `select_backend` function; selection stays deterministic per
    /// counter value either way.
    pub counters: DashMap<Uuid, AtomicU64>,
    /// The running Pingora service handle. Only present when the
    /// `gateway` feature is compiled in.
    #[cfg(feature = "gateway")]
    pub service: Mutex<Option<GatewayServiceHandle>>,
}

impl GatewayState {
    pub fn new() -> Self {
        Self {
            config: RwLock::new(GatewayConfig::default()),
            app_config: RwLock::new(None),
            tls_storage: RwLock::new(None),
            hooks: RwLock::new(None),
            health: DashMap::new(),
            request_log: Mutex::new(VecDeque::new()),
            running: AtomicBool::new(false),
            ever_started: AtomicBool::new(false),
            stop_requested: AtomicBool::new(false),
            started_at: RwLock::new(None),
            counters: DashMap::new(),
            #[cfg(feature = "gateway")]
            service: Mutex::new(None),
        }
    }

    /// The process-wide gateway state, shared by the proxy service,
    /// the health sweeper, and the management handlers.
    pub fn global() -> &'static Arc<Self> {
        static STATE: OnceLock<Arc<GatewayState>> = OnceLock::new();
        STATE.get_or_init(|| Arc::new(GatewayState::new()))
    }

    pub fn config_snapshot(&self) -> GatewayConfig {
        self.config.read().expect("gateway config lock poisoned").clone()
    }

    pub fn replace_config(&self, config: GatewayConfig) -> UnifiedResult<()> {
        let problems = config.validate();
        if !problems.is_empty() {
            return Err(UnifiedError::bad_request(format!(
                "Invalid gateway configuration: {}",
                problems.join("; ")
            )));
        }
        *self.config.write().expect("gateway config lock poisoned") = config;
        Ok(())
    }

    pub fn set_app_config(&self, config: Arc<RwLock<AppConfig>>) {
        *self.app_config.write().expect("gateway app config lock poisoned") = Some(config);
    }

    pub fn app_config_snapshot(&self) -> Option<AppConfig> {
        self.app_config
            .read()
            .expect("gateway app config lock poisoned")
            .as_ref()
            .map(|config| config.read().expect("app config lock poisoned").clone())
    }

    /// Attach the live TLS storage handle. Called by the HTTP server
    /// after it builds the storage (integrator snippet).
    pub fn set_tls_storage(&self, storage: Arc<SslStorage>) {
        *self.tls_storage.write().expect("gateway tls lock poisoned") = Some(storage);
    }

    pub fn tls_storage(&self) -> Option<Arc<SslStorage>> {
        self.tls_storage.read().expect("gateway tls lock poisoned").clone()
    }

    pub fn tls_storage_attached(&self) -> bool {
        self.tls_storage().is_some()
    }

    pub fn set_hooks(&self, hooks: Arc<HookBus>) {
        *self.hooks.write().expect("gateway hooks lock poisoned") = Some(hooks);
    }

    pub fn emit(&self, event: &str, payload: serde_json::Value) {
        if let Some(hooks) = self.hooks.read().expect("gateway hooks lock poisoned").clone() {
            hooks.emit(event, payload);
        }
    }

    /// Next selection counter for one upstream pool. Monotonic per
    /// pool id; wraps naturally at `u64::MAX` (round-robin and
    /// weighted selection use the counter modulo the pool size).
    pub fn next_counter(&self, pool_id: Uuid) -> u64 {
        self.counters
            .entry(pool_id)
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, AtomicOrdering::SeqCst)
    }

    pub fn is_running(&self) -> bool {
        self.running.load(AtomicOrdering::SeqCst)
    }

    pub fn started_at(&self) -> Option<DateTime<Utc>> {
        *self.started_at.read().expect("gateway started_at lock poisoned")
    }

    /// Append one proxied request to the ring buffer, evicting the
    /// oldest entries past the configured capacity.
    pub fn record_request(&self, entry: RequestLogEntry) {
        let capacity = self
            .config_snapshot()
            .request_log_capacity
            .clamp(1, MAX_REQUEST_LOG_ENTRIES);
        let mut log = self.request_log.lock().expect("gateway request log lock poisoned");
        log.push_back(entry);
        while log.len() > capacity {
            log.pop_front();
        }
    }

    /// Request-log snapshot, newest first.
    pub fn request_log_snapshot(&self) -> Vec<RequestLogEntry> {
        let log = self.request_log.lock().expect("gateway request log lock poisoned");
        let mut entries: Vec<RequestLogEntry> = log.iter().cloned().collect();
        entries.reverse();
        entries
    }

    pub fn health_snapshot(&self) -> Vec<UpstreamHealth> {
        let mut entries: Vec<UpstreamHealth> =
            self.health.iter().map(|entry| entry.value().clone()).collect();
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        entries
    }
}

impl Default for GatewayState {
    fn default() -> Self {
        Self::new()
    }
}

/// Attach the live TLS storage handle to the process-wide gateway
/// state. Convenience wrapper for the HTTP server wiring snippet.
pub fn set_tls_storage(storage: Arc<SslStorage>) {
    GatewayState::global().set_tls_storage(storage);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_flag_matches_cfg() {
        assert_eq!(feature_enabled(), cfg!(feature = "gateway"));
    }

    #[test]
    fn default_config_is_disabled_and_valid() {
        let config = GatewayConfig::default();
        assert!(!config.enabled);
        assert!(config.validate().is_empty());
    }

    #[test]
    fn config_validation_rejects_bad_listen_address() {
        let mut config = GatewayConfig::default();
        config.listen = vec!["not-an-address".to_string()];
        assert!(!config.validate().is_empty());
    }

    #[test]
    fn request_log_ring_buffer_evicts_oldest() {
        let state = GatewayState::new();
        let mut config = GatewayConfig::default();
        config.request_log_capacity = 2;
        state.replace_config(config).expect("valid config");
        for index in 0..4 {
            state.record_request(RequestLogEntry::new(
                format!("req-{index}"),
                "GET".to_string(),
                "example.com".to_string(),
                "/".to_string(),
                200,
                1,
                None,
                None,
            ));
        }
        let snapshot = state.request_log_snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].request_id, "req-3");
        assert_eq!(snapshot[1].request_id, "req-2");
    }
}
