// load_balancer -- Module struct, Module impl and registration.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::HookBus;
use unified_core::hooks::events;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::web;
use std::sync::Arc;

use super::constants::*;
use super::handlers_backends::*;
use super::handlers_balancers::*;
use super::health::*;
use super::hooks::*;
use super::store::*;

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct LoadBalancerModule;

impl LoadBalancerModule {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LoadBalancerModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for LoadBalancerModule {
    fn id(&self) -> &'static str {
        "load_balancer"
    }

    fn name(&self) -> &'static str {
        "Load Balancer"
    }

    fn description(&self) -> &'static str {
        "Load balancers over provider, cluster, or plain-address backends: round-robin, least-connections, weighted, and IP-hash strategies, HTTP/TCP health checks with automatic failover, TLS termination through the shared certificate storage, and per-backend routing statistics."
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["reqwest", "dashmap", "sha2"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let _ = HOOK_BUS.set(ctx.hooks.clone());
        start_health_sweeper();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        for event in [events::SSL_RENEW, events::SSL_SWAP] {
            bus.register(event, "load_balancer", Arc::new(on_ssl_event));
        }
        bus.register(
            CLUSTER_NODE_UNHEALTHY,
            "load_balancer",
            Arc::new(on_cluster_node_unhealthy),
        );
        bus.register(
            CLUSTER_NODE_HEALTHY,
            "load_balancer",
            Arc::new(on_cluster_node_healthy),
        );
        bus.register(
            CLUSTER_NODE_REMOVED,
            "load_balancer",
            Arc::new(on_cluster_node_removed),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/load-balancer")
                .route("", web::get().to(list_balancers))
                .route("", web::post().to(create_balancer))
                .route("/{id}", web::get().to(get_balancer_handler))
                .route("/{id}", web::put().to(update_balancer))
                .route("/{id}", web::delete().to(delete_balancer))
                .route("/{id}/enable", web::post().to(enable_balancer))
                .route("/{id}/disable", web::post().to(disable_balancer))
                .route("/{id}/route", web::get().to(route_request))
                .route("/{id}/preview", web::post().to(preview_selection))
                .route("/{id}/tls", web::get().to(get_tls_status))
                .route("/{id}/check-now", web::post().to(check_now_balancer))
                .route("/{id}/stats", web::get().to(balancer_stats))
                .route("/{id}/stats/reset", web::post().to(reset_balancer_stats))
                .route("/{id}/backends", web::get().to(list_backends))
                .route("/{id}/backends", web::post().to(create_backend))
                .route(
                    "/{id}/backends/{backend_id}",
                    web::get().to(get_backend_handler),
                )
                .route("/{id}/backends/{backend_id}", web::put().to(update_backend))
                .route(
                    "/{id}/backends/{backend_id}",
                    web::delete().to(delete_backend),
                )
                .route(
                    "/{id}/backends/{backend_id}/enable",
                    web::post().to(enable_backend),
                )
                .route(
                    "/{id}/backends/{backend_id}/disable",
                    web::post().to(disable_backend),
                )
                .route(
                    "/{id}/backends/{backend_id}/drain",
                    web::post().to(drain_backend),
                )
                .route(
                    "/{id}/backends/{backend_id}/undrain",
                    web::post().to(undrain_backend),
                )
                .route(
                    "/{id}/backends/{backend_id}/check-now",
                    web::post().to(check_now_backend),
                )
                .route(
                    "/{id}/backends/{backend_id}/stats",
                    web::get().to(backend_stats_handler),
                )
                .route(
                    "/{id}/backends/{backend_id}/stats/reset",
                    web::post().to(reset_backend_stats),
                )
                .route(
                    "/{id}/backends/{backend_id}/connections/release",
                    web::post().to(release_connection),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        let routes = [
            (
                "GET",
                "/api/v1/load-balancer",
                "List load balancers (pagination/filter)",
            ),
            ("POST", "/api/v1/load-balancer", "Create a load balancer"),
            ("GET", "/api/v1/load-balancer/{id}", "Get one load balancer"),
            (
                "PUT",
                "/api/v1/load-balancer/{id}",
                "Update a load balancer",
            ),
            (
                "DELETE",
                "/api/v1/load-balancer/{id}",
                "Delete a load balancer and its backends",
            ),
            (
                "POST",
                "/api/v1/load-balancer/{id}/enable",
                "Enable a load balancer",
            ),
            (
                "POST",
                "/api/v1/load-balancer/{id}/disable",
                "Disable a load balancer",
            ),
            (
                "GET",
                "/api/v1/load-balancer/{id}/route",
                "Route one request (simulated)",
            ),
            (
                "POST",
                "/api/v1/load-balancer/{id}/preview",
                "Preview backend selection for a client IP",
            ),
            (
                "GET",
                "/api/v1/load-balancer/{id}/tls",
                "TLS termination status",
            ),
            (
                "POST",
                "/api/v1/load-balancer/{id}/check-now",
                "Probe every backend now",
            ),
            (
                "GET",
                "/api/v1/load-balancer/{id}/stats",
                "Balancer and backend statistics",
            ),
            (
                "POST",
                "/api/v1/load-balancer/{id}/stats/reset",
                "Reset balancer and backend statistics",
            ),
            (
                "GET",
                "/api/v1/load-balancer/{id}/backends",
                "List backends (pagination/filter)",
            ),
            (
                "POST",
                "/api/v1/load-balancer/{id}/backends",
                "Add a backend",
            ),
            (
                "GET",
                "/api/v1/load-balancer/{id}/backends/{backend_id}",
                "Get one backend",
            ),
            (
                "PUT",
                "/api/v1/load-balancer/{id}/backends/{backend_id}",
                "Update a backend",
            ),
            (
                "DELETE",
                "/api/v1/load-balancer/{id}/backends/{backend_id}",
                "Delete a backend",
            ),
            (
                "POST",
                "/api/v1/load-balancer/{id}/backends/{backend_id}/enable",
                "Enable a backend",
            ),
            (
                "POST",
                "/api/v1/load-balancer/{id}/backends/{backend_id}/disable",
                "Disable a backend",
            ),
            (
                "POST",
                "/api/v1/load-balancer/{id}/backends/{backend_id}/drain",
                "Drain a backend (no new traffic)",
            ),
            (
                "POST",
                "/api/v1/load-balancer/{id}/backends/{backend_id}/undrain",
                "Stop draining a backend",
            ),
            (
                "POST",
                "/api/v1/load-balancer/{id}/backends/{backend_id}/check-now",
                "Probe one backend now",
            ),
            (
                "GET",
                "/api/v1/load-balancer/{id}/backends/{backend_id}/stats",
                "Backend statistics",
            ),
            (
                "POST",
                "/api/v1/load-balancer/{id}/backends/{backend_id}/stats/reset",
                "Reset backend statistics",
            ),
            (
                "POST",
                "/api/v1/load-balancer/{id}/backends/{backend_id}/connections/release",
                "Release one active connection",
            ),
        ];
        routes
            .iter()
            .map(|(method, path, description)| RouteInfo {
                method,
                path,
                owner: "load_balancer",
                description,
                // Table-driven admin routes: none of these is public.
                auth: unified_core::auth::RouteAuth::Authenticated,
            })
            .collect()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(LoadBalancerModule::new()));
}

// ---------------------------------------------------------------------------
// INTEGRATOR NOTES -- snippets for shared files (do not apply here).
//
// 1. `src/modules/mod.rs` -- declare and register the module:
//
//        pub mod load_balancer;
//        // ... in register_all(), Operations section, after
//        // network::register(registry):
//        load_balancer::register(registry);
//
// 2. crates/unified-core/src/config.rs` -- optional defaults section (all fields
//    have defaults; balancers also carry per-balancer overrides):
//
//        #[derive(Debug, Clone, Serialize, Deserialize)]
//        #[serde(default)]
//        pub struct LoadBalancerSection {
//            /// Default health-check interval in seconds (5..=3600).
//            pub health_check_interval_secs: u64,
//            /// Default health-check timeout in seconds (1..=60).
//            pub health_check_timeout_secs: u64,
//            /// Default health-check path for HTTP checks.
//            pub health_check_path: String,
//            /// Default strategy: round_robin | least_connections |
//            /// weighted | ip_hash.
//            pub default_strategy: String,
//            /// Maximum backends per balancer (1..=1000).
//            pub max_backends_per_balancer: usize,
//        }
//        // Add `pub load_balancer: LoadBalancerSection` to `AppConfig`
//        // (with `load_balancer: LoadBalancerSection::default()` in
//        // `AppConfig::default()`), plus a `Default` impl with
//        // interval 30, timeout 5, path "/health", strategy
//        // "round_robin", max backends 100. YAML example:
//        //
//        //   load_balancer:
//        //     health_check_interval_secs: 30
//        //     health_check_timeout_secs: 5
//        //     health_check_path: /health
//        //     default_strategy: round_robin
//        //     max_backends_per_balancer: 100
//
// 3. crates/unified-core/src/context.rs` + `src/http/server.rs` -- TLS storage
//    wiring. DONE: the context field, the setter, the `set_tls` call
//    in `run_server`, and the `get_tls_status` switch to `ctx.tls()`
//    (with the config-only fallback) are all in place; the snippet
//    below is kept for reference. Without the attached handle the
//    endpoint reports configured certificates from `app.ssl.domains`
//    plus the `ssl.swap` / `ssl.renew` cache only:
//
//        // context.rs -- add to `AppContext`:
//        pub tls: Arc<RwLock<Option<Arc<unified_core::tls::SslStorage>>>>,
//        // ... and in `AppContext::new`:
//        tls: Arc::new(RwLock::new(None)),
//        // ... plus a setter:
//        pub fn set_tls(&self, storage: Arc<unified_core::tls::SslStorage>) {
//            *self.tls.write().expect("tls lock poisoned") = Some(storage);
//        }
//
//        // server.rs -- in `run_server`, after the TLS storage is
//        // built (`let storage = unified_core::tls::SslStorage::from_config(&ssl)?;`):
//        ctx.set_tls(storage.clone());
//        // ... and in this module's `get_tls_status`, replace the
//        // config-only lookup with:
//        let info = match ctx.tls.read().expect("tls lock poisoned").as_ref() {
//            Some(storage) => Some(tls_certificate_status(storage, &name)),
//            None => Some(tls_status_from_config(&name, &config)),
//        };
//        // (and set `"storage_attached"` from whether the handle is
//        // present). The `SslStorage` is also the resolver behind
//        // `tls::server_config`, so a balancer with
//        // `tls_mode: "terminate"` is served by the same storage the
//        // HTTPS listener already uses; no second TLS stack is built.
//        // Per-balancer listeners (one `bind_rustls_0_23` per
//        // `listen_address:listen_port`) are a server.rs concern:
//        // for each enabled balancer with `tls_mode == terminate`,
//        // bind its listen address with `tls::server_config(&storage)`
//        // and route through the same Actix app, dispatching to the
//        // selected backend. That listener work is intentionally not
//        // in this module: the module owns selection, health, and
//        // stats; the server owns sockets.
//
// 4. Database persistence (optional; the module currently stores
//    everything in memory). Migration
//    `migrations/<timestamp>_load_balancer/up.sql`:
//
//        CREATE TABLE load_balancers (
//            id UUID PRIMARY KEY,
//            name VARCHAR(200) NOT NULL,
//            listen_address VARCHAR(255) NOT NULL,
//            listen_port INTEGER NOT NULL CHECK (listen_port BETWEEN 1 AND 65535),
//            tls_mode VARCHAR(16) NOT NULL DEFAULT 'off'
//                CHECK (tls_mode IN ('off', 'terminate', 'passthrough')),
//            domain VARCHAR(253),
//            certificate_name VARCHAR(253),
//            strategy VARCHAR(32) NOT NULL DEFAULT 'round_robin'
//                CHECK (strategy IN ('round_robin', 'least_connections', 'weighted', 'ip_hash')),
//            health_check JSONB NOT NULL DEFAULT '{}'::jsonb,
//            enabled BOOLEAN NOT NULL DEFAULT TRUE,
//            status VARCHAR(16) NOT NULL DEFAULT 'active'
//                CHECK (status IN ('active', 'degraded', 'disabled')),
//            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
//            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
//            UNIQUE (listen_address, listen_port)
//        );
//        CREATE TABLE lb_backends (
//            id UUID PRIMARY KEY,
//            balancer_id UUID NOT NULL REFERENCES load_balancers(id) ON DELETE CASCADE,
//            name VARCHAR(200) NOT NULL,
//            source_kind VARCHAR(16) NOT NULL DEFAULT 'address'
//                CHECK (source_kind IN ('provider', 'cluster', 'address')),
//            source_id VARCHAR(512),
//            address VARCHAR(255) NOT NULL,
//            weight INTEGER NOT NULL DEFAULT 1 CHECK (weight BETWEEN 1 AND 100),
//            enabled BOOLEAN NOT NULL DEFAULT TRUE,
//            draining BOOLEAN NOT NULL DEFAULT FALSE,
//            health VARCHAR(16) NOT NULL DEFAULT 'unknown'
//                CHECK (health IN ('unknown', 'healthy', 'unhealthy')),
//            last_check_at TIMESTAMPTZ,
//            last_latency_ms BIGINT,
//            consecutive_failures INTEGER NOT NULL DEFAULT 0,
//            consecutive_successes INTEGER NOT NULL DEFAULT 0,
//            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
//            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
//        );
//        CREATE INDEX lb_backends_balancer_id_idx ON lb_backends (balancer_id);
//        CREATE TABLE lb_backend_stats (
//            backend_id UUID PRIMARY KEY REFERENCES lb_backends(id) ON DELETE CASCADE,
//            requests_routed BIGINT NOT NULL DEFAULT 0,
//            errors BIGINT NOT NULL DEFAULT 0,
//            total_latency_ms BIGINT NOT NULL DEFAULT 0,
//            latency_samples BIGINT NOT NULL DEFAULT 0,
//            checks_total BIGINT NOT NULL DEFAULT 0,
//            checks_ok BIGINT NOT NULL DEFAULT 0,
//            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
//        );
//
//    crates/unified-core/src/db/schema.rs` -- matching Diesel entries:
//
//        diesel::table! {
//            load_balancers (id) {
//                id -> Uuid,
//                name -> Varchar,
//                listen_address -> Varchar,
//                listen_port -> Int4,
//                tls_mode -> Varchar,
//                domain -> Nullable<Varchar>,
//                certificate_name -> Nullable<Varchar>,
//                strategy -> Varchar,
//                health_check -> Jsonb,
//                enabled -> Bool,
//                status -> Varchar,
//                created_at -> Timestamptz,
//                updated_at -> Timestamptz,
//            }
//        }
//        diesel::table! {
//            lb_backends (id) {
//                id -> Uuid,
//                balancer_id -> Uuid,
//                name -> Varchar,
//                source_kind -> Varchar,
//                source_id -> Nullable<Varchar>,
//                address -> Varchar,
//                weight -> Int4,
//                enabled -> Bool,
//                draining -> Bool,
//                health -> Varchar,
//                last_check_at -> Nullable<Timestamptz>,
//                last_latency_ms -> Nullable<Int8>,
//                consecutive_failures -> Int4,
//                consecutive_successes -> Int4,
//                created_at -> Timestamptz,
//                updated_at -> Timestamptz,
//            }
//        }
//        diesel::table! {
//            lb_backend_stats (backend_id) {
//                backend_id -> Uuid,
//                requests_routed -> Int8,
//                errors -> Int8,
//                total_latency_ms -> Int8,
//                latency_samples -> Int8,
//                checks_total -> Int8,
//                checks_ok -> Int8,
//                updated_at -> Timestamptz,
//            }
//        }
//
// 5. Cluster module contract (for `clusters.rs`): emit these hook
//    events with `{"cluster_id", "node_id"}` payloads and this
//    module reacts without importing cluster code:
//
//        hooks.emit("cluster.node.unhealthy", json!({"cluster_id": id, "node_id": id}));
//        hooks.emit("cluster.node.healthy", json!({"cluster_id": id, "node_id": id}));
//        hooks.emit("cluster.node.removed", json!({"cluster_id": id, "node_id": id}));
//
//    Backend `source_id` for cluster backends is
//    `"<cluster_id>/<node_id>"`, matching the event payload.
//
// 6. README.md / TODO.md -- suggested lines:
//
//        - Load Balancer (`load_balancer`): balancers over provider,
//          cluster, or plain-address backends; round-robin,
//          least-connections, weighted, and IP-hash strategies;
//          HTTP/TCP health checks with automatic failover and a
//          degraded 503 state; TLS termination through the shared
//          certificate storage.
//        - TODO: persist balancers/backends/stats in PostgreSQL
//          (migration + schema in the integrator notes of
//          `src/modules/load_balancer.rs`); attach the live
//          `SslStorage` handle to `AppContext` for the TLS endpoint;
//          bind per-balancer listeners in `http::server` for real
//          traffic proxying (selection, health, and stats already
//          live in the module).
