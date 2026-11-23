// clusters — Module registration: struct, defaults, routes, and hooks.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct ClustersModule {
    state: Arc<ClustersState>,
}

impl ClustersModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(ClustersState::new()),
        }
    }
}

impl Default for ClustersModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ClustersModule {
    fn id(&self) -> &'static str {
        "clusters"
    }

    fn name(&self) -> &'static str {
        "Multi-Cluster"
    }

    fn description(&self) -> &'static str {
        "Multi-cluster management: node registration with HMAC authentication, health probing, and mutual sync with last-write-wins conflict handling, visible conflicts with admin resolution, a retry queue with dead-letter handling, idempotent admin writes, and admin-editable sync settings."
    }

    fn requires(&self) -> &[&'static str] {
        &["scheduler", "notifications"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["reqwest", "hmac", "sha2"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("clusters", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            CLUSTER_HEALTH_CHECK_REQUESTED,
            "clusters",
            Arc::new(move |ctx: &mut HookContext| {
                on_health_check_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            CLUSTER_SYNC_REQUESTED,
            "clusters",
            Arc::new(move |ctx: &mut HookContext| on_sync_requested(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            CLUSTER_SYNC_RETRY_DUE,
            "clusters",
            Arc::new(move |ctx: &mut HookContext| on_sync_retry_due(state.clone(), ctx)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/clusters/health", owner: "clusters", description: "Liveness probe other nodes call when health-checking this instance.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/clusters", owner: "clusters", description: "List clusters (paginated, filter by kind/status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/clusters/clusters", owner: "clusters", description: "Create a cluster.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/clusters/{id}", owner: "clusters", description: "Get one cluster.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/clusters/clusters/{id}", owner: "clusters", description: "Update a cluster.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/clusters/clusters/{id}", owner: "clusters", description: "Delete a cluster with its nodes and sync plans.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/clusters/{id}/sync-status", owner: "clusters", description: "Per-cluster sync state: last sync, pending items, conflicts, failures.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/clusters/clusters/{id}/nodes", owner: "clusters", description: "Register a node; returns the node secret exactly once.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/clusters/{id}/nodes", owner: "clusters", description: "List nodes of a cluster (paginated, filter by role/health/status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/nodes/{id}", owner: "clusters", description: "Get one node (secret hash never returned).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/clusters/nodes/{id}/revoke", owner: "clusters", description: "Revoke a node's credentials.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/clusters/nodes/{id}", owner: "clusters", description: "Remove a node and its sync plans.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/clusters/nodes/{id}/rotate-secret", owner: "clusters", description: "Rotate a node secret; returns the new secret exactly once.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/clusters/nodes/{id}/health-check", owner: "clusters", description: "Probe a node now and record the result.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/nodes/{id}/health", owner: "clusters", description: "Health-check history of a node (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/plans", owner: "clusters", description: "List sync plans (paginated, filter by cluster/direction/enabled).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/plans", owner: "clusters", description: "Create a sync plan (peer secret is hashed, never stored).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/plans/{id}", owner: "clusters", description: "Get one sync plan.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/clusters/sync/plans/{id}", owner: "clusters", description: "Update a sync plan.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/clusters/sync/plans/{id}", owner: "clusters", description: "Delete a sync plan.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/plans/{id}/sync", owner: "clusters", description: "Run a sync plan now; body {dry_run} previews without applying. Accepts an Idempotency-Key header.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/health", owner: "clusters", description: "Aggregate sync health: node health counts, failing plans, open conflicts, pending retries and dead-letters.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/conflicts", owner: "clusters", description: "List sync conflicts (paginated, filter by cluster/plan/domain/status/winner).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/conflicts/{id}", owner: "clusters", description: "Get one sync conflict with both payloads.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/conflicts/{id}/resolve", owner: "clusters", description: "Resolve a conflict: keep_local, keep_remote or dismiss. The chosen payload is applied as the newest local version.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/retries", owner: "clusters", description: "List failed-sync retry queue entries (paginated, filter by cluster/plan/domain/status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/retries/{id}", owner: "clusters", description: "Get one retry queue entry.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/retries/{id}/retry", owner: "clusters", description: "Re-run the sync plan behind a retry entry now (works for pending and dead-letter entries).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/retries/{id}/discard", owner: "clusters", description: "Discard a retry queue entry so it is never retried.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/retries/process-due", owner: "clusters", description: "Queue a background run of every retry entry whose next_retry_at has passed.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/settings", owner: "clusters", description: "Read the live sync settings (timeouts, thresholds, retry backoff, idempotency TTL).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/clusters/sync/settings", owner: "clusters", description: "Update the live sync settings (admin-editable; seeded from configurable defaults).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/domains", owner: "clusters", description: "List the registered sync domains.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/domains", owner: "clusters", description: "Register a new sync domain.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/clusters/sync/domains/{domain}", owner: "clusters", description: "Unregister a sync domain (refused while a plan uses it, and for the last remaining domain).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/logs", owner: "clusters", description: "List sync log entries (paginated, filter by cluster/plan/status/domain/direction).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/logs/{id}", owner: "clusters", description: "Get one sync log entry with its conflicts.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/receive", owner: "clusters", description: "Node-to-node sync receive (HMAC-signed, versioned envelope).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/export", owner: "clusters", description: "Node-to-node sync export (HMAC-signed, domain + since cursor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/clusters/sync/local-items", owner: "clusters", description: "Stage one local domain item for sync.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/clusters/sync/local-items", owner: "clusters", description: "List local domain items (paginated, filter by domain).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/clusters")
                .route("/health", web::get().to(peer_health))
                .route("/clusters", web::get().to(list_clusters))
                .route("/clusters", web::post().to(create_cluster))
                .route("/clusters/{id}", web::get().to(get_cluster))
                .route("/clusters/{id}", web::put().to(update_cluster))
                .route("/clusters/{id}", web::delete().to(delete_cluster))
                .route("/clusters/{id}/sync-status", web::get().to(cluster_sync_status))
                .route("/clusters/{id}/nodes", web::post().to(register_node))
                .route("/clusters/{id}/nodes", web::get().to(list_nodes))
                .route("/nodes/{id}", web::get().to(get_node))
                .route("/nodes/{id}/revoke", web::post().to(revoke_node))
                .route("/nodes/{id}", web::delete().to(remove_node))
                .route("/nodes/{id}/rotate-secret", web::post().to(rotate_node_secret))
                .route("/nodes/{id}/health-check", web::post().to(health_check_now))
                .route("/nodes/{id}/health", web::get().to(node_health_history))
                .route("/sync/plans", web::get().to(list_plans))
                .route("/sync/plans", web::post().to(create_plan))
                .route("/sync/plans/{id}", web::get().to(get_plan))
                .route("/sync/plans/{id}", web::put().to(update_plan))
                .route("/sync/plans/{id}", web::delete().to(delete_plan))
                .route("/sync/plans/{id}/sync", web::post().to(sync_now))
                .route("/sync/health", web::get().to(sync_health_overview))
                .route("/sync/conflicts", web::get().to(list_conflicts))
                .route("/sync/conflicts/{id}", web::get().to(get_conflict))
                .route("/sync/conflicts/{id}/resolve", web::post().to(resolve_conflict))
                .route("/sync/retries", web::get().to(list_retries))
                .route("/sync/retries/{id}", web::get().to(get_retry))
                .route("/sync/retries/{id}/retry", web::post().to(retry_entry_now))
                .route("/sync/retries/{id}/discard", web::post().to(discard_retry))
                .route("/sync/retries/process-due", web::post().to(process_due_retries_endpoint))
                .route("/sync/settings", web::get().to(get_settings))
                .route("/sync/settings", web::put().to(update_settings))
                .route("/sync/domains", web::get().to(list_domains))
                .route("/sync/domains", web::post().to(register_domain))
                .route("/sync/domains/{domain}", web::delete().to(unregister_domain))
                .route("/sync/logs", web::get().to(list_sync_logs))
                .route("/sync/logs/{id}", web::get().to(get_sync_log))
                .route("/sync/receive", web::post().to(sync_receive))
                .route("/sync/export", web::get().to(sync_export))
                .route("/sync/local-items", web::post().to(stage_local_item))
                .route("/sync/local-items", web::get().to(list_local_items)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ClustersModule::new()));
}
