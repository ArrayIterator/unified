// load_balancer -- Backend selection (pure functions).

use chrono::Utc;
use serde_json::json as json_value;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::HashMap;
use uuid::Uuid;

use super::constants::*;
use super::hooks::*;
use super::store::*;
use super::types::*;

// ---------------------------------------------------------------------------
// Backend selection (pure functions, unit-tested below).
// ---------------------------------------------------------------------------

pub(crate) fn hash_u64(data: &[u8]) -> u64 {
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
/// - IP hash: consistent hashing over the backend ids, with
///   `IP_HASH_VIRTUAL_NODES` virtual nodes (replicas) per backend
///   so traffic spreads evenly; the same client IP always maps to
///   the same backend while the set is unchanged, and only the
///   neighbours of a removed backend move.
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
                let load_a =
                    connections.get(&a.id).copied().unwrap_or(0) as f64 / a.weight.max(1) as f64;
                let load_b =
                    connections.get(&b.id).copied().unwrap_or(0) as f64 / b.weight.max(1) as f64;
                load_a
                    .partial_cmp(&load_b)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| a.id.cmp(&b.id))
            })
            .map(|backend| backend.id),
        Strategy::IpHash => {
            let ip = client_ip.unwrap_or("0.0.0.0");
            let hash = hash_u64(ip.as_bytes());
            // One ring point per backend spreads traffic lumpily
            // (a single point lands wherever it lands). Give each
            // backend many virtual nodes so the ring is dense and
            // every backend owns a fair share of the key space.
            let mut ring: Vec<(u64, Uuid)> = Vec::with_capacity(
                backends.len().saturating_mul(IP_HASH_VIRTUAL_NODES),
            );
            for backend in backends {
                for replica in 0..IP_HASH_VIRTUAL_NODES {
                    let point = hash_u64(
                        format!("{}:{replica}", backend.id).as_bytes(),
                    );
                    ring.push((point, backend.id));
                }
            }
            ring.sort();
            ring.iter()
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
    backends.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    backends
}

pub(crate) fn all_backends(balancer_id: Uuid) -> Vec<Backend> {
    let mut backends: Vec<Backend> = backends_store()
        .iter()
        .filter(|entry| entry.balancer_id == balancer_id)
        .map(|entry| entry.clone())
        .collect();
    backends.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    backends
}

/// Effective balancer status from its current state and backends.
pub(crate) fn compute_effective_status(balancer: &LoadBalancer) -> BalancerStatus {
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
/// when the balancer transitions into the degraded state, and
/// `lb.recovered` when it transitions back from degraded to active.
pub(crate) fn refresh_balancer_status(balancer_id: Uuid) {
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
    } else if new_status == BalancerStatus::Active && was == BalancerStatus::Degraded {
        emit(
            LB_RECOVERED,
            json_value!({
                "balancer_id": id,
                "balancer_name": name,
                "reason": "backends_healthy",
                "degraded": false,
            }),
        );
    }
}
