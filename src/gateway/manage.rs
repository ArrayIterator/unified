// Management module for the Pingora gateway (id `"gateway"`).
//
// HTTP routes under `/api/v1/gateway` (see `route_info` for the full
// catalog). Every handler uses the standard JSON envelope
// (`{"data": ...}` on success, `{"error": {...}}` on failure, debug
// only trace) and every list endpoint uses the shared pagination
// parameters (`page`, `per_page`, `sort`, `order`, `search`, plus
// exact-match filters).
//
// Lifecycle:
//   - `init` stores the hook bus and application configuration on the
//     process-wide gateway state and starts the health sweeper. It
//     starts the proxy listener automatically only when the gateway
//     configuration is enabled; a failed auto-start is logged, never
//     fatal to application startup.
//   - `POST /start`, `POST /stop`, `POST /reload` control the service
//     at runtime (see `proxy::GatewayService` for stop semantics).
//
// Hooks observed: `lb.backend.unhealthy` / `lb.backend.healthy`
// (gateway health exclusions follow the load balancer),
// `ssl.swap` / `ssl.renew` (TLS status is computed live from the
// storage, so these only re-emit a gateway TLS event).
//
// Hooks emitted: `gateway.started`, `gateway.stopped`,
// `gateway.reloaded`, `gateway.upstream.unhealthy`,
// `gateway.upstream.healthy`, `gateway.tls.changed`,
// `gateway.rule.created/updated/deleted`.
//
// Registration is conditional on the `gateway` Cargo feature: see
// `register` below and the snippet for `src/modules/mod.rs` in the
// coordinator report. The module struct itself compiles without the
// feature; its control handlers then fail with the "gateway feature
// not enabled" error instead of starting anything.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{CliCommandDef, CliRegistry, Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::gateway::health::{
    backend_health_key, check_now as health_check_now, mark_healthy, mark_unhealthy,
    start_health_sweeper,
};
use crate::gateway::routing::{
    create_rule, delete_rule, get_rule, list_rules, update_rule, CreateRuleRequest,
    UpdateRuleRequest,
};
use crate::gateway::{
    certificate_status, feature_enabled, require_feature_enabled, GatewayState,
    GATEWAY_TLS_CHANGED,
};
use crate::modules::load_balancer::{eligible_backends, LB_BACKEND_HEALTHY, LB_BACKEND_UNHEALTHY};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use clap::{ArgMatches, Command};
use serde::Serialize;
use serde_json::{json as json_value, Value};
use std::sync::Arc;
use uuid::Uuid;

pub const GATEWAY_RULE_CREATED: &str = "gateway.rule.created";
pub const GATEWAY_RULE_UPDATED: &str = "gateway.rule.updated";
pub const GATEWAY_RULE_DELETED: &str = "gateway.rule.deleted";

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct GatewayModule;

impl GatewayModule {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GatewayModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for GatewayModule {
    fn id(&self) -> &'static str {
        "gateway"
    }

