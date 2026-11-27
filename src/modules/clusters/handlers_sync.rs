// clusters - HTTP handlers: sync runs, logs, conflicts, and retries.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
struct SyncNowRequest {
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
struct StageItemRequest {
    pub(crate) domain: String,
    pub(crate) key: String,
    pub(crate) payload: Value,
}

#[derive(Debug, Deserialize)]
struct ExportQuery {
    pub(crate) domain: Option<String>,
    since: Option<String>,
}

/// Idempotency scope for running a sync plan now: the scope carries
/// the plan id and the dry-run flag, so the same key for a preview and
/// a real run (or a different plan) is a different operation instead
/// of replaying the first run's summary.
fn sync_run_scope(plan_id: &str, dry_run: bool) -> String {
    format!("clusters:sync:run:{plan_id}:dry_run={dry_run}")
}

pub(crate) async fn sync_now(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
    body: web::Json<SyncNowRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let idempotency_key = match idempotency_key_from(&req) {
        Ok(key) => key,
        Err(error) => return json::error_response(&error, debug),
    };
    let idempotency_scope = sync_run_scope(
        req.match_info().get("id").unwrap_or_default(),
        body.dry_run,
    );
    if let Some(key) = &idempotency_key {
        if let Some((status, body)) = state.idempotency_lookup(&idempotency_scope, key) {
            return json::success_response(body, status);
        }
    }
    let result: UnifiedResult<SyncRunSummary> =
        execute_sync(&state, id.into_inner(), body.dry_run).await;
    // Only successful runs are stored: a failed run must be retried
    // for real, never answered from a stale stored response.
    if let Ok(summary) = &result {
        if let Some(key) = &idempotency_key {
            state.idempotency_store(
                &idempotency_scope,
                key,
                200,
                serde_json::to_value(summary).unwrap_or(Value::Null),
            );
        }
    }
    json::respond(result, debug)
}

/// Reject sync items stamped too far in the future: an unbounded
/// `updated_at` would win every later last-write-wins comparison and
/// poison ordering. The bound is the replay window, the same skew the
/// HMAC authentication tolerates.
fn check_item_timestamps(items: &[SyncItem], max_future: DateTime<Utc>) -> UnifiedResult<()> {
    if items.iter().any(|item| item.updated_at > max_future) {
        return Err(UnifiedError::bad_request(
            "Sync item updated_at is too far in the future",
        ));
    }
    Ok(())
}

/// Verify that the envelope's plan, when this instance knows it,
/// belongs to the sending node and cluster before any item is
/// applied. Plan ids are generated locally per instance and never
/// shared with the peer, so an unknown plan id is the sender's own
/// correlation id and is accepted as-is; a known plan id that points
/// at a different node or cluster is rejected so items cannot be
/// misattributed to a plan the sender does not own.
fn verify_envelope_plan(
    state: &Arc<ClustersState>,
    plan_id: Option<Uuid>,
    cluster_id: Uuid,
    sender_id: Uuid,
) -> UnifiedResult<()> {
    let Some(plan_id) = plan_id else {
        return Ok(());
    };
    let Some(plan) = state
        .plans
        .read()
        .expect("plans lock poisoned")
        .get(&plan_id)
        .cloned()
    else {
        return Ok(());
    };
    if plan.cluster_id != cluster_id || plan.target_node_id != sender_id {
        return Err(UnifiedError::forbidden(
            "Sync envelope plan does not belong to the sending node",
        ));
    }
    Ok(())
}

// Sync log handlers.

pub(crate) async fn list_sync_logs(
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
        let mut items: Vec<SyncLogEntry> = state
            .logs
            .read()
            .expect("logs lock poisoned")
            .iter()
            .cloned()
            .collect();
        if let Some(cluster_id) = params.filters.get("cluster_id") {
            let wanted: Uuid = cluster_id
                .parse()
                .map_err(|_| UnifiedError::bad_request("cluster_id must be a UUID"))?;
            items.retain(|log| log.cluster_id == wanted);
        }
        if let Some(plan_id) = params.filters.get("plan_id") {
            let wanted: Uuid = plan_id
                .parse()
                .map_err(|_| UnifiedError::bad_request("plan_id must be a UUID"))?;
            items.retain(|log| log.plan_id == Some(wanted));
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|log| log.status == *status);
        }
        if let Some(domain) = params.filters.get("domain") {
            items.retain(|log| log.domain == *domain);
        }
        if let Some(direction) = params.filters.get("direction") {
            items.retain(|log| log.direction == *direction);
        }
        items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_sync_log(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let id = id.into_inner();
    let result: UnifiedResult<SyncLogEntry> = state
        .logs
        .read()
        .expect("logs lock poisoned")
        .iter()
        .find(|log| log.id == id)
        .cloned()
        .ok_or_else(|| UnifiedError::not_found("Sync log entry not found"));
    json::respond(result, debug)
}

// Node-to-node sync transport: receive and export.

pub(crate) async fn sync_receive(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ClustersState>>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let sender = verify_node_request(&state, &req, &body)?;
        let envelope: Value = serde_json::from_slice(&body)
            .map_err(|_| UnifiedError::bad_request("Sync envelope is not valid JSON"))?;
        let version = envelope
            .get("version")
            .and_then(|value| value.as_u64())
            .ok_or_else(|| UnifiedError::bad_request("Sync envelope has no version"))?;
        if version != u64::from(SYNC_ENVELOPE_VERSION) {
            return Err(UnifiedError::bad_request(format!(
                "Unsupported sync envelope version: {version}"
            )));
        }
        let domain = envelope
            .get("domain")
            .and_then(|value| value.as_str())
            .ok_or_else(|| UnifiedError::bad_request("Sync envelope has no domain"))?;
        let domain = validate_domain(&state, domain)?;
        let cluster_id = envelope
            .get("cluster_id")
            .and_then(|value| value.as_str())
            .and_then(|text| text.parse::<Uuid>().ok())
            .ok_or_else(|| UnifiedError::bad_request("Sync envelope has no cluster_id"))?;
        if sender.cluster_id != cluster_id {
            return Err(UnifiedError::forbidden(
                "Sending node does not belong to the envelope cluster",
            ));
        }
        let plan_id = envelope
            .get("plan_id")
            .and_then(|value| value.as_str())
            .and_then(|text| text.parse::<Uuid>().ok());
        verify_envelope_plan(&state, plan_id, cluster_id, sender.id)?;
        let items: Vec<SyncItem> = envelope
            .get("items")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .ok_or_else(|| UnifiedError::bad_request("Sync envelope has no items array"))?;
        let max_future =
            now() + chrono::Duration::seconds(state.settings().replay_window_secs);
        check_item_timestamps(&items, max_future)?;
        let started_at = now();
        let received = items.len() as u64;
        let local_payloads: HashMap<String, Value> = local_items_for_domain(&state, &domain)
            .into_iter()
            .map(|item| (item.key, item.payload))
            .collect();
        let remote_payloads: HashMap<String, Value> = items
            .iter()
            .map(|item| (item.key.clone(), item.payload.clone()))
            .collect();
        let (applied, skipped, conflicts) = apply_incoming_items(&state, &domain, &items);
        let finished_at = now();
        let log_id = Uuid::now_v7();
        if !conflicts.is_empty() {
            state.record_conflicts(
                Some(log_id),
                plan_id,
                cluster_id,
                Some(sender.id),
                &conflicts,
                &local_payloads,
                &remote_payloads,
            );
        }
        state.push_log(SyncLogEntry {
            id: log_id,
            plan_id,
            cluster_id,
            node_id: Some(sender.id),
            direction: "push".to_string(),
            domain: domain.clone(),
            status: "completed".to_string(),
            items_sent: 0,
            items_received: received,
            items_applied: applied,
            items_skipped: skipped,
            conflicts: conflicts.clone(),
            errors: Vec::new(),
            dry_run: false,
            started_at,
            finished_at,
        });
        if !conflicts.is_empty() {
            state.emit(
                CLUSTER_SYNC_CONFLICT,
                json!({
                    "cluster_id": cluster_id,
                    "node_id": sender.id,
                    "domain": domain,
                    "conflicts": conflicts,
                }),
            );
        }
        state.emit(
            CLUSTER_SYNC_COMPLETED,
            json!({
                "cluster_id": cluster_id,
                "node_id": sender.id,
                "domain": domain,
                "direction": "push",
                "received": received,
                "applied": applied,
                "skipped": skipped,
                "conflicts": conflicts.len(),
                "finished_at": finished_at,
            }),
        );
        Ok(json!({
            "received": received,
            "applied": applied,
            "skipped": skipped,
            "conflicts": conflicts,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn sync_export(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<ClustersState>>,
    req: HttpRequest,
    query: web::Query<ExportQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // The export request is signed over an empty body.
    let result: UnifiedResult<Value> = (|| {
        let requester = verify_node_request(&state, &req, &[])?;
        let domain = query
            .domain
            .as_deref()
            .ok_or_else(|| UnifiedError::bad_request("Query parameter domain is required"))?;
        let domain = validate_domain(&state, domain)?;
        let since: Option<DateTime<Utc>> = match &query.since {
            Some(text) => Some(
                text.parse::<DateTime<Utc>>().map_err(|_| {
                    UnifiedError::bad_request("since must be an RFC 3339 timestamp")
                })?,
            ),
            None => None,
        };
        let mut items = local_items_for_domain(&state, &domain);
        if let Some(since) = since {
            items.retain(|item| item.updated_at > since);
        }
        items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        let count = items.len();
        Ok(json!({
            "domain": domain,
            "node_id": requester.id,
            "items": items,
            "count": count,
            "exported_at": Utc::now(),
        }))
    })();
    json::respond(result, debug)
}

// Local domain state (what sync sends and receives).

/// Idempotency scope for staging a local item: the scope embeds a hash
/// of the canonical request body (domain, key, payload), so the same
/// key for a different item is a different operation instead of
/// replaying the first staged item's response.
fn stage_local_item_scope(body: &StageItemRequest) -> String {
    let canonical = serde_json::to_string(&json!({
        "domain": body.domain,
        "key": body.key,
        "payload": body.payload,
    }))
    .unwrap_or_default();
    format!("clusters:local-items:stage:{}", hash_secret(&canonical))
}

pub(crate) async fn stage_local_item(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    body: web::Json<StageItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let idempotency_key = match idempotency_key_from(&req) {
        Ok(key) => key,
        Err(error) => return json::error_response(&error, debug),
    };
    let idempotency_scope = stage_local_item_scope(&body);
    if let Some(key) = &idempotency_key {
        if let Some((status, body)) = state.idempotency_lookup(&idempotency_scope, key) {
            return json::success_response(body, status);
        }
    }
    let result: UnifiedResult<SyncItem> = (|| {
        let domain = validate_domain(&state, &body.domain)?;
        let key = require_non_empty(&body.key, "key")?;
        if key.len() > 256 {
            return Err(UnifiedError::bad_request("key is too long"));
        }
        if body.payload.is_null() {
            return Err(UnifiedError::bad_request("payload is required"));
        }
        let item = SyncItem {
            domain: domain.clone(),
            key: key.clone(),
            payload: body.payload.clone(),
            updated_at: now(),
        };
        state
            .local_items
            .write()
            .expect("items lock poisoned")
            .entry(domain)
            .or_default()
            .insert(key, item.clone());
        Ok(item)
    })();
    match result {
        Ok(item) => {
            if let Some(key) = &idempotency_key {
                state.idempotency_store(
                    &idempotency_scope,
                    key,
                    201,
                    serde_json::to_value(&item).unwrap_or(Value::Null),
                );
            }
            json::success_response(item, 201)
        }
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_local_items(
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
        let mut items: Vec<SyncItem> = state
            .local_items
            .read()
            .expect("items lock poisoned")
            .values()
            .flat_map(|entries| entries.values().cloned())
            .collect();
        if let Some(domain) = params.filters.get("domain") {
            items.retain(|item| item.domain == *domain);
        }
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|item| item.key.to_lowercase().contains(&needle));
        }
        items.sort_by(|a, b| {
            a.domain
                .cmp(&b.domain)
                .then(a.updated_at.cmp(&b.updated_at))
        });
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

// Sync health overview, conflict visibility, retry queue, settings and
// the domain registry.

pub(crate) async fn sync_health_overview(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let nodes: Vec<NodeState> = state
            .nodes
            .read()
            .expect("nodes lock poisoned")
            .values()
            .cloned()
            .collect();
        let plans: Vec<SyncPlanState> = state
            .plans
            .read()
            .expect("plans lock poisoned")
            .values()
            .cloned()
            .collect();
        let logs: Vec<SyncLogEntry> = state
            .logs
            .read()
            .expect("logs lock poisoned")
            .iter()
            .cloned()
            .collect();
        let conflicts: Vec<SyncConflictRecord> = state
            .conflicts
            .read()
            .expect("conflicts lock poisoned")
            .iter()
            .cloned()
            .collect();
        let retries: Vec<SyncRetryEntry> = state
            .retries
            .read()
            .expect("retries lock poisoned")
            .iter()
            .cloned()
            .collect();
        let count_health = |health: &str| {
            nodes.iter().filter(|node| node.health == health).count() as u64
        };
        let plans_failed: u64 = plans
            .iter()
            .filter(|plan| {
                plan.last_run_at.is_some()
                    && plan
                        .last_success_at
                        .map_or(true, |success| plan.last_run_at.unwrap() > success)
            })
            .count() as u64;
        let clusters: Vec<ClusterState> = state
            .clusters
            .read()
            .expect("clusters lock poisoned")
            .values()
            .cloned()
            .collect();
        let per_cluster: Vec<Value> = clusters
            .iter()
            .map(|cluster| {
                let cluster_nodes: Vec<&NodeState> = nodes
                    .iter()
                    .filter(|node| node.cluster_id == cluster.id)
                    .collect();
                let unhealthy = cluster_nodes
                    .iter()
                    .filter(|node| node.health == "unhealthy")
                    .count() as u64;
                let open_conflicts = conflicts
                    .iter()
                    .filter(|conflict| {
                        conflict.cluster_id == cluster.id && conflict.status == "open"
                    })
                    .count() as u64;
                let pending_retries = retries
                    .iter()
                    .filter(|entry| {
                        entry.cluster_id == cluster.id && entry.status == "pending"
                    })
                    .count() as u64;
                let dead_letters = retries
                    .iter()
                    .filter(|entry| {
                        entry.cluster_id == cluster.id && entry.status == "dead_letter"
                    })
                    .count() as u64;
                let last_success_at = logs
                    .iter()
                    .filter(|log| log.cluster_id == cluster.id && log.status == "completed")
                    .map(|log| log.finished_at)
                    .max();
                json!({
                    "cluster_id": cluster.id,
                    "cluster_name": cluster.name,
                    "status": cluster.status,
                    "nodes": cluster_nodes.len(),
                    "nodes_unhealthy": unhealthy,
                    "open_conflicts": open_conflicts,
                    "pending_retries": pending_retries,
                    "dead_letters": dead_letters,
                    "last_success_at": last_success_at,
                })
            })
            .collect();
        Ok(json!({
            "clusters": clusters.len(),
            "nodes": nodes.len(),
            "nodes_healthy": count_health("healthy"),
            "nodes_degraded": count_health("degraded"),
            "nodes_unhealthy": count_health("unhealthy"),
            "nodes_unknown": count_health("unknown"),
            "plans": plans.len(),
            "plans_enabled": plans.iter().filter(|plan| plan.enabled).count(),
            "plans_failed": plans_failed,
            "open_conflicts": conflicts.iter().filter(|conflict| conflict.status == "open").count(),
            "conflicts": conflicts.len(),
            "pending_retries": retries.iter().filter(|entry| entry.status == "pending").count(),
            "dead_letters": retries.iter().filter(|entry| entry.status == "dead_letter").count(),
            "failed_logs": logs.iter().filter(|log| log.status == "failed").count(),
            "last_sync_at": logs.iter().map(|log| log.finished_at).max(),
            "last_success_at": logs.iter().filter(|log| log.status == "completed").map(|log| log.finished_at).max(),
            "per_cluster": per_cluster,
        }))
    })();
    json::respond(result, debug)
}

// Conflict visibility and resolution.

pub(crate) async fn list_conflicts(
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
        let mut items: Vec<SyncConflictRecord> = state
            .conflicts
            .read()
            .expect("conflicts lock poisoned")
            .iter()
            .cloned()
            .collect();
        if let Some(cluster_id) = params.filters.get("cluster_id") {
            let wanted: Uuid = cluster_id
                .parse()
                .map_err(|_| UnifiedError::bad_request("cluster_id must be a UUID"))?;
            items.retain(|conflict| conflict.cluster_id == wanted);
        }
        if let Some(plan_id) = params.filters.get("plan_id") {
            let wanted: Uuid = plan_id
                .parse()
                .map_err(|_| UnifiedError::bad_request("plan_id must be a UUID"))?;
            items.retain(|conflict| conflict.plan_id == Some(wanted));
        }
        if let Some(domain) = params.filters.get("domain") {
            items.retain(|conflict| conflict.domain == *domain);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|conflict| conflict.status == *status);
        }
        if let Some(winner) = params.filters.get("winner") {
            items.retain(|conflict| conflict.winner == *winner);
        }
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|conflict| conflict.key.to_lowercase().contains(&needle));
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_conflict(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let id = id.into_inner();
    let result: UnifiedResult<SyncConflictRecord> = state
        .conflicts
        .read()
        .expect("conflicts lock poisoned")
        .iter()
        .find(|conflict| conflict.id == id)
        .cloned()
        .ok_or_else(|| UnifiedError::not_found("Sync conflict not found"));
    json::respond(result, debug)
}

#[derive(Debug, Deserialize)]
struct ResolveConflictRequest {
    /// `keep_local`, `keep_remote` or `dismiss`.
    resolution: String,
    #[serde(default)]
    note: String,
}

pub(crate) async fn resolve_conflict(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
    body: web::Json<ResolveConflictRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let actor = match caller(&req) {
        Ok(who) => who.actor,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<SyncConflictRecord> = (|| {
        let id = id.into_inner();
        let resolution = body.resolution.trim();
        if !matches!(resolution, "keep_local" | "keep_remote" | "dismiss") {
            return Err(UnifiedError::bad_request(
                "resolution must be keep_local, keep_remote or dismiss",
            ));
        }
        if body.note.len() > 1024 {
            return Err(UnifiedError::bad_request("note is too long"));
        }
        let mut conflicts = state.conflicts.write().expect("conflicts lock poisoned");
        let conflict = conflicts
            .iter_mut()
            .find(|conflict| conflict.id == id)
            .ok_or_else(|| UnifiedError::not_found("Sync conflict not found"))?;
        if conflict.status != "open" {
            return Err(UnifiedError::conflict(format!(
                "Sync conflict is already {}",
                conflict.status
            )));
        }
        // An admin override applies the chosen payload as the local
        // item with a fresh timestamp, so the chosen version wins the
        // next sync instead of losing silently again. `dismiss` changes
        // no data; it only closes the record.
        if resolution != "dismiss" {
            let chosen = if resolution == "keep_local" {
                conflict.local_payload.clone()
            } else {
                conflict.remote_payload.clone()
            };
            if let Some(payload) = chosen {
                let mut items = state.local_items.write().expect("items lock poisoned");
                items.entry(conflict.domain.clone()).or_default().insert(
                    conflict.key.clone(),
                    SyncItem {
                        domain: conflict.domain.clone(),
                        key: conflict.key.clone(),
                        payload,
                        updated_at: now(),
                    },
                );
            }
        }
        conflict.status = if resolution == "dismiss" {
            "dismissed".to_string()
        } else {
            "resolved".to_string()
        };
        conflict.resolution = Some(format!(
            "{resolution}{}",
            if body.note.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", body.note.trim())
            }
        ));
        conflict.resolved_by = Some(actor.clone());
        conflict.resolved_at = Some(now());
        let updated = conflict.clone();
        drop(conflicts);
        state.emit(
            CLUSTER_SYNC_CONFLICT_RESOLVED,
            json!({
                "conflict_id": updated.id,
                "cluster_id": updated.cluster_id,
                "domain": updated.domain,
                "key": updated.key,
                "resolution": resolution,
                "resolved_by": actor,
            }),
        );
        Ok(updated)
    })();
    json::respond(result, debug)
}

// Failed-sync retry queue and dead-letter handling.

pub(crate) async fn list_retries(
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
        let mut items: Vec<SyncRetryEntry> = state
            .retries
            .read()
            .expect("retries lock poisoned")
            .iter()
            .cloned()
            .collect();
        if let Some(cluster_id) = params.filters.get("cluster_id") {
            let wanted: Uuid = cluster_id
                .parse()
                .map_err(|_| UnifiedError::bad_request("cluster_id must be a UUID"))?;
            items.retain(|entry| entry.cluster_id == wanted);
        }
        if let Some(plan_id) = params.filters.get("plan_id") {
            let wanted: Uuid = plan_id
                .parse()
                .map_err(|_| UnifiedError::bad_request("plan_id must be a UUID"))?;
            items.retain(|entry| entry.plan_id == wanted);
        }
        if let Some(domain) = params.filters.get("domain") {
            items.retain(|entry| entry.domain == *domain);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|entry| entry.status == *status);
        }
        items.sort_by(|a, b| b.next_retry_at.cmp(&a.next_retry_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_retry(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let id = id.into_inner();
    let result: UnifiedResult<SyncRetryEntry> = state
        .retries
        .read()
        .expect("retries lock poisoned")
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
        .ok_or_else(|| UnifiedError::not_found("Sync retry entry not found"));
    json::respond(result, debug)
}

/// Re-run the sync plan behind a retry entry right now. The entry is
/// marked `retrying` first; a successful run completes it through the
/// normal path, a failed run re-queues it with backoff (or dead-letters
/// it at the attempt limit) through the normal path.
pub(crate) async fn retry_entry_now(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<SyncRetryEntry> = (|| {
        let id = id.into_inner();
        let mut queue = state.retries.write().expect("retries lock poisoned");
        let entry = queue
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| UnifiedError::not_found("Sync retry entry not found"))?;
        if !matches!(entry.status.as_str(), "pending" | "dead_letter" | "retrying") {
            return Err(UnifiedError::conflict(format!(
                "Sync retry entry is {} and cannot be retried",
                entry.status
            )));
        }
        entry.status = "retrying".to_string();
        entry.updated_at = now();
        Ok(entry.clone())
    })();
    let entry = match result {
        Ok(entry) => entry,
        Err(error) => return json::error_response(&error, debug),
    };
    let run = execute_sync(&state, entry.plan_id, false).await;
    if let Err(error) = run {
        // A failure before the domain loop leaves the entry
        // `retrying` with no re-queue; put it back to `pending` (or
        // dead-letter it) instead of returning with it wedged.
        state.requeue_stuck_retries(entry.plan_id, &error.message);
        return json::error_response(&error, debug);
    }
    // `execute_sync` completes or re-queues the entry through the
    // normal path; re-read it so the caller sees the final status.
    let stored: UnifiedResult<SyncRetryEntry> = state
        .retries
        .read()
        .expect("retries lock poisoned")
        .iter()
        .find(|candidate| candidate.id == entry.id)
        .cloned()
        .ok_or_else(|| UnifiedError::not_found("Sync retry entry not found"));
    json::respond(stored, debug)
}

pub(crate) async fn discard_retry(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<SyncRetryEntry> = (|| {
        let id = id.into_inner();
        let mut queue = state.retries.write().expect("retries lock poisoned");
        let entry = queue
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| UnifiedError::not_found("Sync retry entry not found"))?;
        if !matches!(
            entry.status.as_str(),
            "pending" | "retrying" | "dead_letter"
        ) {
            return Err(UnifiedError::conflict(format!(
                "Sync retry entry is {} and cannot be discarded",
                entry.status
            )));
        }
        entry.status = "discarded".to_string();
        entry.updated_at = now();
        Ok(entry.clone())
    })();
    json::respond(result, debug)
}

pub(crate) async fn process_due_retries_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let state_clone = state.clone();
    spawn_background(async move {
        let _ = process_due_retries(&state_clone).await;
    });
    let due: u64 = {
        let now = now();
        state
            .retries
            .read()
            .expect("retries lock poisoned")
            .iter()
            .filter(|entry| entry.status == "pending" && entry.next_retry_at <= now)
            .count() as u64
    };
    json::respond(
        Ok::<Value, UnifiedError>(json!({ "queued": true, "due": due })),
        debug,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage_request(key: &str, payload: &str) -> StageItemRequest {
        StageItemRequest {
            domain: "config".to_string(),
            key: key.to_string(),
            payload: serde_json::serde_json::json!(payload),
        }
    }

    #[test]
    fn stage_local_item_scope_hashes_the_request_body() {
        let first = stage_local_item_scope(&stage_request("k1", "v1"));
        let same = stage_local_item_scope(&stage_request("k1", "v1"));
        let different_key = stage_local_item_scope(&stage_request("k2", "v1"));
        let different_payload = stage_local_item_scope(&stage_request("k1", "v2"));
        assert_eq!(first, same);
        assert_ne!(first, different_key);
        assert_ne!(first, different_payload);
        assert!(first.starts_with("clusters:local-items:stage:"));

        // The same key under different scopes does not collide.
        let state = Arc::new(ClustersState::new());
        state.idempotency_store(&first, "key-1", 201, serde_json::json!({ "key": "k1" }));
        assert!(state.idempotency_lookup(&different_key, "key-1").is_none());
        assert!(state.idempotency_lookup(&first, "key-1").is_some());
    }

    #[test]
    fn sync_run_scope_separates_dry_runs_from_real_runs() {
        assert_eq!(
            sync_run_scope("plan-1", true),
            sync_run_scope("plan-1", true)
        );
        assert_ne!(sync_run_scope("plan-1", true), sync_run_scope("plan-1", false));
        assert_ne!(sync_run_scope("plan-1", true), sync_run_scope("plan-2", true));
    }

    fn sync_item(key: &str, at: DateTime<Utc>) -> SyncItem {
        SyncItem {
            domain: "config".to_string(),
            key: key.to_string(),
            payload: serde_json::serde_json::json!("v"),
            updated_at: at,
        }
    }

    #[test]
    fn check_item_timestamps_rejects_far_future_items() {
        let window = chrono::Duration::seconds(DEFAULT_REPLAY_WINDOW_SECS);
        let max_future = Utc::now() + window;
        // In-window and past items are fine.
        assert!(check_item_timestamps(&[sync_item("k1", Utc::now())], max_future).is_ok());
        assert!(check_item_timestamps(&[], max_future).is_ok());
        // Beyond the replay window: rejected, so a far-future stamp
        // cannot poison last-write-wins ordering.
        let far_future = Utc::now() + window + chrono::Duration::seconds(1);
        assert!(check_item_timestamps(&[sync_item("k1", far_future)], max_future).is_err());
        // One bad item rejects the whole envelope.
        assert!(check_item_timestamps(
            &[sync_item("k1", Utc::now()), sync_item("k2", far_future)],
            max_future
        )
        .is_err());
    }

    fn stored_plan(cluster_id: Uuid, target_node_id: Uuid) -> SyncPlanState {
        SyncPlanState {
            id: Uuid::now_v7(),
            cluster_id,
            target_node_id,
            local_node_id: "local-node".to_string(),
            direction: "pull".to_string(),
            domains: vec!["config".to_string()],
            schedule: "3600".to_string(),
            enabled: true,
            peer_secret_hash: "aabbcc".to_string(),
            peer_secret_fingerprint: None,
            last_run_at: None,
            last_success_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn verify_envelope_plan_checks_ownership_when_known() {
        let state = Arc::new(ClustersState::new());
        let cluster_id = Uuid::now_v7();
        let sender_id = Uuid::now_v7();
        let other_node = Uuid::now_v7();
        let other_cluster = Uuid::now_v7();
        let plan = stored_plan(cluster_id, sender_id);
        let plan_id = plan.id;
        state.plans.write().expect("plans lock poisoned").insert(plan_id, plan);

        // The sender's own plan for this cluster is accepted.
        assert!(verify_envelope_plan(&state, Some(plan_id), cluster_id, sender_id).is_ok());
        // A plan owned by another node is rejected.
        assert!(verify_envelope_plan(&state, Some(plan_id), cluster_id, other_node).is_err());
        // A plan from another cluster is rejected.
        assert!(verify_envelope_plan(&state, Some(plan_id), other_cluster, sender_id).is_err());
        // An envelope without a plan id, or with a plan id this
        // instance never saw (the sender's own correlation id), is
        // accepted: plan ids are generated locally per instance and
        // never shared with the peer.
        assert!(verify_envelope_plan(&state, None, cluster_id, sender_id).is_ok());
        assert!(verify_envelope_plan(&state, Some(Uuid::now_v7()), cluster_id, sender_id).is_ok());
    }
}
