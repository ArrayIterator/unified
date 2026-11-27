// load_balancer -- Balancer handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json as json_value};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering as AtomicOrdering;
use std::time::Instant;
use uuid::Uuid;

use super::constants::*;
use super::health::*;
use super::hooks::*;
use super::requests::*;
use super::selection::*;
use super::store::*;
use super::tls::*;
use super::types::*;
use super::validation::*;
use super::views::*;

// Balancer handlers.

pub(crate) async fn list_balancers(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_READ)?;
        let pagination = query.normalized();
        let mut items: Vec<BalancerView> = balancers_store()
            .iter()
            .map(|entry| balancer_view(&entry))
            .filter(|view| {
                matches_search(&view.name, &pagination.search)
                    && pagination
                        .filters
                        .get("status")
                        .is_none_or(|value| format!("{:?}", view.status).to_lowercase() == *value)
                    && pagination.filters.get("strategy").is_none_or(|value| {
                        format!("{:?}", view.strategy).to_lowercase() == *value
                            || serde_json::to_string(&view.strategy)
                                .unwrap_or_default()
                                .trim_matches('"')
                                == value
                    })
                    && pagination.filters.get("tls_mode").is_none_or(|value| {
                        serde_json::to_string(&view.tls_mode)
                            .unwrap_or_default()
                            .trim_matches('"')
                            == value
                    })
                    && pagination
                        .filters
                        .get("enabled")
                        .is_none_or(|value| view.enabled == (value == "true" || value == "1"))
            })
            .collect();
        let page = apply_list_params(
            std::mem::take(&mut items),
            &query,
            &["name", "listen_port", "created_at"],
            "name",
            |view, sort| match sort {
                "listen_port" => format!("{:05}", view.listen_port),
                "created_at" => view.created_at.to_rfc3339(),
                _ => view.name.clone(),
            },
        );
        Ok(page.to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_balancer(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateBalancerRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BalancerView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_MANAGE)?;
        let config = ctx.config.read().expect("config lock poisoned").clone();
        let name = validate_name(&body.name, "Balancer name")?;
        validate_host_port(&body.listen_address, "Listen address")?;
        if body.listen_port == 0 {
            return Err(UnifiedError::bad_request(
                "Listen port must be between 1 and 65535",
            ));
        }
        let domain = match &body.domain {
            Some(domain) => Some(validate_domain(domain)?),
            None => None,
        };
        let health_check = match &body.health_check {
            Some(input) => {
                let merged = input.merged(&HealthCheckConfig::default());
                validate_health_check(&merged)?;
                merged
            }
            None => HealthCheckConfig::default(),
        };
        validate_tls_reference(&config, body.tls_mode, &domain)?;
        if balancers_store().iter().any(|entry| {
            entry.listen_address == body.listen_address && entry.listen_port == body.listen_port
        }) {
            return Err(UnifiedError::conflict(format!(
                "Another load balancer already listens on {}:{}",
                body.listen_address, body.listen_port
            )));
        }
        let now = Utc::now();
        let balancer = LoadBalancer {
            id: Uuid::new_v4(),
            name,
            listen_address: body.listen_address.trim().to_string(),
            listen_port: body.listen_port,
            tls_mode: body.tls_mode,
            domain: domain.clone(),
            certificate_name: body
                .certificate_name
                .as_ref()
                .map(|value| value.trim().to_string()),
            strategy: body.strategy,
            health_check,
            enabled: body.enabled,
            status: if body.enabled {
                BalancerStatus::Degraded
            } else {
                BalancerStatus::Disabled
            },
            created_at: now,
            updated_at: now,
        };
        let view = balancer_view(&balancer);
        balancers_store().insert(balancer.id, balancer.clone());
        emit_ctx(
            &ctx,
            LB_CREATED,
            json_value!({
                "balancer_id": balancer.id,
                "balancer_name": balancer.name,
                "created_by": caller.actor,
                "listen_address": balancer.listen_address,
                "listen_port": balancer.listen_port,
                "tls_mode": balancer.tls_mode,
                "strategy": balancer.strategy,
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_balancer_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BalancerView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_READ)?;
        let id = parse_id(&id, "load balancer")?;
        Ok(balancer_view(&get_balancer(id)?))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_balancer(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<UpdateBalancerRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BalancerView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_MANAGE)?;
        let id = parse_id(&id, "load balancer")?;
        let config = ctx.config.read().expect("config lock poisoned").clone();
        let mut balancer = get_balancer(id)?;
        let old_strategy = balancer.strategy;
        let old_tls_mode = balancer.tls_mode;
        let old_domain = balancer.domain.clone();
        if let Some(name) = &body.name {
            balancer.name = validate_name(name, "Balancer name")?;
        }
        if let Some(address) = &body.listen_address {
            validate_host_port(address, "Listen address")?;
            balancer.listen_address = address.trim().to_string();
        }
        if let Some(port) = body.listen_port {
            if port == 0 {
                return Err(UnifiedError::bad_request(
                    "Listen port must be between 1 and 65535",
                ));
            }
            balancer.listen_port = port;
        }
        if let Some(tls_mode) = body.tls_mode {
            balancer.tls_mode = tls_mode;
        }
        if let Some(domain) = &body.domain {
            balancer.domain = Some(validate_domain(domain)?);
        }
        if let Some(certificate_name) = &body.certificate_name {
            balancer.certificate_name = Some(certificate_name.trim().to_string());
        }
        if let Some(strategy) = body.strategy {
            balancer.strategy = strategy;
        }
        if let Some(input) = &body.health_check {
            let merged = input.merged(&balancer.health_check);
            validate_health_check(&merged)?;
            balancer.health_check = merged;
        }
        if let Some(enabled) = body.enabled {
            balancer.enabled = enabled;
        }
        validate_tls_reference(&config, balancer.tls_mode, &balancer.domain)?;
        if balancers_store().iter().any(|entry| {
            entry.id != id
                && entry.listen_address == balancer.listen_address
                && entry.listen_port == balancer.listen_port
        }) {
            return Err(UnifiedError::conflict(format!(
                "Another load balancer already listens on {}:{}",
                balancer.listen_address, balancer.listen_port
            )));
        }
        balancer.updated_at = Utc::now();
        balancer.status = compute_effective_status(&balancer);
        let view = balancer_view(&balancer);
        let (strategy_changed, tls_changed) = (
            balancer.strategy != old_strategy,
            balancer.tls_mode != old_tls_mode || balancer.domain != old_domain,
        );
        balancers_store().insert(id, balancer.clone());
        emit_ctx(
            &ctx,
            LB_UPDATED,
            json_value!({
                "balancer_id": balancer.id,
                "balancer_name": balancer.name,
                "strategy_changed": strategy_changed,
                "tls_changed": tls_changed,
            }),
        );
        if strategy_changed {
            emit_ctx(
                &ctx,
                LB_STRATEGY_CHANGED,
                json_value!({
                    "balancer_id": balancer.id,
                    "balancer_name": balancer.name,
                    "old_strategy": old_strategy,
                    "strategy": balancer.strategy,
                }),
            );
        }
        Ok(view)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_balancer(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_MANAGE)?;
        let id = parse_id(&id, "load balancer")?;
        let balancer = get_balancer(id)?;
        let backend_ids: Vec<Uuid> = backends_store()
            .iter()
            .filter(|entry| entry.balancer_id == id)
            .map(|entry| entry.id)
            .collect();
        for backend_id in backend_ids {
            backends_store().remove(&backend_id);
            stats_store().remove(&backend_id);
            connections_store().remove(&backend_id);
        }
        round_robin_store().remove(&id);
        balancers_store().remove(&id);
        emit_ctx(
            &ctx,
            LB_UPDATED,
            json_value!({
                "balancer_id": id,
                "balancer_name": balancer.name,
                "deleted": true,
            }),
        );
        Ok(json_value!({ "id": id, "deleted": true }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn set_balancer_enabled(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    enabled: bool,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BalancerView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_MANAGE)?;
        let id = parse_id(&id, "load balancer")?;
        let mut balancer = get_balancer(id)?;
        balancer.enabled = enabled;
        balancer.updated_at = Utc::now();
        balancer.status = compute_effective_status(&balancer);
        let view = balancer_view(&balancer);
        balancers_store().insert(id, balancer.clone());
        emit_ctx(
            &ctx,
            LB_UPDATED,
            json_value!({
                "balancer_id": balancer.id,
                "balancer_name": balancer.name,
                "enabled": enabled,
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

pub(crate) async fn enable_balancer(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    set_balancer_enabled(ctx, req, id, true).await
}

pub(crate) async fn disable_balancer(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    set_balancer_enabled(ctx, req, id, false).await
}

pub(crate) async fn get_tls_status(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_READ)?;
        let id = parse_id(&id, "load balancer")?;
        let balancer = get_balancer(id)?;
        let config = ctx.config.read().expect("config lock poisoned").clone();
        let certificate_name = balancer
            .certificate_name
            .clone()
            .or_else(|| balancer.domain.clone());
        // Resolve through the one shared storage the HTTP server
        // attached to the context (the same storage the HTTPS listener
        // and the gateway serve from), so renewals and swaps are
        // visible here without a restart. When no handle is attached
        // yet (no TLS listener, or a non-server context such as tests)
        // fall back to the config-only status: the endpoint never
        // fails or panics over a missing handle.
        let storage = ctx.tls();
        let storage_attached = storage.is_some();
        let (terminated, info) = match (balancer.tls_mode, certificate_name) {
            (TlsMode::Terminate, Some(name)) => (
                true,
                Some(match storage.as_ref() {
                    Some(storage) => tls_certificate_status(storage, &name),
                    None => tls_status_from_config(&name, &config),
                }),
            ),
            _ => (false, None),
        };
        Ok(json_value!({
            "balancer_id": balancer.id,
            "tls_mode": balancer.tls_mode,
            "domain": balancer.domain,
            "certificate_name": balancer.certificate_name,
            "terminated": terminated,
            "certificate": info,
            "storage_attached": storage_attached,
            "note": if storage_attached {
                "Live SslStorage lookup through the shared storage attached by http::server::run_server."
            } else {
                "No shared SslStorage handle is attached to the context (no TLS listener, or a non-server context); this endpoint reports configured certificates (app.ssl.domains) plus the cache refreshed by ssl.swap / ssl.renew events."
            },
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn preview_selection(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<PreviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_READ)?;
        let id = parse_id(&id, "load balancer")?;
        let balancer = get_balancer(id)?;
        let client_ip = body
            .client_ip
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if let Some(ip) = client_ip {
            ip.parse::<std::net::IpAddr>().map_err(|_| {
                UnifiedError::bad_request(format!("Invalid client IP address \"{ip}\""))
            })?;
        }
        let eligible = eligible_backends(id);
        let connections: HashMap<Uuid, u64> = eligible
            .iter()
            .map(|backend| (backend.id, active_connections(backend.id)))
            .collect();
        let counter = round_robin_store()
            .get(&id)
            .map(|entry| entry.load(AtomicOrdering::SeqCst))
            .unwrap_or(0);
        let selected = select_backend(
            &eligible,
            balancer.strategy,
            client_ip,
            counter,
            &connections,
        )
        .and_then(|backend_id| eligible.iter().find(|backend| backend.id == backend_id));
        Ok(json_value!({
            "balancer_id": balancer.id,
            "strategy": balancer.strategy,
            "status": balancer.status,
            "degraded": balancer.status == BalancerStatus::Degraded,
            "client_ip": client_ip,
            "eligible_backends": eligible.len(),
            "backend": selected.map(backend_view),
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn route_request(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    query: web::Query<RouteQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_MANAGE)?;
        let id = parse_id(&id, "load balancer")?;
        let balancer = get_balancer(id)?;
        let client_ip = query
            .client_ip
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if let Some(ip) = client_ip {
            ip.parse::<std::net::IpAddr>().map_err(|_| {
                UnifiedError::bad_request(format!("Invalid client IP address \"{ip}\""))
            })?;
        }
        if !balancer.enabled {
            return Err(UnifiedError::new(
                format!("Load balancer \"{}\" is disabled", balancer.name),
                503,
            )
            .with_metadata("balancer_id", json_value!(balancer.id))
            .with_metadata("reason", json_value!("balancer_disabled")));
        }
        let started = Instant::now();
        let eligible = eligible_backends(id);
        if eligible.is_empty() {
            // `refresh_balancer_status` already emits `lb.failover`
            // on the transition into Degraded; emitting here as well
            // would double-report the incident (and spam one event
            // per failed request while already degraded).
            refresh_balancer_status(id);
            return Err(UnifiedError::new(
                format!(
                    "Load balancer \"{}\" has no healthy backend available",
                    balancer.name
                ),
                503,
            )
            .with_metadata("balancer_id", json_value!(balancer.id))
            .with_metadata("reason", json_value!("all_backends_unhealthy")));
        }
        let connections: HashMap<Uuid, u64> = eligible
            .iter()
            .map(|backend| (backend.id, active_connections(backend.id)))
            .collect();
        let counter = next_round_robin(id);
        let backend_id = select_backend(
            &eligible,
            balancer.strategy,
            client_ip,
            counter,
            &connections,
        )
        .ok_or_else(|| UnifiedError::internal("Backend selection returned no backend"))?;
        let backend = eligible
            .into_iter()
            .find(|candidate| candidate.id == backend_id)
            .expect("selected backend is in the eligible list");
        let latency_ms = started.elapsed().as_millis() as u64;
        // Simulated routing only: count the routed request in the
        // cumulative stats, but book no active connection. A
        // simulated request has no close event, so booking one here
        // would make `active_connections` a growing routed total
        // instead of live load. Real connections are opened and
        // closed by the proxy path only.
        mutate_backend_stats(backend.id, |stats| {
            stats.requests_routed = stats.requests_routed.saturating_add(1);
            stats.total_latency_ms = stats.total_latency_ms.saturating_add(latency_ms);
            stats.latency_samples = stats.latency_samples.saturating_add(1);
        });
        Ok(json_value!({
            "balancer_id": balancer.id,
            "balancer_name": balancer.name,
            "strategy": balancer.strategy,
            "client_ip": client_ip,
            "backend": backend_view(&backend),
            "selection_latency_ms": latency_ms,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn check_now_balancer(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = parse_id(&id, "load balancer");
    let result: UnifiedResult<Value> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_MANAGE)?;
        let id = id?;
        let balancer = get_balancer(id)?;
        let backends = all_backends(id);
        let check = balancer.health_check.clone();
        let probes = futures_util::future::join_all(
            backends
                .iter()
                .map(|backend| probe_backend(backend, &check)),
        )
        .await;
        let mut results = Vec::new();
        for (backend, outcome) in backends.iter().zip(probes.into_iter()) {
            let healthy = outcome.healthy;
            let latency_ms = outcome.latency_ms;
            apply_probe_result(backend.id, outcome);
            results.push(json_value!({
                "backend_id": backend.id,
                "backend_name": backend.name,
                "address": backend.address,
                "healthy": healthy,
                "latency_ms": latency_ms,
                "health": backends_store().get(&backend.id).map(|entry| entry.health),
            }));
        }
        refresh_balancer_status(id);
        Ok(json_value!({
            "balancer_id": id,
            "checked": results.len(),
            "results": results,
        }))
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn balancer_stats(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BalancerStatsView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_READ)?;
        let id = parse_id(&id, "load balancer")?;
        let balancer = get_balancer(id)?;
        let backends = all_backends(id);
        let views: Vec<BackendStatsView> = backends.iter().map(backend_stats_view).collect();
        let requests_routed: u64 = views.iter().map(|view| view.requests_routed).sum();
        let errors: u64 = views.iter().map(|view| view.errors).sum();
        let (latency_total, latency_samples): (u64, u64) =
            backends.iter().fold((0, 0), |(total, samples), backend| {
                let stats = backend_stats(backend.id);
                (
                    total.saturating_add(stats.total_latency_ms),
                    samples.saturating_add(stats.latency_samples),
                )
            });
        let (checks_total, checks_ok): (u64, u64) =
            backends.iter().fold((0, 0), |(total, ok), backend| {
                let stats = backend_stats(backend.id);
                (
                    total.saturating_add(stats.checks_total),
                    ok.saturating_add(stats.checks_ok),
                )
            });
        Ok(BalancerStatsView {
            balancer_id: balancer.id,
            balancer_name: balancer.name.clone(),
            status: balancer.status,
            total_backends: backends.len(),
            healthy_backends: backends
                .iter()
                .filter(|backend| backend.health == HealthState::Healthy)
                .count(),
            requests_routed,
            errors,
            average_latency_ms: if latency_samples == 0 {
                None
            } else {
                Some(latency_total as f64 / latency_samples as f64)
            },
            uptime_ratio: if checks_total == 0 {
                None
            } else {
                Some(checks_ok as f64 / checks_total as f64)
            },
            backends: views,
        })
    })();
    json::respond(result, debug)
}

pub(crate) async fn reset_balancer_stats(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LB_MANAGE)?;
        let id = parse_id(&id, "load balancer")?;
        let balancer = get_balancer(id)?;
        for backend in all_backends(id) {
            stats_store().remove(&backend.id);
        }
        Ok(json_value!({ "balancer_id": balancer.id, "reset": true }))
    })();
    json::respond(result, debug)
}