    fn name(&self) -> &'static str {
        "Pingora Gateway"
    }

    fn description(&self) -> &'static str {
        "Reverse proxy / gateway in front of the application: Pingora request routing by host, path, and headers, upstream selection through the load balancer, TLS termination from the shared certificate storage, upstream health checks, and a proxied-request log."
    }

    fn requires(&self) -> &[&'static str] {
        &["load_balancer", "security"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["pingora"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let state = GatewayState::global();
        state.set_hooks(ctx.hooks.clone());
        state.set_app_config(ctx.config.clone());
        start_health_sweeper(state.clone());
        // Auto-start only when the operator enabled the gateway.
        // The default configuration is disabled, so a fresh install
        // never binds a listener by surprise. A failed auto-start is
        // logged, not fatal: the management API can start the
        // gateway later, and a failed listener must not take the
        // whole application down with it.
        if state.config_snapshot().enabled {
            #[cfg(feature = "gateway")]
            {
                match crate::gateway::proxy::GatewayService::start(state.clone()) {
                    Ok(handle) => {
                        let mut service = state
                            .service
                            .lock()
                            .expect("gateway service lock poisoned");
                        *service = Some(handle);
                    }
                    Err(error) => {
                        log::error!("Gateway auto-start failed: {error}");
                    }
                }
            }
            #[cfg(not(feature = "gateway"))]
            {
                log::warn!(
                    "Gateway is enabled in configuration but the \"gateway\" feature is not compiled in; the gateway stays stopped."
                );
            }
        }
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        bus.register(
            LB_BACKEND_UNHEALTHY,
            "gateway",
            Arc::new(on_lb_backend_unhealthy),
        );
        bus.register(
            LB_BACKEND_HEALTHY,
            "gateway",
            Arc::new(on_lb_backend_healthy),
        );
        bus.register(events::SSL_SWAP, "gateway", Arc::new(on_ssl_event));
        bus.register(events::SSL_RENEW, "gateway", Arc::new(on_ssl_event));
    }

    fn register_cli(&self, registry: &mut CliRegistry) {
        registry.register(CliCommandDef {
            name: "gateway",
            about: "Manage the Pingora gateway proxy (start, stop, status, reload)",
            command: gateway_command(),
            handler: Arc::new(|matches| gateway_cli(matches)),
        });
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/gateway")
                .route("", web::get().to(get_status))
                .route("/rules", web::get().to(list_rules_handler))
                .route("/rules", web::post().to(create_rule_handler))
                .route("/rules/{id}", web::get().to(get_rule_handler))
                .route("/rules/{id}", web::put().to(update_rule_handler))
                .route("/rules/{id}", web::delete().to(delete_rule_handler))
                .route("/upstreams", web::get().to(list_upstreams))
                .route("/health", web::get().to(list_health))
                .route("/health/check-now", web::post().to(check_now_handler))
                .route("/tls/{name}", web::get().to(get_tls_status))
                .route("/requests", web::get().to(list_requests))
                .route("/start", web::post().to(start_handler))
                .route("/stop", web::post().to(stop_handler))
                .route("/reload", web::post().to(reload_handler)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        let routes = [
            ("GET", "/api/v1/gateway", "Gateway status: feature, enabled, running, listeners, rule and upstream counts"),
            ("GET", "/api/v1/gateway/rules", "List routing rules (pagination/filter)"),
            ("POST", "/api/v1/gateway/rules", "Create a routing rule"),
            ("GET", "/api/v1/gateway/rules/{id}", "Get one routing rule"),
            ("PUT", "/api/v1/gateway/rules/{id}", "Update a routing rule"),
            ("DELETE", "/api/v1/gateway/rules/{id}", "Delete a routing rule"),
            ("GET", "/api/v1/gateway/upstreams", "List upstream pools (read-only views from load balancers and direct pools, paginated)"),
            ("GET", "/api/v1/gateway/health", "List upstream health state (pagination/filter)"),
            ("POST", "/api/v1/gateway/health/check-now", "Probe every upstream now and return the health snapshot"),
            ("GET", "/api/v1/gateway/tls/{name}", "TLS certificate status for one server name"),
            ("GET", "/api/v1/gateway/requests", "Proxied-request log tail (pagination/filter, newest first)"),
            ("POST", "/api/v1/gateway/start", "Start the gateway service"),
            ("POST", "/api/v1/gateway/stop", "Stop (drain) the gateway service"),
            ("POST", "/api/v1/gateway/reload", "Reload gateway configuration and routing rules without restarting the listener"),
        ];
        routes
            .iter()
            .map(|(method, path, description)| RouteInfo {
                method,
                path,
                owner: "gateway",
                description,
            })
            .collect()
    }
}

/// Register the gateway module. Conditional on the `gateway` Cargo
/// feature: when the feature is off there is no proxy service to
/// manage, so the module is not registered at all.
#[cfg(feature = "gateway")]
pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(GatewayModule::new()));
}

// ---------------------------------------------------------------------------
// Hook handlers.
// ---------------------------------------------------------------------------

/// A load balancer backend went unhealthy: exclude it from gateway
/// rotation. The payload carries `balancer_id`, `backend_id`, and
/// `address` (see `load_balancer.rs`).
fn on_lb_backend_unhealthy(context: &mut HookContext) {
    let Some((balancer_id, backend_id, address)) = lb_backend_identity(context) else {
        return;
    };
    let state = GatewayState::global();
    let key = backend_health_key(balancer_id, backend_id);
    mark_unhealthy(state, &key, &address);
}

