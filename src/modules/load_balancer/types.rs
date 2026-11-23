// load_balancer -- Enumerations and stored records.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::constants::*;

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
    /// HTTPS probe (`https://{address}{path}`), verified against the
    /// system CA roots through the shared reqwest client. Backends
    /// with self-signed certificates fail the probe; use plain HTTP
    /// or TCP checks for those.
    Https,
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
    /// for TCP checks. Applies to HTTP and HTTPS checks alike.
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
