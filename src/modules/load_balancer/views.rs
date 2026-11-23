// load_balancer -- Response views.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::selection::*;
use super::store::*;
use super::types::*;

// ---------------------------------------------------------------------------
// Views.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BalancerView {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) listen_address: String,
    pub(crate) listen_port: u16,
    pub(crate) tls_mode: TlsMode,
    pub(crate) domain: Option<String>,
    pub(crate) certificate_name: Option<String>,
    pub(crate) strategy: Strategy,
    pub(crate) health_check: HealthCheckConfig,
    pub(crate) enabled: bool,
    pub(crate) status: BalancerStatus,
    pub(crate) backend_count: usize,
    pub(crate) healthy_backend_count: usize,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

pub(crate) fn balancer_view(balancer: &LoadBalancer) -> BalancerView {
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
pub(crate) struct BackendView {
    pub(crate) id: Uuid,
    pub(crate) balancer_id: Uuid,
    pub(crate) name: String,
    pub(crate) source_kind: BackendSourceKind,
    pub(crate) source_id: Option<String>,
    pub(crate) address: String,
    pub(crate) weight: u32,
    pub(crate) enabled: bool,
    pub(crate) draining: bool,
    pub(crate) health: HealthState,
    pub(crate) last_check_at: Option<DateTime<Utc>>,
    pub(crate) last_latency_ms: Option<u64>,
    pub(crate) consecutive_failures: u32,
    pub(crate) consecutive_successes: u32,
    pub(crate) active_connections: u64,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

pub(crate) fn backend_view(backend: &Backend) -> BackendView {
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
pub(crate) struct BackendStatsView {
    pub(crate) backend_id: Uuid,
    pub(crate) backend_name: String,
    pub(crate) address: String,
    pub(crate) health: HealthState,
    pub(crate) requests_routed: u64,
    pub(crate) errors: u64,
    pub(crate) average_latency_ms: Option<f64>,
    pub(crate) uptime_ratio: Option<f64>,
    pub(crate) checks_total: u64,
    pub(crate) checks_ok: u64,
    pub(crate) active_connections: u64,
}

pub(crate) fn backend_stats_view(backend: &Backend) -> BackendStatsView {
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
pub(crate) struct BalancerStatsView {
    pub(crate) balancer_id: Uuid,
    pub(crate) balancer_name: String,
    pub(crate) status: BalancerStatus,
    pub(crate) total_backends: usize,
    pub(crate) healthy_backends: usize,
    pub(crate) requests_routed: u64,
    pub(crate) errors: u64,
    pub(crate) average_latency_ms: Option<f64>,
    pub(crate) uptime_ratio: Option<f64>,
    pub(crate) backends: Vec<BackendStatsView>,
}