/// A load balancer backend recovered: re-admit it to gateway rotation.
fn on_lb_backend_healthy(context: &mut HookContext) {
    let Some((balancer_id, backend_id, address)) = lb_backend_identity(context) else {
        return;
    };
    let state = GatewayState::global();
    let key = backend_health_key(balancer_id, backend_id);
    mark_healthy(state, &key, &address);
}

fn lb_backend_identity(context: &HookContext) -> Option<(Uuid, Uuid, String)> {
    let balancer_id = context
        .get("balancer_id")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<Uuid>().ok())?;
    let backend_id = context
        .get("backend_id")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<Uuid>().ok())?;
    let address = context
        .get("address")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    Some((balancer_id, backend_id, address))
}

/// A certificate was swapped or renewed: TLS status is computed live
/// from the storage, so there is nothing to invalidate — re-emit a
/// gateway event so operators and the monitor can observe it.
fn on_ssl_event(context: &mut HookContext) {
    let domain = context
        .get("domain")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    GatewayState::global().emit(
        GATEWAY_TLS_CHANGED,
        json_value!({ "domain": domain, "event": context.event.clone() }),
    );
}

// ---------------------------------------------------------------------------
// Views.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct GatewayStatusView {
    feature_enabled: bool,
    enabled: bool,
    running: bool,
    listen: Vec<String>,
    tls_listen: Vec<String>,
    default_upstream: Option<String>,
    rule_count: usize,
    upstream_pool_count: usize,
    eligible_upstream_count: usize,
    healthy_upstream_count: usize,
    unhealthy_upstream_count: usize,
    request_log_entries: usize,
    tls_storage_attached: bool,
    started_at: Option<DateTime<Utc>>,
    uptime_seconds: Option<u64>,
}

fn status_view(state: &GatewayState) -> GatewayStatusView {
    let config = state.config_snapshot();
    let pools = upstream_pools(state);
    let health = state.health_snapshot();
    GatewayStatusView {
        feature_enabled: feature_enabled(),
        enabled: config.enabled,
        running: state.is_running(),
        listen: config.listen,
        tls_listen: config.tls_listen,
        default_upstream: config.default_upstream,
        rule_count: list_rules().len(),
        upstream_pool_count: pools.len(),
        eligible_upstream_count: pools.iter().map(|pool| pool.backends.len()).sum(),
        healthy_upstream_count: health
            .iter()
            .filter(|entry| entry.state == crate::gateway::UpstreamHealthState::Healthy)
            .count(),
        unhealthy_upstream_count: health
            .iter()
            .filter(|entry| entry.state == crate::gateway::UpstreamHealthState::Unhealthy)
            .count(),
        request_log_entries: state.request_log_snapshot().len(),
        tls_storage_attached: state.tls_storage_attached(),
        started_at: state.started_at(),
        uptime_seconds: state
            .started_at()
            .map(|started| Utc::now().signed_duration_since(started).num_seconds().max(0) as u64),
    }
}

#[derive(Debug, Clone, Serialize)]
struct UpstreamBackendView {
    backend_id: Option<Uuid>,
    name: String,
    address: String,
    source_kind: Option<String>,
    source_id: Option<String>,
    weight: u32,
    health: String,
}

#[derive(Debug, Clone, Serialize)]
struct UpstreamPoolView {
    rule_id: Uuid,
    rule_name: String,
    upstream_kind: String,
    balancer_id: Option<Uuid>,
    backends: Vec<UpstreamBackendView>,
    /// Where the health in this view comes from: the load balancer
    /// for balancer pools, the gateway sweeper for direct pools.
    health_source: String,
}

