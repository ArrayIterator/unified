// Upstream health checks for the Pingora gateway.
//
// Health model (reused from the load-balancer module, not
// reimplemented):
//   - Probing uses the load balancer's `probe_backend` with a
//     `HealthCheckConfig` (HTTP or TCP), and the same thresholding:
//     `healthy_threshold` consecutive successes recover an unhealthy
//     upstream, `unhealthy_threshold` consecutive failures remove it
//     from rotation.
//   - Health transitions emit hook events (`gateway.upstream.unhealthy`
//     / `gateway.upstream.healthy`); the load balancer's own
//     `lb.backend.unhealthy` / `lb.backend.healthy` events are
//     observed by the management module and update this state too.
//
// Split of responsibilities (forced by the public API surface):
//   - Balancer pools: the load-balancer module owns probing — it has
//     the full backend list and its own sweeper, and the gateway
//     cannot enumerate a balancer's backends (the list accessor is
//     private; only `eligible_backends` is public). The gateway
//     therefore tracks balancer backends event-driven: a backend the
//     load balancer reports unhealthy is excluded from gateway
//     rotation, and a backend it reports healthy is re-admitted. The
//     gateway ALSO probes currently-eligible backends on its sweep so
//     it has latency data and a second opinion; it never probes a
//     backend the load balancer already excluded, because it cannot
//     list it.
//   - Direct address pools: the gateway owns probing end to end. The
//     pool addresses come from the routing rules, so the sweeper can
//     enumerate them, probe them, remove unhealthy ones from
//     rotation, and re-add them on recovery.
//
// Health keys: `lb:<balancer_id>:<backend_id>` for balancer
// backends, `addr:<address>` for direct addresses.

use crate::gateway::{GatewayState, GATEWAY_UPSTREAM_HEALTHY, GATEWAY_UPSTREAM_UNHEALTHY};
use crate::modules::load_balancer::{
    eligible_backends, probe_backend, Backend, HealthCheckConfig, HealthState,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json as json_value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

const SWEEP_TICK_SECS: u64 = 1;

/// Health state of one upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamHealthState {
    Unknown,
    Healthy,
    Unhealthy,
}

impl From<HealthState> for UpstreamHealthState {
    fn from(state: HealthState) -> Self {
        match state {
            HealthState::Unknown => Self::Unknown,
            HealthState::Healthy => Self::Healthy,
            HealthState::Unhealthy => Self::Unhealthy,
        }
    }
}

/// Health record for one upstream.
#[derive(Debug, Clone, Serialize)]
pub struct UpstreamHealth {
    /// `lb:<balancer_id>:<backend_id>` or `addr:<address>`.
    pub key: String,
    /// `"load_balancer_backend"` or `"direct_address"`.
    pub kind: String,
    pub balancer_id: Option<Uuid>,
    pub backend_id: Option<Uuid>,
    /// Dial address (`host:port`).
    pub address: String,
    pub state: UpstreamHealthState,
    pub last_check_at: Option<DateTime<Utc>>,
    pub last_latency_ms: Option<u64>,
    pub consecutive_failures: u32,
    pub consecutive_successes: u32,
    pub checks_total: u64,
    pub checks_ok: u64,
}

/// Health key for a load-balancer backend.
pub fn backend_health_key(balancer_id: Uuid, backend_id: Uuid) -> String {
    format!("lb:{balancer_id}:{backend_id}")
}

/// Health key for a direct address.
pub fn address_health_key(address: &str) -> String {
    format!("addr:{}", address.trim())
}

/// True when the gateway currently considers the upstream healthy
/// enough to receive traffic. Unknown upstreams are eligible
/// (optimistic until the first probe proves otherwise), matching the
/// load balancer's rule.
pub fn is_eligible(state: &GatewayState, key: &str) -> bool {
    state
        .health
        .get(key)
        .map(|entry| entry.state != UpstreamHealthState::Unhealthy)
        .unwrap_or(true)
}

/// Record an upstream as unhealthy (event-driven path, from a load
/// balancer `lb.backend.unhealthy` event). Returns true when the
/// state changed.
pub fn mark_unhealthy(state: &GatewayState, key: &str, address: &str) -> bool {
    mark(state, key, address, UpstreamHealthState::Unhealthy)
}

