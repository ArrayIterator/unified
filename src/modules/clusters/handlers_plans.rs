// clusters — HTTP handlers: sync plans.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
struct CreatePlanRequest {
    pub(crate) cluster_id: Uuid,
    pub(crate) target_node_id: Uuid,
    pub(crate) local_node_id: String,
    #[serde(default = "default_direction")]
    pub(crate) direction: String,
    pub(crate) domains: Vec<String>,
    #[serde(default = "default_schedule")]
    schedule: String,
    /// The secret the peer gave this instance at registration. It is
    /// hashed immediately and never stored or returned.
    peer_secret: String,
}

fn default_direction() -> String {
    "bidirectional".to_string()
}

fn default_schedule() -> String {
    "3600".to_string()
}

#[derive(Debug, Deserialize)]
struct UpdatePlanRequest {
    pub(crate) direction: Option<String>,
    pub(crate) domains: Option<Vec<String>>,
    schedule: Option<String>,
    pub(crate) enabled: Option<bool>,
    /// A replacement peer secret; hashed immediately, never stored.
    peer_secret: Option<String>,
}

// ---------------------------------------------------------------------------
// Sync plan handlers.
// ---------------------------------------------------------------------------

pub(crate) async fn list_plans(
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
        let mut items: Vec<SyncPlanState> = state
            .plans
            .read()
            .expect("plans lock poisoned")
            .values()
            .cloned()
            .collect();
        if let Some(cluster_id) = params.filters.get("cluster_id") {
            let wanted: Uuid = cluster_id
                .parse()
                .map_err(|_| UnifiedError::bad_request("cluster_id must be a UUID"))?;
            items.retain(|plan| plan.cluster_id == wanted);
        }
        if let Some(direction) = params.filters.get("direction") {
            items.retain(|plan| plan.direction == *direction);
        }
        if let Some(enabled) = params.filters.get("enabled") {
            let wanted = enabled == "true" || enabled == "1";
            items.retain(|plan| plan.enabled == wanted);
        }
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

/// Idempotency scope for creating a sync plan: the scope embeds a hash
/// of the canonical request body, excluding the peer secret (which is
/// hashed separately and must never appear in a stored scope). The
/// same key with a different body is a different operation instead of
/// replaying the first plan's response.
fn create_plan_scope(body: &CreatePlanRequest) -> String {
    let canonical = serde_json::to_string(&json!({
        "cluster_id": body.cluster_id,
        "target_node_id": body.target_node_id,
        "local_node_id": body.local_node_id,
        "direction": body.direction,
        "domains": body.domains,
        "schedule": body.schedule,
    }))
    .unwrap_or_default();
    format!("clusters:plans:create:{}", hash_secret(&canonical))
}

pub(crate) async fn create_plan(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    body: web::Json<CreatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let idempotency_key = match idempotency_key_from(&req) {
        Ok(key) => key,
        Err(error) => return json::error_response(&error, debug),
    };
    let idempotency_scope = create_plan_scope(&body);
    if let Some(key) = &idempotency_key {
        if let Some((status, body)) = state.idempotency_lookup(&idempotency_scope, key) {
            return json::success_response(body, status);
        }
    }
    let result: UnifiedResult<SyncPlanState> = (|| {
        if !state
            .clusters
            .read()
            .expect("clusters lock poisoned")
            .contains_key(&body.cluster_id)
        {
            return Err(UnifiedError::not_found("Cluster not found"));
        }
        let target = state
            .nodes
            .read()
            .expect("nodes lock poisoned")
            .get(&body.target_node_id)
            .cloned()
            .ok_or_else(|| UnifiedError::not_found("Target node not found"))?;
        if target.cluster_id != body.cluster_id {
            return Err(UnifiedError::unprocessable(
                "Target node does not belong to this cluster",
            ));
        }
        if target.status == "revoked" {
            return Err(UnifiedError::unprocessable("Target node credentials have been revoked"));
        }
        let local_node_id = require_non_empty(&body.local_node_id, "local_node_id")?;
        if local_node_id.len() > 128 {
            return Err(UnifiedError::bad_request("local_node_id is too long"));
        }
        let direction = body.direction.trim();
        if !matches!(direction, "push" | "pull" | "bidirectional") {
            return Err(UnifiedError::bad_request(
                "direction must be push, pull or bidirectional",
            ));
        }
        let domains = validate_domains(&state, body.domains.clone())?;
        if !valid_schedule(&body.schedule) {
            return Err(UnifiedError::bad_request(
                "schedule must be an interval in seconds or a cron-like expression such as \"*/5 * * * *\"",
            ));
        }
        if body.peer_secret.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "peer_secret is required: the secret the peer gave this instance at registration",
            ));
        }
        // Hash immediately; the raw secret is never stored or returned.
        let peer_secret_hash = hash_secret(body.peer_secret.trim());
        let plan = SyncPlanState {
            id: Uuid::now_v7(),
            cluster_id: body.cluster_id,
            target_node_id: body.target_node_id,
            local_node_id,
            direction: direction.to_string(),
            domains,
            schedule: body.schedule.trim().to_string(),
            enabled: true,
            peer_secret_fingerprint: Some(secret_fingerprint(&peer_secret_hash)),
            peer_secret_hash,
            last_run_at: None,
            last_success_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .plans
            .write()
            .expect("plans lock poisoned")
            .insert(plan.id, plan.clone());
        Ok(plan)
    })();
    match result {
        Ok(plan) => {
            if let Some(key) = &idempotency_key {
                state.idempotency_store(
                    &idempotency_scope,
                    key,
                    201,
                    serde_json::to_value(&plan).unwrap_or(Value::Null),
                );
            }
            json::success_response(plan, 201)
        }
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_plan(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<SyncPlanState> = state
        .plans
        .read()
        .expect("plans lock poisoned")
        .get(&id.into_inner())
        .cloned()
        .ok_or_else(|| UnifiedError::not_found("Sync plan not found"));
    json::respond(result, debug)
}

/// Apply an update request to a plan record. When the update changes
/// the sync shape (direction or domains) the incremental cursors are
/// cleared: `last_success_at` is the `since` cursor for pulls and it
/// only makes sense for the previous shape — after the change the
/// first pull must fetch everything the new shape covers instead of
/// skipping items the old shape never pulled.
fn apply_plan_update(
    state: &Arc<ClustersState>,
    plan: &mut SyncPlanState,
    body: &UpdatePlanRequest,
) -> UnifiedResult<()> {
    let mut shape_changed = false;
    if let Some(direction) = &body.direction {
        let direction = direction.trim();
        if !matches!(direction, "push" | "pull" | "bidirectional") {
            return Err(UnifiedError::bad_request(
                "direction must be push, pull or bidirectional",
            ));
        }
        if plan.direction != direction {
            shape_changed = true;
        }
        plan.direction = direction.to_string();
    }
    if let Some(domains) = &body.domains {
        let domains = validate_domains(state, domains.clone())?;
        if plan.domains != domains {
            shape_changed = true;
        }
        plan.domains = domains;
    }
    if let Some(schedule) = &body.schedule {
        if !valid_schedule(schedule) {
            return Err(UnifiedError::bad_request(
                "schedule must be an interval in seconds or a cron-like expression such as \"*/5 * * * *\"",
            ));
        }
        plan.schedule = schedule.trim().to_string();
    }
    if let Some(enabled) = body.enabled {
        plan.enabled = enabled;
    }
    if let Some(secret) = &body.peer_secret {
        if secret.trim().is_empty() {
            return Err(UnifiedError::bad_request("peer_secret must not be empty"));
        }
        let hash = hash_secret(secret.trim());
        plan.peer_secret_fingerprint = Some(secret_fingerprint(&hash));
        plan.peer_secret_hash = hash;
    }
    if shape_changed {
        plan.last_success_at = None;
        plan.last_run_at = None;
    }
    Ok(())
}

pub(crate) async fn update_plan(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<ClustersState>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_permission(&ctx, &req, PERM_CLUSTERS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<SyncPlanState> = (|| {
        let id = id.into_inner();
        let mut plans = state.plans.write().expect("plans lock poisoned");
        let plan = plans
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Sync plan not found"))?;
        apply_plan_update(&state, plan, &body)?;
        plan.updated_at = now();
        Ok(plan.clone())
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_plan(
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
        let plan_id = id.into_inner();
        state
            .plans
            .write()
            .expect("plans lock poisoned")
            .remove(&plan_id)
            .ok_or_else(|| UnifiedError::not_found("Sync plan not found"))?;
        // Logs, conflicts and retry entries owned by the plan would
        // otherwise keep pointing at a plan that no longer exists.
        state.delete_plan_records(plan_id);
        Ok(json!({ "deleted": true, "id": plan_id }))
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn plan_request(direction: &str, domains: Vec<&str>) -> CreatePlanRequest {
        CreatePlanRequest {
            cluster_id: Uuid::now_v7(),
            target_node_id: Uuid::now_v7(),
            local_node_id: "local-node".to_string(),
            direction: direction.to_string(),
            domains: domains.into_iter().map(str::to_string).collect(),
            schedule: "3600".to_string(),
            peer_secret: "peer-secret".to_string(),
        }
    }

    #[test]
    fn create_plan_scope_hashes_the_request_body() {
        let first = create_plan_scope(&plan_request("pull", vec!["config"]));
        let same = create_plan_scope(&plan_request("pull", vec!["config"]));
        assert_eq!(first, same);
        assert!(first.starts_with("clusters:plans:create:"));

        // A different direction or domain is a different operation,
        // even with the same idempotency key.
        let different_direction = create_plan_scope(&plan_request("push", vec!["config"]));
        assert_ne!(first, different_direction);
        let different_domain = create_plan_scope(&plan_request("pull", vec!["modules"]));
        assert_ne!(first, different_domain);

        // The peer secret never changes the scope: two requests that
        // differ only by secret are the same operation.
        let mut rotated = plan_request("pull", vec!["config"]);
        rotated.peer_secret = "a-different-secret".to_string();
        assert_eq!(first, create_plan_scope(&rotated));
    }

    fn stored_plan() -> SyncPlanState {
        let at = Utc::now() - chrono::Duration::seconds(60);
        SyncPlanState {
            id: Uuid::now_v7(),
            cluster_id: Uuid::now_v7(),
            target_node_id: Uuid::now_v7(),
            local_node_id: "local-node".to_string(),
            direction: "pull".to_string(),
            domains: vec!["config".to_string()],
            schedule: "3600".to_string(),
            enabled: true,
            peer_secret_hash: "aabbcc".to_string(),
            peer_secret_fingerprint: None,
            last_run_at: Some(at),
            last_success_at: Some(at),
            created_at: at,
            updated_at: at,
        }
    }

    fn update_request() -> UpdatePlanRequest {
        UpdatePlanRequest {
            direction: None,
            domains: None,
            schedule: None,
            enabled: None,
            peer_secret: None,
        }
    }

    #[test]
    fn plan_update_clears_cursors_when_direction_changes() {
        let state = Arc::new(ClustersState::new());
        let mut plan = stored_plan();
        let mut body = update_request();
        body.direction = Some("bidirectional".to_string());
        apply_plan_update(&state, &mut plan, &body).expect("direction update applies");
        assert_eq!(plan.direction, "bidirectional");
        assert!(plan.last_success_at.is_none());
        assert!(plan.last_run_at.is_none());
    }

    #[test]
    fn plan_update_clears_cursors_when_domains_change() {
        let state = Arc::new(ClustersState::new());
        let mut plan = stored_plan();
        let mut body = update_request();
        body.domains = Some(vec!["config".to_string(), "modules".to_string()]);
        apply_plan_update(&state, &mut plan, &body).expect("domains update applies");
        assert_eq!(plan.domains, vec!["config".to_string(), "modules".to_string()]);
        assert!(plan.last_success_at.is_none());
        assert!(plan.last_run_at.is_none());
    }

    #[test]
    fn plan_update_keeps_cursors_when_shape_is_unchanged() {
        let state = Arc::new(ClustersState::new());
        let mut plan = stored_plan();

        // Same direction and domains: cursors survive.
        let mut body = update_request();
        body.direction = Some("pull".to_string());
        body.domains = Some(vec!["config".to_string()]);
        apply_plan_update(&state, &mut plan, &body).expect("no-op update applies");
        assert!(plan.last_success_at.is_some());
        assert!(plan.last_run_at.is_some());

        // Schedule-only and enable-only changes also keep cursors.
        let mut body = update_request();
        body.schedule = Some("60".to_string());
        body.enabled = Some(false);
        apply_plan_update(&state, &mut plan, &body).expect("schedule update applies");
        assert_eq!(plan.schedule, "60");
        assert!(!plan.enabled);
        assert!(plan.last_success_at.is_some());
        assert!(plan.last_run_at.is_some());
    }

    #[test]
    fn plan_update_rejects_bad_direction_and_schedule() {
        let state = Arc::new(ClustersState::new());
        let mut plan = stored_plan();
        let mut body = update_request();
        body.direction = Some("sideways".to_string());
        assert!(apply_plan_update(&state, &mut plan, &body).is_err());
        let mut body = update_request();
        body.schedule = Some("0".to_string());
        assert!(apply_plan_update(&state, &mut plan, &body).is_err());
    }
}