/// Read-only upstream pool views: balancer pools resolve through the
/// load balancer's live eligible set (cluster-node backends appear
/// here with `source_kind: "cluster"` and
/// `source_id: "<cluster_id>/<node_id>"`); direct pools come from
/// the rule itself.
fn upstream_pools(state: &GatewayState) -> Vec<UpstreamPoolView> {
    let mut pools = Vec::new();
    for rule in list_rules() {
        let (kind, balancer_id, backends, health_source) = match &rule.upstream {
            crate::gateway::routing::UpstreamRef::Balancer { balancer_id } => {
                let backends = eligible_backends(*balancer_id)
                    .iter()
                    .map(|backend| {
                        let gateway_health = state
                            .health
                            .get(&backend_health_key(*balancer_id, backend.id))
                            .map(|entry| {
                                format!("{:?}", entry.state).to_lowercase()
                            })
                            .unwrap_or_else(|| {
                                format!("{:?}", backend.health).to_lowercase()
                            });
                        UpstreamBackendView {
                            backend_id: Some(backend.id),
                            name: backend.name.clone(),
                            address: backend.address.clone(),
                            source_kind: Some(
                                format!("{:?}", backend.source_kind).to_lowercase(),
                            ),
                            source_id: backend.source_id.clone(),
                            weight: backend.weight,
                            health: gateway_health,
                        }
                    })
                    .collect();
                (
                    "balancer".to_string(),
                    Some(*balancer_id),
                    backends,
                    "load_balancer".to_string(),
                )
            }
            crate::gateway::routing::UpstreamRef::Address { address } => {
                let health = state
                    .health
                    .get(&crate::gateway::health::address_health_key(address))
                    .map(|entry| format!("{:?}", entry.state).to_lowercase())
                    .unwrap_or_else(|| "unknown".to_string());
                (
                    "address".to_string(),
                    None,
                    vec![UpstreamBackendView {
                        backend_id: None,
                        name: address.clone(),
                        address: address.clone(),
                        source_kind: None,
                        source_id: None,
                        weight: 1,
                        health,
                    }],
                    "gateway".to_string(),
                )
            }
            crate::gateway::routing::UpstreamRef::AddressPool { addresses } => {
                let backends = addresses
                    .iter()
                    .map(|entry| {
                        let health = state
                            .health
                            .get(&crate::gateway::health::address_health_key(&entry.address))
                            .map(|record| format!("{:?}", record.state).to_lowercase())
                            .unwrap_or_else(|| "unknown".to_string());
                        UpstreamBackendView {
                            backend_id: None,
                            name: entry.address.clone(),
                            address: entry.address.clone(),
                            source_kind: None,
                            source_id: None,
                            weight: entry.weight,
                            health,
                        }
                    })
                    .collect();
                (
                    "address_pool".to_string(),
                    None,
                    backends,
                    "gateway".to_string(),
                )
            }
        };
        pools.push(UpstreamPoolView {
            rule_id: rule.id,
            rule_name: rule.name.clone(),
            upstream_kind: kind,
            balancer_id,
            backends,
            health_source,
        });
    }
    pools
}

// ---------------------------------------------------------------------------
// Pagination helper (same shape as the load balancer's list helper:
// allow-listed sort, normalized page, `PageResult` JSON).
// ---------------------------------------------------------------------------

fn paginate<T, F>(
    items: Vec<T>,
    query: &PaginationParams,
    sort_allowed: &[&str],
    default_sort: &str,
    mut sort_key: F,
) -> PageResult<T>
where
    T: Serialize,
    F: FnMut(&T, &str) -> String,
{
    let pagination = query.normalized();
    let sort = query.allowed_sort(sort_allowed, default_sort);
    let mut items = items;
    let order_desc = pagination.order == "desc";
    items.sort_by(|a, b| {
        let ordering = sort_key(a, &sort).cmp(&sort_key(b, &sort));
        if order_desc {
            ordering.reverse()
        } else {
            ordering
        }
    });
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    raw.parse::<Uuid>().map_err(|_| {
        UnifiedError::bad_request(format!("Invalid {what} id \"{raw}\": expected a UUID"))
    })
}