/// Record an upstream as healthy (event-driven path, from a load
/// balancer `lb.backend.healthy` event). Returns true when the state
/// changed.
pub fn mark_healthy(state: &GatewayState, key: &str, address: &str) -> bool {
    mark(state, key, address, UpstreamHealthState::Healthy)
}

fn mark(state: &GatewayState, key: &str, address: &str, new_state: UpstreamHealthState) -> bool {
    let mut entry = state.health.entry(key.to_string()).or_insert_with(|| UpstreamHealth {
        key: key.to_string(),
        kind: if key.starts_with("lb:") {
            "load_balancer_backend".to_string()
        } else {
            "direct_address".to_string()
        },
        balancer_id: None,
        backend_id: None,
        address: address.to_string(),
        state: UpstreamHealthState::Unknown,
        last_check_at: None,
        last_latency_ms: None,
        consecutive_failures: 0,
        consecutive_successes: 0,
        checks_total: 0,
        checks_ok: 0,
    });
    if entry.state == new_state {
        return false;
    }
    let previous = entry.state;
    entry.state = new_state;
    entry.updated_marker(address);
    let (key, address, balancer_id, backend_id) = (
        entry.key.clone(),
        entry.address.clone(),
        entry.balancer_id,
        entry.backend_id,
    );
    drop(entry);
    let (event, healthy) = if new_state == UpstreamHealthState::Unhealthy {
        (GATEWAY_UPSTREAM_UNHEALTHY, false)
    } else {
        (GATEWAY_UPSTREAM_HEALTHY, true)
    };
    state.emit(
        event,
        json_value!({
            "key": key,
            "address": address,
            "balancer_id": balancer_id,
            "backend_id": backend_id,
            "previous_health": format!("{previous:?}").to_lowercase(),
            "health": if healthy { "healthy" } else { "unhealthy" },
            "source": "load_balancer_event",
        }),
    );
    true
}

impl UpstreamHealth {
    /// Keep the stored address fresh when the same key is reported
    /// with a newer address (a backend whose dial address changed).
    fn updated_marker(&mut self, address: &str) {
        if !address.is_empty() {
            self.address = address.to_string();
        }
        self.last_check_at = Some(Utc::now());
    }
}

/// Deterministic backend id for a direct pool address, derived from
/// the address text. The load balancer's `select_backend` needs a
/// backend id; deriving it from the address keeps IP-hash selection
/// stable across restarts for the same pool.
pub fn backend_id_for_address(address: &str) -> Uuid {
    let mut hasher = Sha256::new();
    hasher.update(b"gateway-direct-upstream:");
    hasher.update(address.trim().as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[0..16]);
    Uuid::from_bytes(bytes)
}

