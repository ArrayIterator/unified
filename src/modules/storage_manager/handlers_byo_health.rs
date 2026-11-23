// BYO health check handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// BYO health checks.
// ---------------------------------------------------------------------------

pub(crate) async fn health_byo_connection(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_HEALTH) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let connection = match load_byo_connection_for(&state, &id, &found.actor, is_admin) {
        Ok(connection) => connection,
        Err(error) => return err_response(error, debug),
    };
    if connection.status == "revoked" {
        return err_response(
            UnifiedError::conflict("A revoked BYO connection cannot be health-checked"),
            debug,
        );
    }
    let result: UnifiedResult<ByoHealthCheck> = (|| async {
        run_byo_health_check(&state, &ctx.config, &connection).await
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn list_byo_health(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_HEALTH) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let params = params.into_inner();
    let mut items: Vec<ByoHealthCheck> = state
        .byo_health_checks
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|check| {
            if is_admin {
                return true;
            }
            state
                .byo_connections
                .get(&check.connection_id)
                .is_some_and(|entry| entry.value().owner_user == found.actor)
        })
        .collect();
    if let Some(filter) = params.filters.get("connection_id") {
        items.retain(|check| check.connection_id.to_string() == *filter);
    }
    if let Some(filter) = params.filters.get("ok") {
        let want = filter == "true";
        items.retain(|check| check.ok == want);
    }
    items.sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
    ok(paginate(items, &params))
}

pub(crate) async fn list_byo_connection_health(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_HEALTH) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let connection = match load_byo_connection_for(&state, &id, &found.actor, is_admin) {
        Ok(connection) => connection,
        Err(error) => return err_response(error, debug),
    };
    let params = params.into_inner();
    let mut items: Vec<ByoHealthCheck> = state
        .byo_health_checks
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|check| check.connection_id == connection.id)
        .collect();
    items.sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
    ok(paginate(items, &params))
}

/// Run BYO health checks now for every active connection (or one
/// connection when `connection_id` is given), synchronously, and
/// optionally ask the scheduler to enqueue a
/// `storage.byo.health_check` job. The scheduler declines it until
/// the kind is registered (see the module header); the run still
/// executes in-process.
pub(crate) async fn run_byo_health(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<RunByoHealthRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_HEALTH) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = check_capability(&ctx, &found.role, PERM_MANAGE_BYO);
    let input = body.into_inner();
    // Tenant scoping for the enqueue: a single-connection enqueue
    // is checked against the owner-or-admin loader BEFORE the
    // scheduler is asked, and an all-connections enqueue is refused
    // for non-admins (the scheduler payload carries no owner, so
    // the scheduled run could not be scoped later).
    if input.enqueue.unwrap_or(false) {
        match input.connection_id {
            Some(id) => {
                if let Err(error) =
                    load_byo_connection_for(&state, &id.to_string(), &found.actor, is_admin)
                {
                    return err_response(error, debug);
                }
            }
            None if !is_admin => {
                return err_response(
                    UnifiedError::forbidden(
                        "Only a BYO admin may enqueue a health check for all connections",
                    ),
                    debug,
                );
            }
            None => {}
        }
    }
    let mut queued = false;
    let mut queue_error: Option<String> = None;
    if input.enqueue.unwrap_or(false) {
        let answer = ctx.hooks.emit(
            events::SCHEDULER_ENQUEUE,
            json!({
                "kind": "storage.byo.health_check",
                "payload": {
                    "connection_id": input.connection_id.map(|id| id.to_string()),
                    "requested_by": found.actor,
                },
            }),
        );
        let first = answer.results.into_iter().next().unwrap_or(Value::Null);
        queued = first.get("queued").and_then(Value::as_bool).unwrap_or(false);
        if !queued {
            queue_error = first
                .get("error")
                .and_then(|value| value.as_str())
                .map(str::to_string);
        }
    }
    let result: UnifiedResult<Value> = (|| async {
        let connections: Vec<ByoConnection> = match input.connection_id {
            Some(id) => vec![load_byo_connection_for(
                &state,
                &id.to_string(),
                &found.actor,
                is_admin,
            )?],
            None => state
                .byo_connections
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|connection| {
                    connection.status == "active"
                        && (is_admin || connection.owner_user == found.actor)
                })
                .collect(),
        };
        let mut results: Vec<Value> = Vec::new();
        for connection in &connections {
            match run_byo_health_check(&state, &ctx.config, connection).await {
                Ok(check) => results.push(json!({
                    "connection_id": connection.id,
                    "ok": true,
                    "latency_ms": check.latency_ms,
                    "checked_at": check.checked_at.to_rfc3339(),
                })),
                Err(error) => results.push(json!({
                    "connection_id": connection.id,
                    "ok": false,
                    "error": error.message,
                })),
            }
        }
        Ok(json!({
            "connections_checked": results.len(),
            "results": results,
            "scheduler_queued": queued,
            "scheduler_error": queue_error,
        }))
    })()
    .await;
    json::respond(result, debug)
}

/// Answer `storage.byo.health_check_requested`: the event the
/// scheduler emits when a `storage.byo.health_check` job fires.
/// Hook handlers are synchronous, so the checks spawn on the async
/// runtime and the handler answers immediately with
/// `{"accepted": true, "queued": true}`. When `init` never ran (no
/// config handle), the handler answers `accepted: false` instead of
/// pretending the checks happened.
pub(crate) fn on_byo_health_check_requested(state: StorageManagerState, ctx: &mut HookContext) {
    let connection_id = ctx
        .data
        .get("connection_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let connections: Vec<ByoConnection> = match connection_id {
        Some(id) => match state.byo_connections.get(&id) {
            Some(entry) => vec![entry.value().clone()],
            None => {
                ctx.add_result(json!({
                    "accepted": false,
                    "error": format!("BYO connection not found: {id}"),
                }));
                return;
            }
        },
        None => state
            .byo_connections
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|connection| connection.status == "active")
            .collect(),
    };
    let Some(config) = state
        .app_config
        .read()
        .expect("app config lock poisoned")
        .clone()
    else {
        ctx.add_result(json!({
            "accepted": false,
            "connections_matched": connections.len(),
            "error": "Storage Manager init has not run, so no config handle is available to build providers",
        }));
        return;
    };
    let run_state = state.clone();
    tokio::spawn(async move {
        for connection in &connections {
            let _ = run_byo_health_check(&run_state, &config, connection).await;
        }
    });
    ctx.add_result(json!({
        "accepted": true,
        "queued": true,
        "connections_matched": connections.len(),
    }));
}