fn matches_search(haystack: &str, search: &Option<String>) -> bool {
    match search {
        None => true,
        Some(needle) => haystack.to_lowercase().contains(&needle.to_lowercase()),
    }
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn get_status(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(Ok::<GatewayStatusView, UnifiedError>(status_view(GatewayState::global())), debug)
}

async fn list_rules_handler(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut rules = list_rules();
        if let Some(search) = &query.search {
            let needle = search.to_lowercase();
            rules.retain(|rule| {
                rule.name.to_lowercase().contains(&needle)
                    || rule
                        .host
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&needle)
            });
        }
        if let Some(enabled) = query.filters.get("enabled") {
            let wanted = enabled == "true";
            rules.retain(|rule| rule.enabled == wanted);
        }
        if let Some(host) = query.filters.get("host") {
            rules.retain(|rule| rule.host.as_deref() == Some(host.as_str()));
        }
        Ok(paginate(
            rules,
            &query,
            &["priority", "name", "created_at"],
            "priority",
            |rule, sort| match sort {
                "name" => rule.name.clone(),
                "created_at" => rule.created_at.to_rfc3339(),
                _ => format!("{:010}", rule.priority),
            },
        )
        .to_json())
    })();
    json::respond(result, debug)
}

async fn create_rule_handler(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = create_rule(body.into_inner());
    match &result {
        Ok(rule) => GatewayState::global().emit(
            GATEWAY_RULE_CREATED,
            json_value!({
                "rule_id": rule.id,
                "name": rule.name,
                "priority": rule.priority,
            }),
        ),
        Err(_) => {}
    }
    match result {
        Ok(rule) => json::success_response(rule, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_rule_handler(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = parse_uuid(&id, "routing rule").and_then(get_rule);
    json::respond(result, debug)
}

async fn update_rule_handler(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
    body: web::Json<UpdateRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = parse_uuid(&id, "routing rule")
        .and_then(|rule_id| update_rule(rule_id, body.into_inner()));
    if let Ok(rule) = &result {
        GatewayState::global().emit(
            GATEWAY_RULE_UPDATED,
            json_value!({
                "rule_id": rule.id,
                "name": rule.name,
                "priority": rule.priority,
            }),
        );
    }
    json::respond(result, debug)
}

async fn delete_rule_handler(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = parse_uuid(&id, "routing rule").and_then(|rule_id| {
        let removed = delete_rule(rule_id)?;
        GatewayState::global().emit(
            GATEWAY_RULE_DELETED,
            json_value!({ "rule_id": removed.id, "name": removed.name }),
        );
        Ok(json_value!({ "deleted": true, "id": removed.id }))
    });
    json::respond(result, debug)
}

async fn list_upstreams(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut pools = upstream_pools(GatewayState::global());
        if let Some(search) = &query.search {
            let needle = search.to_lowercase();
            pools.retain(|pool| {
                pool.rule_name.to_lowercase().contains(&needle)
                    || pool
                        .backends
                        .iter()
                        .any(|backend| backend.address.to_lowercase().contains(&needle))
            });
        }
        if let Some(kind) = query.filters.get("upstream_kind") {
            pools.retain(|pool| pool.upstream_kind == *kind);
        }
        Ok(paginate(
            pools,
            &query,
            &["rule_name", "upstream_kind"],
            "rule_name",
            |pool, sort| match sort {
                "upstream_kind" => pool.upstream_kind.clone(),
                _ => pool.rule_name.clone(),
            },
        )
        .to_json())
    })();
    json::respond(result, debug)
}

async fn list_health(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut entries = GatewayState::global().health_snapshot();
        if let Some(search) = &query.search {
            let needle = search.to_lowercase();
            entries.retain(|entry| {
                entry.address.to_lowercase().contains(&needle)
                    || entry.key.to_lowercase().contains(&needle)
            });
        }
        if let Some(state) = query.filters.get("state") {
            let wanted = state.to_lowercase();
            entries.retain(|entry| {
                format!("{:?}", entry.state).to_lowercase() == wanted
            });
        }
        if let Some(kind) = query.filters.get("kind") {
            entries.retain(|entry| entry.kind == *kind);
        }
        Ok(paginate(
            entries,
            &query,
            &["address", "key", "state"],
            "address",
            |entry, sort| match sort {
                "key" => entry.key.clone(),
                "state" => format!("{:?}", entry.state),
                _ => entry.address.clone(),
            },
        )
        .to_json())
    })();
    json::respond(result, debug)
}

