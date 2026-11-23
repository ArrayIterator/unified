// load_balancer -- Hook emission and hook listeners.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use actix_web::HttpRequest;
use chrono::{DateTime, Utc};
use serde_json::{Value, json, json as json_value};
use std::collections::HashSet;
use uuid::Uuid;

use super::constants::*;
use super::selection::*;
use super::store::*;
use super::types::*;

// ---------------------------------------------------------------------------
// Caller identity and capabilities.
//
// Every handler gates on these first, exactly like the web3 and
// procurement modules: `X-User-Id` (required) plus `X-Role`
// (required), then a `role.check` hook lookup for the permission.
// Fails closed: no answer from the roles module means not allowed.
// ---------------------------------------------------------------------------

pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

// ---------------------------------------------------------------------------
// Hook emission.
// ---------------------------------------------------------------------------

pub(crate) fn emit(event: &str, payload: Value) {
    if let Some(bus) = HOOK_BUS.get() {
        bus.emit(event, payload);
    }
}

pub(crate) fn emit_ctx(ctx: &AppContext, event: &str, payload: Value) {
    ctx.hooks.emit(event, payload);
}

// ---------------------------------------------------------------------------
// Hook listeners (registered through `register_hooks`).
// ---------------------------------------------------------------------------

/// A completed renewal / swap refreshes the cached TLS status for
/// the domain so the TLS endpoint reports the new expiry without a
/// storage handle. Request payloads (no expiry, no completion flag)
/// are ignored.
pub(crate) fn on_ssl_event(context: &mut HookContext) {
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
pub(crate) fn cluster_node_id(context: &HookContext) -> Option<String> {
    let cluster_id = context.get("cluster_id")?.as_str()?;
    let node_id = context.get("node_id")?.as_str()?;
    Some(format!("{cluster_id}/{node_id}"))
}

pub(crate) fn on_cluster_node_unhealthy(context: &mut HookContext) {
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

pub(crate) fn on_cluster_node_healthy(context: &mut HookContext) {
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

pub(crate) fn on_cluster_node_removed(context: &mut HookContext) {
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
