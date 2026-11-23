// clusters — HTTP handlers.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateClusterRequest {
    pub(crate) name: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_cluster_kind")]
    kind: String,
}

fn default_cluster_kind() -> String {
    "unified".to_string()
}

#[derive(Debug, Deserialize)]
struct UpdateClusterRequest {
    pub(crate) name: Option<String>,
    description: Option<String>,
    kind: Option<String>,
    pub(crate) status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RegisterNodeRequest {
    display_name: String,
    pub(crate) base_url: String,
    #[serde(default = "default_node_role")]
    pub(crate) role: String,
}

fn default_node_role() -> String {
    "member".to_string()
}

// ---------------------------------------------------------------------------
// Cluster handlers.
// ---------------------------------------------------------------------------

pub(crate) async fn list_clusters(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<ClusterState> = state
            .clusters
            .read()
            .expect("clusters lock poisoned")
            .values()
            .cloned()
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|cluster| {
                cluster.name.to_lowercase().contains(&needle)
                    || cluster.description.to_lowercase().contains(&needle)
            });
        }
        if let Some(kind) = params.filters.get("kind") {
            items.retain(|cluster| cluster.kind == *kind);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|cluster| cluster.status == *status);
        }
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

/// Idempotency scope for creating a cluster: the scope embeds a hash
/// of the canonical request body, so the same key with a different
/// body is a different operation instead of replaying the first
/// cluster's response.
fn create_cluster_scope(body: &CreateClusterRequest) -> String {
    let canonical = serde_json::to_string(&json!({
        "name": body.name,
        "description": body.description,
        "kind": body.kind,
    }))
    .unwrap_or_default();
    format!("clusters:create:{}", hash_secret(&canonical))
}