async fn check_now_handler(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = match require_feature_enabled() {
        Ok(()) => Ok(json_value!(
            health_check_now(GatewayState::global()).await
        )),
        Err(error) => Err(error),
    };
    json::respond(result, debug)
}

async fn get_tls_status(
    ctx: web::Data<Arc<AppContext>>,
    name: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let state = GatewayState::global();
        let app_config = state.app_config_snapshot().unwrap_or_default();
        let storage = state.tls_storage();
        Ok(json_value!(certificate_status(
            storage.as_deref(),
            &name,
            &app_config
        )))
    })();
    json::respond(result, debug)
}

async fn list_requests(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut entries = GatewayState::global().request_log_snapshot();
        if let Some(search) = &query.search {
            let needle = search.to_lowercase();
            entries.retain(|entry| {
                entry.path.to_lowercase().contains(&needle)
                    || entry.host.to_lowercase().contains(&needle)
                    || entry.request_id.to_lowercase().contains(&needle)
            });
        }
        if let Some(status) = query.filters.get("status") {
            if let Ok(wanted) = status.parse::<u16>() {
                entries.retain(|entry| entry.status == wanted);
            }
        }
        if let Some(method) = query.filters.get("method") {
            entries.retain(|entry| entry.method.eq_ignore_ascii_case(method));
        }
        if let Some(upstream) = query.filters.get("upstream") {
            entries.retain(|entry| {
                entry
                    .upstream
                    .as_deref()
                    .unwrap_or("")
                    .contains(upstream.as_str())
            });
        }
        Ok(paginate(
            entries,
            &query,
            &["recorded_at", "status", "duration_ms"],
            "recorded_at",
            |entry, sort| match sort {
                "status" => format!("{:05}", entry.status),
                "duration_ms" => format!("{:020}", entry.duration_ms),
                _ => entry.recorded_at.to_rfc3339(),
            },
        )
        .to_json())
    })();
    json::respond(result, debug)
}

async fn start_handler(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(start_gateway_inner(), debug)
}

async fn stop_handler(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(stop_gateway_inner(), debug)
}

async fn reload_handler(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(reload_gateway_inner(), debug)
}

/// Start the service (feature-on implementation). The feature-off
/// twin below returns the "feature not enabled" error. Split into
/// two cfg-gated functions because two cfg-gated blocks in tail
/// position do not compose: the surviving block would become a
/// discarded statement instead of the return value.
#[cfg(feature = "gateway")]
fn start_gateway_inner() -> UnifiedResult<Value> {
    require_feature_enabled()?;
    let state = GatewayState::global();
    let handle = crate::gateway::proxy::GatewayService::start(state.clone())?;
    {
        let mut service = state.service.lock().expect("gateway service lock poisoned");
        *service = Some(handle);
    }
    Ok(json_value!(status_view(state)))
}

#[cfg(not(feature = "gateway"))]
fn start_gateway_inner() -> UnifiedResult<Value> {
    Err(UnifiedError::unprocessable(
        "Gateway feature not enabled: rebuild with the \"gateway\" Cargo feature enabled",
    ))
}

#[cfg(feature = "gateway")]
fn stop_gateway_inner() -> UnifiedResult<Value> {
    require_feature_enabled()?;
    let state = GatewayState::global();
    crate::gateway::proxy::GatewayService::stop(state.clone())?;
    Ok(json_value!(status_view(state)))
}

#[cfg(not(feature = "gateway"))]
fn stop_gateway_inner() -> UnifiedResult<Value> {
    Err(UnifiedError::unprocessable(
        "Gateway feature not enabled: rebuild with the \"gateway\" Cargo feature enabled",
    ))
}

#[cfg(feature = "gateway")]
fn reload_gateway_inner() -> UnifiedResult<Value> {
    require_feature_enabled()?;
    let state = GatewayState::global();
    let problems = crate::gateway::proxy::GatewayService::reload(state.clone())?;
    Ok(json_value!({
        "problems": problems,
        "status": status_view(state),
    }))
}

