//! BI external module (split from lib.rs; moved verbatim).

use super::*;


// ---------------------------------------------------------------------------
// Module implementation
// ---------------------------------------------------------------------------

/// The BI module. One struct holding the shared in-memory state.
pub struct BiModule {
    state: Arc<BiState>,
}

impl BiModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(BiState::new()),
        }
    }
}

impl Default for BiModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for BiModule {
    fn id(&self) -> &'static str {
        "bi"
    }

    fn name(&self) -> &'static str {
        "BI / Data-Warehouse Connector"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn description(&self) -> &'static str {
        "BI / data-warehouse connector: dataset definitions, hook-driven extracts, immutable snapshots, snapshot queries, webhook/file destinations, CSV/JSONL exports, and cron schedules. Per-tenant, in-memory."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &[]
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Seed provider: serves this module's own extract-run history
        // to any dataset whose source is "bi.extract_runs".
        let seed_state = Arc::clone(&self.state);
        bus.register(
            events::DATASET_PROVIDE,
            "bi",
            Arc::new(move |ctx: &mut HookContext| {
                let source = ctx.get("source").and_then(Value::as_str).unwrap_or("");
                if source != SEED_SOURCE {
                    return;
                }
                let tenant = ctx
                    .get("tenant_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let rows = seed_state.seed_history_rows(tenant);
                ctx.add_result(json!({
                    "provider": "bi.seed.extract_runs",
                    "rows": rows,
                }));
            }),
        );

        // Schedule tick: evaluate due schedules. File destinations
        // deliver synchronously; webhook deliveries are spawned when
        // the tick runs inside an async runtime, otherwise recorded
        // as deferred (use run-now for a guaranteed delivery).
        let tick_state = Arc::clone(&self.state);
        bus.register(
            events::SCHEDULE_TICK,
            "bi",
            Arc::new(move |ctx: &mut HookContext| {
                let now = Utc::now();
                let due: Vec<Schedule> = match tick_state.schedules.read() {
                    Ok(schedules) => schedules
                        .values()
                        .filter(|schedule| schedule.enabled && cron_matches(&schedule.cron, now))
                        .cloned()
                        .collect(),
                    Err(_) => Vec::new(),
                };
                let mut ran: Vec<Value> = Vec::new();
                for schedule in due {
                    match tick_state.run_extract(&schedule.tenant_id, &schedule.dataset_id, "schedule") {
                        Ok(run) => {
                            if let Ok(mut schedules) = tick_state.schedules.write() {
                                if let Some(entry) = schedules.get_mut(&schedule.id) {
                                    entry.last_run_at = Some(now.to_rfc3339());
                                }
                            }
                            if let Some(destination_id) = &schedule.destination_id {
                                if let Ok(destination) = tick_state
                                    .destination_for(&schedule.tenant_id, destination_id)
                                {
                                    if let Some(snapshot) = tick_state
                                        .latest_snapshot(&schedule.tenant_id, &schedule.dataset_id)
                                    {
                                        match destination.kind.as_str() {
                                            "file" => {
                                                if let Ok(dataset) = tick_state.dataset_for(
                                                    &schedule.tenant_id,
                                                    &schedule.dataset_id,
                                                ) {
                                                    let attempt = tick_state.deliver_file(
                                                        &destination,
                                                        &dataset,
                                                        &snapshot,
                                                    );
                                                    tick_state.record_delivery(attempt);
                                                }
                                            }
                                            _ => {
                                                if tokio::runtime::Handle::try_current().is_ok() {
                                                    let state = Arc::clone(&tick_state);
                                                    let tenant = schedule.tenant_id.clone();
                                                    let dataset_id = schedule.dataset_id.clone();
                                                    let destination_id = destination_id.clone();
                                                    let snapshot_id = snapshot.id.clone();
                                                    tokio::task::spawn(async move {
                                                        let _ = state
                                                            .deliver(
                                                                &tenant,
                                                                &destination_id,
                                                                &dataset_id,
                                                                &snapshot_id,
                                                            )
                                                            .await;
                                                    });
                                                } else {
                                                    tick_state.record_delivery(DeliveryAttempt {
                                                        id: new_id(),
                                                        tenant_id: schedule.tenant_id.clone(),
                                                        destination_id: destination_id.clone(),
                                                        dataset_id: Some(schedule.dataset_id.clone()),
                                                        snapshot_id: Some(snapshot.id.clone()),
                                                        status: "deferred".to_string(),
                                                        status_code: None,
                                                        latency_ms: None,
                                                        error: Some(
                                                            "schedule tick ran outside an async runtime; webhook delivery deferred, use run-now"
                                                                .to_string(),
                                                        ),
                                                        attempted_at: now.to_rfc3339(),
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            ran.push(json!({
                                "schedule_id": schedule.id,
                                "dataset_id": schedule.dataset_id,
                                "status": run.status,
                                "rows_accepted": run.rows_accepted,
                                "rows_rejected": run.rows_rejected,
                            }));
                        }
                        Err(error) => {
                            ran.push(json!({
                                "schedule_id": schedule.id,
                                "dataset_id": schedule.dataset_id,
                                "status": "failed",
                                "error": error.to_string(),
                            }));
                        }
                    }
                }
                ctx.add_result(json!({
                    "tick": now.to_rfc3339(),
                    "ran": ran,
                }));
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(Arc::clone(&self.state)));
        cfg.route("/api/v1/bi/datasets", web::post().to(create_dataset));
        cfg.route("/api/v1/bi/datasets", web::get().to(list_datasets));
        cfg.route("/api/v1/bi/datasets/{id}", web::get().to(get_dataset));
        cfg.route("/api/v1/bi/datasets/{id}", web::put().to(update_dataset));
        cfg.route("/api/v1/bi/datasets/{id}", web::delete().to(delete_dataset));
        cfg.route(
            "/api/v1/bi/datasets/{id}/extract",
            web::post().to(run_extract_handler),
        );
        cfg.route(
            "/api/v1/bi/datasets/{id}/extracts",
            web::get().to(list_extracts),
        );
        cfg.route(
            "/api/v1/bi/datasets/{id}/snapshots",
            web::get().to(list_snapshots),
        );
        cfg.route(
            "/api/v1/bi/datasets/{id}/query",
            web::post().to(query_dataset),
        );
        cfg.route(
            "/api/v1/bi/datasets/{id}/export",
            web::get().to(export_dataset),
        );
        cfg.route("/api/v1/bi/destinations", web::post().to(create_destination));
        cfg.route("/api/v1/bi/destinations", web::get().to(list_destinations));
        cfg.route(
            "/api/v1/bi/destinations/{id}",
            web::get().to(get_destination),
        );
        cfg.route(
            "/api/v1/bi/destinations/{id}",
            web::delete().to(delete_destination),
        );
        cfg.route(
            "/api/v1/bi/destinations/{id}/test",
            web::post().to(test_destination),
        );
        cfg.route("/api/v1/bi/deliveries", web::get().to(list_deliveries));
        cfg.route("/api/v1/bi/schedules", web::post().to(create_schedule));
        cfg.route("/api/v1/bi/schedules", web::get().to(list_schedules));
        cfg.route("/api/v1/bi/schedules/{id}", web::get().to(get_schedule));
        cfg.route(
            "/api/v1/bi/schedules/{id}",
            web::delete().to(delete_schedule),
        );
        cfg.route(
            "/api/v1/bi/schedules/{id}/run-now",
            web::post().to(run_schedule_now),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "POST", path: "/api/v1/bi/datasets", owner: "bi", description: "Create a dataset definition.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/datasets", owner: "bi", description: "List datasets for the tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/datasets/{id}", owner: "bi", description: "Get one dataset.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/bi/datasets/{id}", owner: "bi", description: "Update a dataset; changing fields bumps the schema version.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/bi/datasets/{id}", owner: "bi", description: "Delete a dataset and its runs, snapshots, and schedules.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/bi/datasets/{id}/extract", owner: "bi", description: "Run an extract now, optionally delivering to a destination.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/datasets/{id}/extracts", owner: "bi", description: "List extract runs for a dataset.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/datasets/{id}/snapshots", owner: "bi", description: "List immutable snapshots for a dataset.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/bi/datasets/{id}/query", owner: "bi", description: "Query the latest (or a chosen) snapshot: filters, grouping, aggregations, sorting, limit.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/datasets/{id}/export", owner: "bi", description: "Export a snapshot as CSV or JSONL (raw file bytes).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/bi/destinations", owner: "bi", description: "Create a webhook or file destination.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/destinations", owner: "bi", description: "List destinations for the tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/destinations/{id}", owner: "bi", description: "Get one destination.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/bi/destinations/{id}", owner: "bi", description: "Delete a destination.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/bi/destinations/{id}/test", owner: "bi", description: "Dry-run a destination with a 1-row sample.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/deliveries", owner: "bi", description: "List delivery attempts for the tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/bi/schedules", owner: "bi", description: "Create a cron schedule for a dataset.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/schedules", owner: "bi", description: "List schedules for the tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/schedules/{id}", owner: "bi", description: "Get one schedule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/bi/schedules/{id}", owner: "bi", description: "Delete a schedule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/bi/schedules/{id}/run-now", owner: "bi", description: "Run a schedule immediately (extract + optional delivery).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        match self.state.hooks.write() {
            Ok(mut hooks) => {
                *hooks = Some(Arc::clone(&ctx.hooks));
                Ok(())
            }
            Err(_) => Err(UnifiedError::internal("bi state lock poisoned")),
        }
    }
}

/// The one function the server calls. Same shape as every built-in
/// module's `register` function.
///
/// Wiring (done by the server, not by this crate): the root
/// `Cargo.toml` gains `external-bi = { path = "crates/external-bi" }`
/// under `[workspace] members` and under `[dependencies]`, and
/// `src/modules/mod.rs::register_all` gains one line in its
/// external-modules section: `external_bi::register(registry);`
pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(BiModule::new()));
}

/// Convenience accessor for tests and for embedding.
pub fn module() -> Arc<dyn Module> {
    Arc::new(BiModule::new())
}