pub(crate) async fn create_cluster(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    body: web::Json<CreateClusterRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let idempotency_key = match idempotency_key_from(&req) {
        Ok(key) => key,
        Err(error) => return json::error_response(&error, debug),
    };
    let idempotency_scope = create_cluster_scope(&body);
    if let Some(key) = &idempotency_key {
        if let Some((status, body)) = state.idempotency_lookup(&idempotency_scope, key) {
            return json::success_response(body, status);
        }
    }
    let result: UnifiedResult<ClusterState> = (|| {
        let name = require_non_empty(&body.name, "name")?;
        if name.len() > 128 {
            return Err(UnifiedError::bad_request("name is too long"));
        }
        if body.description.len() > 1024 {
            return Err(UnifiedError::bad_request("description is too long"));
        }
        let kind = body.kind.trim();
        if !matches!(kind, "unified" | "provider_pool") {
            return Err(UnifiedError::bad_request(
                "kind must be unified or provider_pool",
            ));
        }
        let cluster = ClusterState {
            id: Uuid::now_v7(),
            name,
            description: body.description.trim().to_string(),
            kind: kind.to_string(),
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .clusters
            .write()
            .expect("clusters lock poisoned")
            .insert(cluster.id, cluster.clone());
        state.emit(
            CLUSTER_CREATED,
            json!({
                "cluster_id": cluster.id,
                "name": cluster.name,
                "kind": cluster.kind,
                "status": cluster.status,
            }),
        );
        Ok(cluster)
    })();
    match result {
        Ok(cluster) => {
            if let Some(key) = &idempotency_key {
                state.idempotency_store(
                    &idempotency_scope,
                    key,
                    201,
                    serde_json::to_value(&cluster).unwrap_or(Value::Null),
                );
            }
            json::success_response(cluster, 201)
        }
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_cluster(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<ClusterState> = state
        .clusters
        .read()
        .expect("clusters lock poisoned")
        .get(&id.into_inner())
        .cloned()
        .ok_or_else(|| UnifiedError::not_found("Cluster not found"));
    json::respond(result, debug)
}

pub(crate) async fn update_cluster(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateClusterRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<ClusterState> = (|| {
        let id = id.into_inner();
        let mut clusters = state.clusters.write().expect("clusters lock poisoned");
        let cluster = clusters
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Cluster not found"))?;
        if let Some(name) = &body.name {
            let name = require_non_empty(name, "name")?;
            if name.len() > 128 {
                return Err(UnifiedError::bad_request("name is too long"));
            }
            cluster.name = name;
        }
        if let Some(description) = &body.description {
            if description.len() > 1024 {
                return Err(UnifiedError::bad_request("description is too long"));
            }
            cluster.description = description.trim().to_string();
        }
        if let Some(kind) = &body.kind {
            let kind = kind.trim();
            if !matches!(kind, "unified" | "provider_pool") {
                return Err(UnifiedError::bad_request(
                    "kind must be unified or provider_pool",
                ));
            }
            cluster.kind = kind.to_string();
        }
        if let Some(status) = &body.status {
            let status = status.trim();
            if !matches!(status, "active" | "paused" | "archived") {
                return Err(UnifiedError::bad_request(
                    "status must be active, paused or archived",
                ));
            }
            cluster.status = status.to_string();
        }
        cluster.updated_at = now();
        let updated = cluster.clone();
        drop(clusters);
        state.emit(
            CLUSTER_UPDATED,
            json!({
                "cluster_id": updated.id,
                "name": updated.name,
                "kind": updated.kind,
                "status": updated.status,
            }),
        );
        Ok(updated)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_cluster(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        let removed = state
            .clusters
            .write()
            .expect("clusters lock poisoned")
            .remove(&id)
            .ok_or_else(|| UnifiedError::not_found("Cluster not found"))?;
        let _ = removed;
        state
            .nodes
            .write()
            .expect("nodes lock poisoned")
            .retain(|_, node| node.cluster_id != id);
        state
            .plans
            .write()
            .expect("plans lock poisoned")
            .retain(|_, plan| plan.cluster_id != id);
        // Logs, conflicts and retry entries owned by the cluster
        // would otherwise be orphaned (and retries would keep
        // pointing at plans that no longer exist).
        state.delete_cluster_records(id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn cluster_sync_status(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        let cluster = state
            .clusters
            .read()
            .expect("clusters lock poisoned")
            .get(&id)
            .cloned()
            .ok_or_else(|| UnifiedError::not_found("Cluster not found"))?;
        let plans: Vec<SyncPlanState> = state
            .plans
            .read()
            .expect("plans lock poisoned")
            .values()
            .filter(|plan| plan.cluster_id == id)
            .cloned()
            .collect();
        let logs: Vec<SyncLogEntry> = state
            .logs
            .read()
            .expect("logs lock poisoned")
            .iter()
            .filter(|log| log.cluster_id == id)
            .cloned()
            .collect();
        let last_sync_at = logs.iter().map(|log| log.finished_at).max();
        let last_success_at = logs
            .iter()
            .filter(|log| log.status == "completed")
            .map(|log| log.finished_at)
            .max();
        // Open conflicts come from the dedicated conflict store, not
        // from the rotating log, so they stay visible until resolved.
        let open_conflicts: u64 = state
            .conflicts
            .read()
            .expect("conflicts lock poisoned")
            .iter()
            .filter(|conflict| conflict.cluster_id == id && conflict.status == "open")
            .count() as u64;
        let conflicts_count: u64 = state
            .conflicts
            .read()
            .expect("conflicts lock poisoned")
            .iter()
            .filter(|conflict| conflict.cluster_id == id)
            .count() as u64;
        let pending_retries: u64 = state
            .retries
            .read()
            .expect("retries lock poisoned")
            .iter()
            .filter(|entry| entry.cluster_id == id && entry.status == "pending")
            .count() as u64;
        let dead_letters: u64 = state
            .retries
            .read()
            .expect("retries lock poisoned")
            .iter()
            .filter(|entry| entry.cluster_id == id && entry.status == "dead_letter")
            .count() as u64;
        let failures: u64 = logs
            .iter()
            .filter(|log| log.status == "failed")
            .count() as u64;
        let plan_domains: HashSet<String> = plans
            .iter()
            .flat_map(|plan| plan.domains.iter().cloned())
            .collect();
        let pending_items: u64 = state
            .local_items
            .read()
            .expect("items lock poisoned")
            .iter()
            .filter(|(domain, _)| plan_domains.contains(*domain))
            .flat_map(|(_, entries)| entries.values())
            .filter(|item| last_success_at.map_or(true, |since| item.updated_at > since))
            .count() as u64;
        let plan_summaries: Vec<Value> = plans
            .iter()
            .map(|plan| {
                json!({
                    "plan_id": plan.id,
                    "target_node_id": plan.target_node_id,
                    "direction": plan.direction,
                    "domains": plan.domains,
                    "enabled": plan.enabled,
                    "last_run_at": plan.last_run_at,
                    "last_success_at": plan.last_success_at,
                })
            })
            .collect();
        Ok(json!({
            "cluster_id": cluster.id,
            "cluster_name": cluster.name,
            "status": cluster.status,
            "last_sync_at": last_sync_at,
            "last_success_at": last_success_at,
            "pending_items": pending_items,
            "conflicts_count": conflicts_count,
            "open_conflicts": open_conflicts,
            "pending_retries": pending_retries,
            "dead_letters": dead_letters,
            "failures": failures,
            "plans": plan_summaries,
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Node handlers.
// ---------------------------------------------------------------------------

/// Idempotency scope for registering a node: the scope embeds a hash
/// of the canonical request body (display name, base URL, role), so
/// the same key for a different node is a different operation.
fn register_node_scope(cluster_id: &str, body: &RegisterNodeRequest) -> String {
    let canonical = serde_json::to_string(&json!({
        "display_name": body.display_name,
        "base_url": body.base_url,
        "role": body.role,
    }))
    .unwrap_or_default();
    format!(
        "clusters:{cluster_id}:nodes:register:{}",
        hash_secret(&canonical)
    )
}

/// A replayed registration must not hand the node secret out again:
/// the secret is returned exactly once, in the original response.
/// Strip it from the stored response before replaying it.
fn strip_registration_secret(mut body: Value) -> Value {
    if let Some(object) = body.as_object_mut() {
        object.remove("secret");
    }
    body
}

pub(crate) async fn register_node(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    cluster_id: web::Path<Uuid>,
    body: web::Json<RegisterNodeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let idempotency_key = match idempotency_key_from(&req) {
        Ok(key) => key,
        Err(error) => return json::error_response(&error, debug),
    };
    let idempotency_scope = register_node_scope(
        req.match_info().get("id").unwrap_or_default(),
        &body,
    );
    if let Some(key) = &idempotency_key {
        if let Some((status, body)) = state.idempotency_lookup(&idempotency_scope, key) {
            return json::success_response(strip_registration_secret(body), status);
        }
    }
    let result: UnifiedResult<Value> = (|| {
        let cluster_id = cluster_id.into_inner();
        if !state
            .clusters
            .read()
            .expect("clusters lock poisoned")
            .contains_key(&cluster_id)
        {
            return Err(UnifiedError::not_found("Cluster not found"));
        }
        let display_name = require_non_empty(&body.display_name, "display_name")?;
        if display_name.len() > 128 {
            return Err(UnifiedError::bad_request("display_name is too long"));
        }
        let base_url = validate_base_url(&body.base_url)?;
        let role = body.role.trim();
        if !matches!(role, "primary" | "member" | "edge") {
            return Err(UnifiedError::bad_request("role must be primary, member or edge"));
        }
        {
            let nodes = state.nodes.read().expect("nodes lock poisoned");
            if nodes.values().any(|node| {
                node.cluster_id == cluster_id
                    && node.base_url == base_url
                    && node.status == "active"
            }) {
                return Err(UnifiedError::conflict(
                    "An active node with the same base_url already exists in this cluster",
                ));
            }
            if role == "primary"
                && nodes.values().any(|node| {
                    node.cluster_id == cluster_id
                        && node.role == "primary"
                        && node.status == "active"
                })
            {
                return Err(UnifiedError::conflict(
                    "This cluster already has a primary node",
                ));
            }
        }
        // The secret is returned exactly once, in this response. Only its
        // hash is stored, and it is never logged.
        let secret = generate_secret();
        let secret_hash = hash_secret(&secret);
        let node = NodeState {
            id: Uuid::now_v7(),
            cluster_id,
            display_name,
            base_url,
            role: role.to_string(),
            status: "active".to_string(),
            secret_fingerprint: secret_fingerprint(&secret_hash),
            secret_hash,
            health: "unknown".to_string(),
            consecutive_failures: 0,
            last_seen_at: None,
            last_latency_ms: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .nodes
            .write()
            .expect("nodes lock poisoned")
            .insert(node.id, node.clone());
        state.emit(
            CLUSTER_NODE_REGISTERED,
            json!({
                "node_id": node.id,
                "cluster_id": cluster_id,
                "display_name": node.display_name,
                "role": node.role,
            }),
        );
        Ok(json!({ "node": node, "secret": secret }))
    })();
    match result {
        Ok(value) => {
            if let Some(key) = &idempotency_key {
                state.idempotency_store(
                    &idempotency_scope,
                    key,
                    201,
                    serde_json::to_value(&value).unwrap_or(Value::Null),
                );
            }
            json::success_response(value, 201)
        }
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_nodes(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    cluster_id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let cluster_id = cluster_id.into_inner();
        if !state
            .clusters
            .read()
            .expect("clusters lock poisoned")
            .contains_key(&cluster_id)
        {
            return Err(UnifiedError::not_found("Cluster not found"));
        }
        let mut items: Vec<NodeState> = state
            .nodes
            .read()
            .expect("nodes lock poisoned")
            .values()
            .filter(|node| node.cluster_id == cluster_id)
            .cloned()
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|node| {
                node.display_name.to_lowercase().contains(&needle)
                    || node.base_url.to_lowercase().contains(&needle)
            });
        }
        if let Some(role) = params.filters.get("role") {
            items.retain(|node| node.role == *role);
        }
        if let Some(health) = params.filters.get("health") {
            items.retain(|node| node.health == *health);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|node| node.status == *status);
        }
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_node(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<NodeState> = state
        .nodes
        .read()
        .expect("nodes lock poisoned")
        .get(&id.into_inner())
        .cloned()
        .ok_or_else(|| UnifiedError::not_found("Node not found"));
    json::respond(result, debug)
}

pub(crate) async fn revoke_node(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<NodeState> = (|| {
        let id = id.into_inner();
        let mut nodes = state.nodes.write().expect("nodes lock poisoned");
        let node = nodes
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Node not found"))?;
        node.status = "revoked".to_string();
        node.health = "unknown".to_string();
        node.updated_at = now();
        let updated = node.clone();
        drop(nodes);
        state.emit(
            CLUSTER_NODE_REVOKED,
            json!({
                "node_id": updated.id,
                "cluster_id": updated.cluster_id,
                "display_name": updated.display_name,
            }),
        );
        Ok(updated)
    })();
    json::respond(result, debug)
}

pub(crate) async fn remove_node(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        let removed = state
            .nodes
            .write()
            .expect("nodes lock poisoned")
            .remove(&id)
            .ok_or_else(|| UnifiedError::not_found("Node not found"))?;
        // Removing the node also removes its plans; their logs,
        // conflicts and retry entries go with them so no retry keeps
        // pointing at a deleted plan.
        let removed_plan_ids: Vec<Uuid> = {
            let mut plans = state.plans.write().expect("plans lock poisoned");
            let ids: Vec<Uuid> = plans
                .values()
                .filter(|plan| plan.target_node_id == id)
                .map(|plan| plan.id)
                .collect();
            plans.retain(|_, plan| plan.target_node_id != id);
            ids
        };
        for plan_id in removed_plan_ids {
            state.delete_plan_records(plan_id);
        }
        state.emit(
            CLUSTER_NODE_REVOKED,
            json!({
                "node_id": removed.id,
                "cluster_id": removed.cluster_id,
                "display_name": removed.display_name,
                "removed": true,
            }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn rotate_node_secret(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        let mut nodes = state.nodes.write().expect("nodes lock poisoned");
        let node = nodes
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Node not found"))?;
        if node.status == "revoked" {
            return Err(UnifiedError::unprocessable("Node credentials have been revoked"));
        }
        // The new secret is returned exactly once, in this response.
        // Only its hash is stored.
        let secret = generate_secret();
        let secret_hash = hash_secret(&secret);
        node.secret_hash = secret_hash;
        node.secret_fingerprint = secret_fingerprint(&node.secret_hash);
        node.updated_at = now();
        let updated = node.clone();
        drop(nodes);
        // The new secret itself is only in this response, never in
        // the event; the fingerprint is enough to correlate.
        state.emit(
            super::types::CLUSTER_NODE_SECRET_ROTATED,
            json!({
                "node_id": updated.id,
                "cluster_id": updated.cluster_id,
                "secret_fingerprint": updated.secret_fingerprint,
            }),
        );
        Ok(json!({ "node": updated, "secret": secret }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn health_check_now(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<HealthCheckEntry> = probe_node(&state, id.into_inner()).await;
    json::respond(result, debug)
}

pub(crate) async fn node_health_history(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        if !state
            .nodes
            .read()
            .expect("nodes lock poisoned")
            .contains_key(&id)
        {
            return Err(UnifiedError::not_found("Node not found"));
        }
        let mut items: Vec<HealthCheckEntry> = state
            .health_history
            .read()
            .expect("health lock poisoned")
            .iter()
            .filter(|entry| entry.node_id == id)
            .cloned()
            .collect();
        if let Some(status) = params.filters.get("status") {
            items.retain(|entry| entry.status == *status);
        }
        items.sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

/// Lightweight probe endpoint other Unified instances call when they
/// health-check this node. No authentication: it reveals nothing but
/// liveness.
pub(crate) async fn peer_health(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(
        Ok::<Value, UnifiedError>(json!({
            "status": "ok",
            "module": "clusters",
            "time": Utc::now(),
            "uptime_seconds": ctx.uptime_seconds(),
        })),
        debug,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cluster_request(name: &str) -> CreateClusterRequest {
        CreateClusterRequest {
            name: name.to_string(),
            description: String::new(),
            kind: "unified".to_string(),
        }
    }

    #[test]
    fn create_cluster_scope_hashes_the_request_body() {
        let first = create_cluster_scope(&cluster_request("alpha"));
        let same = create_cluster_scope(&cluster_request("alpha"));
        let different = create_cluster_scope(&cluster_request("beta"));
        assert_eq!(first, same);
        assert_ne!(first, different);
        assert!(first.starts_with("clusters:create:"));

        // The same key under different scopes does not collide.
        let state = Arc::new(ClustersState::new());
        state.idempotency_store(&first, "key-1", 201, serde_json::json!({ "name": "alpha" }));
        assert!(state.idempotency_lookup(&different, "key-1").is_none());
        assert!(state.idempotency_lookup(&first, "key-1").is_some());
    }

    fn node_request(display_name: &str) -> RegisterNodeRequest {
        RegisterNodeRequest {
            display_name: display_name.to_string(),
            base_url: "https://peer.example.com".to_string(),
            role: "member".to_string(),
        }
    }

    #[test]
    fn register_node_scope_hashes_the_request_body() {
        let first = register_node_scope("cluster-1", &node_request("node-a"));
        let same = register_node_scope("cluster-1", &node_request("node-a"));
        let different = register_node_scope("cluster-1", &node_request("node-b"));
        let other_cluster = register_node_scope("cluster-2", &node_request("node-a"));
        assert_eq!(first, same);
        assert_ne!(first, different);
        assert_ne!(first, other_cluster);
    }

    #[test]
    fn replayed_registration_response_drops_the_secret() {
        let stored = serde_json::json!({
            "node": { "id": "node-1", "display_name": "node-a" },
            "secret": "cls_should-never-be-replayed",
        });
        let replayed = strip_registration_secret(stored);
        assert!(replayed.get("secret").is_none());
        assert_eq!(
            replayed
                .get("node")
                .and_then(|node| node.get("display_name"))
                .and_then(|name| name.as_str()),
            Some("node-a")
        );

        // Bodies without a secret pass through untouched.
        let plain = serde_json::json!({ "node": { "id": "node-1" } });
        assert_eq!(strip_registration_secret(plain.clone()), plain);
        let scalar = serde_json::json!("not-an-object");
        assert_eq!(strip_registration_secret(scalar.clone()), scalar);
    }
}