#[cfg(not(feature = "gateway"))]
fn reload_gateway_inner() -> UnifiedResult<Value> {
    Err(UnifiedError::unprocessable(
        "Gateway feature not enabled: rebuild with the \"gateway\" Cargo feature enabled",
    ))
}

// ---------------------------------------------------------------------------
// CLI: `unified gateway <start|stop|status|reload>`.
//
// The gateway lives inside the daemon process, so this command works
// through the daemon: `status` reads the live management API over
// HTTP, `reload` sends SIGHUP (the daemon's existing reload signal),
// `start` refuses when the daemon is already running and otherwise
// tells the operator to start the daemon (`unified start`), and
// `stop` cannot stop the in-process gateway from a separate process
// — it reports the management API call to use instead. There is no
// separate gateway process to start or stop from the CLI.
// ---------------------------------------------------------------------------

fn gateway_command() -> Command {
    Command::new("gateway")
        .about("Manage the Pingora gateway proxy")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(Command::new("start").about("Start the gateway (starts the daemon when it is not running)"))
        .subcommand(Command::new("stop").about("Stop the gateway (drains proxied traffic; use the management API against the running daemon)"))
        .subcommand(Command::new("status").about("Show the gateway status from the running daemon"))
        .subcommand(Command::new("reload").about("Reload the gateway configuration (sends SIGHUP to the daemon)"))
}

fn gateway_cli(matches: &ArgMatches) -> UnifiedResult<()> {
    match matches.subcommand() {
        Some(("start", _)) => gateway_cli_start(),
        Some(("stop", _)) => gateway_cli_stop(),
        Some(("status", _)) => gateway_cli_status(),
        Some(("reload", _)) => gateway_cli_reload(),
        _ => Err(UnifiedError::bad_request(
            "Usage: unified gateway <start|stop|status|reload>",
        )),
    }
}

fn gateway_cli_start() -> UnifiedResult<()> {
    if crate::cli::daemon::is_running().is_some() {
        println!("Unified daemon is already running; the gateway runs inside it.");
        println!("Check its status with `unified gateway status`.");
        return Ok(());
    }
    Err(UnifiedError::unprocessable(
        "The gateway runs inside the Unified daemon, which is not running. Start it with `unified start` (enable the gateway with gateway.enabled=true first).",
    ))
}

fn gateway_cli_stop() -> UnifiedResult<()> {
    if crate::cli::daemon::is_running().is_none() {
        println!("Unified daemon is not running; the gateway is already stopped.");
        return Ok(());
    }
    Err(UnifiedError::unprocessable(
        "The gateway cannot be stopped from a separate CLI process. Stop (drain) it through the running daemon instead: POST /api/v1/gateway/stop (or stop the whole daemon with `unified stop`).",
    ))
}

fn gateway_cli_status() -> UnifiedResult<()> {
    // Read the live status from the daemon's management API instead
    // of reporting a fresh process's empty in-memory state.
    let config = crate::core::config::AppConfig::load_default().unwrap_or_default();
    let base = config
        .app
        .tcp
        .first()
        .cloned()
        .unwrap_or_else(|| "127.0.0.1:8080".to_string());
    let url = format!("http://{base}/api/v1/gateway");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot start the CLI runtime: {error}"))
        })?;
    let body = runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot build the HTTP client: {error}"))
            })?;
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!(
                    "Cannot reach the gateway status endpoint at {url}: {error}"
                ))
            })?;
        response.text().await.map_err(|error| {
            UnifiedError::internal(format!("Cannot read the gateway status response: {error}"))
        })
    })?;
    println!("Gateway feature compiled in: {}", feature_enabled());
    println!("{body}");
    Ok(())
}

fn gateway_cli_reload() -> UnifiedResult<()> {
    match crate::cli::daemon::send_signal(nix::sys::signal::Signal::SIGHUP) {
        Ok(true) => {
            println!("Reload signal sent to the running daemon; the gateway re-validates its configuration and rules.");
            Ok(())
        }
        Ok(false) => Err(UnifiedError::unprocessable(
            "No daemon is running; start it with `unified start` before reloading the gateway.",
        )),
        Err(error) => Err(UnifiedError::internal(format!(
            "Cannot send the reload signal: {error}"
        ))),
    }
}