/// Synthesize a load-balancer `Backend` record for a direct address
/// so direct pools can reuse the pure `select_backend` function.
/// `balancer_id` is the rule id (the pool owner), which is also the
/// selection-counter key.
pub fn backend_for_direct_address(
    rule_id: Uuid,
    address: &str,
    weight: u32,
) -> Backend {
    let now = Utc::now();
    Backend {
        id: backend_id_for_address(address),
        balancer_id: rule_id,
        name: address.to_string(),
        source_kind: crate::modules::load_balancer::BackendSourceKind::Address,
        source_id: None,
        address: address.to_string(),
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

/// Apply one probe result to a health record with the same
/// thresholding the load balancer uses. Emits the matching hook
/// event on a state transition.
fn apply_probe_result(
    state: &GatewayState,
    key: &str,
    address: &str,
    healthy: bool,
    latency_ms: u64,
    check: &HealthCheckConfig,
) {
    let mut entry = state.health.entry(key.to_string()).or_insert_with(|| UpstreamHealth {
        key: key.to_string(),
        kind: if key.starts_with("lb:") {
            "load_balancer_backend".to_string()
        } else {
            "direct_address".to_string()
        },
        balancer_id: None,
        backend_id: None,
        address: address.to_string(),
        state: UpstreamHealthState::Unknown,
        last_check_at: None,
        last_latency_ms: None,
        consecutive_failures: 0,
        consecutive_successes: 0,
        checks_total: 0,
        checks_ok: 0,
    });
    let previous = entry.state;
    entry.last_check_at = Some(Utc::now());
    entry.last_latency_ms = Some(latency_ms);
    entry.checks_total = entry.checks_total.saturating_add(1);
    if healthy {
        entry.checks_ok = entry.checks_ok.saturating_add(1);
        entry.consecutive_successes = entry.consecutive_successes.saturating_add(1);
        entry.consecutive_failures = 0;
        if entry.state == UpstreamHealthState::Unhealthy
            && entry.consecutive_successes >= check.healthy_threshold
        {
            entry.state = UpstreamHealthState::Healthy;
        } else if entry.state == UpstreamHealthState::Unknown {
            entry.state = UpstreamHealthState::Healthy;
        }
    } else {
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        entry.consecutive_successes = 0;
        if entry.state != UpstreamHealthState::Unhealthy
            && entry.consecutive_failures >= check.unhealthy_threshold
        {
            entry.state = UpstreamHealthState::Unhealthy;
        }
    }
    let new_state = entry.state;
    let (key, address) = (entry.key.clone(), entry.address.clone());
    drop(entry);
    if new_state != previous {
        let (event, healthy_name) = if new_state == UpstreamHealthState::Unhealthy {
            (GATEWAY_UPSTREAM_UNHEALTHY, "unhealthy")
        } else {
            (GATEWAY_UPSTREAM_HEALTHY, "healthy")
        };
        state.emit(
            event,
            json_value!({
                "key": key,
                "address": address,
                "previous_health": format!("{previous:?}").to_lowercase(),
                "health": healthy_name,
                "latency_ms": latency_ms,
                "source": "gateway_probe",
            }),
        );
    }
}

/// Probe one direct address with a rule's health-check configuration.
async fn probe_direct_address(
    state: &GatewayState,
    rule_id: Uuid,
    address: &str,
    weight: u32,
    check: &HealthCheckConfig,
) {
    let backend = backend_for_direct_address(rule_id, address, weight);
    let outcome = probe_backend(&backend, check).await;
    apply_probe_result(
        state,
        &address_health_key(address),
        address,
        outcome.healthy,
        outcome.latency_ms,
        check,
    );
}

/// Probe one currently-eligible load-balancer backend with the
/// owning rule's health-check configuration.
async fn probe_lb_backend(
    state: &GatewayState,
    backend: &Backend,
    check: &HealthCheckConfig,
) {
    let outcome = probe_backend(backend, check).await;
    apply_probe_result(
        state,
        &backend_health_key(backend.balancer_id, backend.id),
        &backend.address,
        outcome.healthy,
        outcome.latency_ms,
        check,
    );
}

/// One sweep pass: probe every direct pool address that is due, and
/// every currently-eligible load-balancer backend that is due. A
/// backend the load balancer already excluded is not probed here —
/// its recovery is observed through `lb.backend.healthy` events.
/// Direct addresses marked unhealthy ARE re-probed (they are
/// enumerable from the rules), so recovery re-adds them.
async fn sweep_once(state: &GatewayState) {
    let now = Utc::now();
    let rules = crate::gateway::routing::list_rules();
    for rule in &rules {
        if !rule.enabled {
            continue;
        }
        let check = rule.health_check.clone();
        match &rule.upstream {
            crate::gateway::routing::UpstreamRef::Balancer { balancer_id } => {
                for backend in eligible_backends(*balancer_id) {
                    let key = backend_health_key(*balancer_id, backend.id);
                    let due = match state.health.get(&key) {
                        None => true,
                        Some(entry) => match entry.last_check_at {
                            None => true,
                            Some(last) => {
                                now.signed_duration_since(last).num_seconds()
                                    >= check.interval_secs as i64
                            }
                        },
                    };
                    if due {
                        probe_lb_backend(state, &backend, &check).await;
                    }
                }
            }
            crate::gateway::routing::UpstreamRef::Address { address } => {
                let key = address_health_key(address);
                let due = match state.health.get(&key) {
                    None => true,
                    Some(entry) => match entry.last_check_at {
                        None => true,
                        Some(last) => {
                            now.signed_duration_since(last).num_seconds()
                                >= check.interval_secs as i64
                        }
                    },
                };
                if due {
                    probe_direct_address(state, rule.id, address, 1, &check).await;
                }
            }
            crate::gateway::routing::UpstreamRef::AddressPool { addresses } => {
                for entry in addresses {
                    let key = address_health_key(&entry.address);
                    let due = match state.health.get(&key) {
                        None => true,
                        Some(record) => match record.last_check_at {
                            None => true,
                            Some(last) => {
                                now.signed_duration_since(last).num_seconds()
                                    >= check.interval_secs as i64
                            }
                        },
                    };
                    if due {
                        probe_direct_address(state, rule.id, &entry.address, entry.weight, &check)
                            .await;
                    }
                }
            }
        }
    }
}

/// Probe every known upstream now (direct pools and eligible
/// balancer backends), regardless of the sweep schedule. Returns the
/// health snapshot afterwards.
pub async fn check_now(state: &GatewayState) -> Vec<UpstreamHealth> {
    let rules = crate::gateway::routing::list_rules();
    for rule in &rules {
        if !rule.enabled {
            continue;
        }
        let check = rule.health_check.clone();
        match &rule.upstream {
            crate::gateway::routing::UpstreamRef::Balancer { balancer_id } => {
                for backend in eligible_backends(*balancer_id) {
                    probe_lb_backend(state, &backend, &check).await;
                }
            }
            crate::gateway::routing::UpstreamRef::Address { address } => {
                probe_direct_address(state, rule.id, address, 1, &check).await;
            }
            crate::gateway::routing::UpstreamRef::AddressPool { addresses } => {
                for entry in addresses {
                    probe_direct_address(state, rule.id, &entry.address, entry.weight, &check)
                        .await;
                }
            }
        }
    }
    state.health_snapshot()
}

/// Start the background health sweeper on its own thread (same shape
/// as the load balancer's sweeper: a dedicated thread with a
/// current-thread runtime, ticking once per second and sweeping only
/// due upstreams). Safe to call repeatedly: only the first call
/// starts the thread.
pub fn start_health_sweeper(state: Arc<GatewayState>) {
    static SWEEPER_STARTED: AtomicBool = AtomicBool::new(false);
    if SWEEPER_STARTED.swap(true, AtomicOrdering::SeqCst) {
        return;
    }
    let started = std::thread::Builder::new()
        .name("gateway-health-sweeper".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    log::error!("Gateway health sweeper cannot start a runtime: {error}");
                    return;
                }
            };
            loop {
                std::thread::sleep(Duration::from_secs(SWEEP_TICK_SECS));
                let state = state.clone();
                runtime.block_on(sweep_once(&state));
            }
        });
    if let Err(error) = started {
        log::error!("Gateway health sweeper thread failed to start: {error}");
        SWEEPER_STARTED.store(false, AtomicOrdering::SeqCst);
    }
}

/// Selection-time view of a pool: the eligible backends of one load
/// balancer, with the gateway's own unhealthy exclusions applied on
/// top of the load balancer's eligible set.
pub fn eligible_for_gateway(balancer_id: Uuid) -> Vec<Backend> {
    eligible_backends(balancer_id)
        .into_iter()
        .filter(|backend| {
            is_eligible(
                GatewayState::global(),
                &backend_health_key(balancer_id, backend.id),
            )
        })
        .collect()
}

/// Active-connection counts for least-connections selection.
///
/// INTEGRATOR NOTE (dedup): the load-balancer module tracks active
/// connections per backend but its accessor is private, so the
/// gateway passes an empty map today and least-connections falls
/// back to a deterministic tie-break on backend id. The one-line
/// fix is a public snapshot in `load_balancer.rs`:
/// `pub fn backend_connections_snapshot() -> HashMap<Uuid, u64>`,
/// and then this function returns that snapshot instead of an empty
/// map. No other gateway code changes.
pub fn connections_snapshot() -> HashMap<Uuid, u64> {
    HashMap::new()
}
