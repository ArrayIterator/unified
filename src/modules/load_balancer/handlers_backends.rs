// load_balancer -- Backend handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json as json_value};
use std::sync::Arc;
use uuid::Uuid;

use super::constants::*;
use super::health::*;
use super::hooks::*;
use super::requests::*;
use super::selection::*;
use super::store::*;
use super::types::*;
use super::validation::*;
use super::views::*;

// Backend handlers.

pub(crate) async fn list_backends(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_READ)?;
        let id = parse_id(&id, "load balancer")?;
        let _balancer = get_balancer(id)?;
        let pagination = query.normalized();
        let mut items: Vec<BackendView> = all_backends(id)
            .iter()
            .map(backend_view)
            .filter(|view| {
                matches_search(&view.name, &pagination.search)
                    && pagination.filters.get("health").is_none_or(|value| {
                        serde_json::to_string(&view.health)
                            .unwrap_or_default()
                            .trim_matches('"')
                            == value
                    })
                    && pagination.filters.get("source_kind").is_none_or(|value| {
                        serde_json::to_string(&view.source_kind)
                            .unwrap_or_default()
                            .trim_matches('"')
                            == value
                    })
                    && pagination
                        .filters
                        .get("enabled")
                        .is_none_or(|value| view.enabled == (value == "true" || value == "1"))
                    && pagination
                        .filters
                        .get("draining")
                        .is_none_or(|value| view.draining == (value == "true" || value == "1"))
            })
            .collect();
        let page = apply_list_params(
            std::mem::take(&mut items),
            &query,
            &["name", "address", "created_at"],
            "name",
            |view, sort| match sort {
                "address" => view.address.clone(),
                "created_at" => view.created_at.to_rfc3339(),
                _ => view.name.clone(),
            },
        );
        Ok(page.to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_backend(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<CreateBackendRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackendView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_MANAGE)?;
        let id = parse_id(&id, "load balancer")?;
        let balancer = get_balancer(id)?;
        let name = validate_name(&body.name, "Backend name")?;
        validate_host_port(&body.address, "Backend address")?;
        let weight = validate_weight(body.weight)?;
        let source_id = match body.source_kind {
            BackendSourceKind::Address => {
                if body
                    .source_id
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
                {
                    return Err(UnifiedError::bad_request(
                        "source_id must be empty for plain address backends",
                    ));
                }
                None
            }
            BackendSourceKind::Provider | BackendSourceKind::Cluster => {
                let raw = body.source_id.as_deref().unwrap_or("").trim();
                if raw.is_empty() {
                    return Err(UnifiedError::bad_request(
                        "source_id is required for provider and cluster backends (format \"<owner_id>/<record_id>\")",
                    ));
                }
                if raw.split('/').count() != 2 || raw.split('/').any(|part| part.trim().is_empty())
                {
                    return Err(UnifiedError::bad_request(
                        "source_id must have the format \"<owner_id>/<record_id>\"",
                    ));
                }
                Some(raw.to_string())
            }
        };
        let now = Utc::now();
        let backend = Backend {
            id: Uuid::new_v4(),
            balancer_id: id,
            name,
            source_kind: body.source_kind,
            source_id,
            address: body.address.trim().to_string(),
            weight,
            enabled: body.enabled,
            draining: false,
            health: HealthState::Unknown,
            last_check_at: None,
            last_latency_ms: None,
            consecutive_failures: 0,
            consecutive_successes: 0,
            created_at: now,
            updated_at: now,
        };
        let view = backend_view(&backend);
        backends_store().insert(backend.id, backend.clone());
        refresh_balancer_status(id);
        emit_ctx(
            &ctx,
            LB_BACKEND_ADDED,
            json_value!({
                "balancer_id": id,
                "balancer_name": balancer.name,
                "backend_id": backend.id,
                "backend_name": backend.name,
                "created_by": caller.actor,
                "source_kind": backend.source_kind,
                "source_id": backend.source_id,
                "address": backend.address,
                "weight": backend.weight,
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_backend_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackendView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_READ)?;
        let (balancer_id, backend_id) = path.into_inner();
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        Ok(backend_view(&get_backend(balancer_id, backend_id)?))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_backend(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateBackendRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackendView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_MANAGE)?;
        let (balancer_id, backend_id) = path.into_inner();
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        let mut backend = get_backend(balancer_id, backend_id)?;
        if let Some(name) = &body.name {
            backend.name = validate_name(name, "Backend name")?;
        }
        if let Some(address) = &body.address {
            validate_host_port(address, "Backend address")?;
            backend.address = address.trim().to_string();
        }
        if let Some(weight) = body.weight {
            backend.weight = validate_weight(weight)?;
        }
        if let Some(enabled) = body.enabled {
            backend.enabled = enabled;
        }
        if let Some(draining) = body.draining {
            backend.draining = draining;
        }
        if let Some(source_kind) = body.source_kind {
            backend.source_kind = source_kind;
        }
        if let Some(source_id) = &body.source_id {
            backend.source_id = if source_id.trim().is_empty() {
                None
            } else {
                Some(source_id.trim().to_string())
            };
        }
        if backend.source_kind == BackendSourceKind::Address && backend.source_id.is_some() {
            return Err(UnifiedError::bad_request(
                "source_id must be empty for plain address backends",
            ));
        }
        if backend.source_kind != BackendSourceKind::Address
            && backend.source_id.as_deref().unwrap_or("").is_empty()
        {
            return Err(UnifiedError::bad_request(
                "source_id is required for provider and cluster backends",
            ));
        }
        backend.updated_at = Utc::now();
        let view = backend_view(&backend);
        backends_store().insert(backend_id, backend.clone());
        refresh_balancer_status(balancer_id);
        emit_ctx(
            &ctx,
            LB_UPDATED,
            json_value!({
                "balancer_id": balancer_id,
                "backend_id": backend.id,
                "backend_name": backend.name,
                "draining": backend.draining,
                "enabled": backend.enabled,
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_backend(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_MANAGE)?;
        let (balancer_id, backend_id) = path.into_inner();
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        let backend = get_backend(balancer_id, backend_id)?;
        backends_store().remove(&backend_id);
        stats_store().remove(&backend_id);
        connections_store().remove(&backend_id);
        refresh_balancer_status(balancer_id);
        emit_ctx(
            &ctx,
            LB_BACKEND_REMOVED,
            json_value!({
                "balancer_id": balancer_id,
                "backend_id": backend.id,
                "backend_name": backend.name,
            }),
        );
        Ok(json_value!({ "id": backend_id, "deleted": true }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn set_backend_enabled(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    enabled: bool,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackendView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_MANAGE)?;
        let (balancer_id, backend_id) = path.into_inner();
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        let mut backend = get_backend(balancer_id, backend_id)?;
        backend.enabled = enabled;
        backend.updated_at = Utc::now();
        let view = backend_view(&backend);
        backends_store().insert(backend_id, backend.clone());
        refresh_balancer_status(balancer_id);
        Ok(view)
    })();
    json::respond(result, debug)
}

pub(crate) async fn enable_backend(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    set_backend_enabled(ctx, req, path, true).await
}

pub(crate) async fn disable_backend(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    set_backend_enabled(ctx, req, path, false).await
}

pub(crate) async fn set_backend_draining(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    draining: bool,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackendView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_MANAGE)?;
        let (balancer_id, backend_id) = path.into_inner();
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        let mut backend = get_backend(balancer_id, backend_id)?;
        backend.draining = draining;
        backend.updated_at = Utc::now();
        let view = backend_view(&backend);
        backends_store().insert(backend_id, backend.clone());
        refresh_balancer_status(balancer_id);
        emit_ctx(
            &ctx,
            LB_UPDATED,
            json_value!({
                "balancer_id": balancer_id,
                "backend_id": backend.id,
                "backend_name": backend.name,
                "draining": draining,
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

pub(crate) async fn drain_backend(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    set_backend_draining(ctx, req, path, true).await
}

pub(crate) async fn undrain_backend(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    set_backend_draining(ctx, req, path, false).await
}

pub(crate) async fn check_now_backend(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_MANAGE)?;
        let (balancer_id, backend_id) = path.into_inner();
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        let balancer = get_balancer(balancer_id)?;
        let backend = get_backend(balancer_id, backend_id)?;
        let outcome = probe_backend(&backend, &balancer.health_check).await;
        let healthy = outcome.healthy;
        let latency_ms = outcome.latency_ms;
        apply_probe_result(backend_id, outcome);
        let updated = get_backend(balancer_id, backend_id)?;
        Ok(json_value!({
            "balancer_id": balancer_id,
            "backend_id": backend_id,
            "healthy": healthy,
            "latency_ms": latency_ms,
            "health": updated.health,
            "last_check_at": updated.last_check_at,
        }))
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn backend_stats_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackendStatsView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_READ)?;
        let (balancer_id, backend_id) = path.into_inner();
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        Ok(backend_stats_view(&get_backend(balancer_id, backend_id)?))
    })();
    json::respond(result, debug)
}

pub(crate) async fn reset_backend_stats(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_MANAGE)?;
        let (balancer_id, backend_id) = path.into_inner();
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        let backend = get_backend(balancer_id, backend_id)?;
        stats_store().remove(&backend_id);
        Ok(json_value!({ "backend_id": backend.id, "reset": true }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn release_connection(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_MANAGE)?;
        let (balancer_id, backend_id) = path.into_inner();
        let balancer_id = parse_id(&balancer_id, "load balancer")?;
        let backend_id = parse_id(&backend_id, "backend")?;
        let backend = get_backend(balancer_id, backend_id)?;
        note_backend_connection_closed(backend_id);
        Ok(json_value!({
            "backend_id": backend.id,
            "active_connections": active_connections(backend_id),
        }))
    })();
    json::respond(result, debug)
}
