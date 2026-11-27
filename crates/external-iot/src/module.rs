//! IoT external module.

use super::*;


// Module

/// The IoT device manager module.
pub struct IotModule {
    state: Arc<IotState>,
}

impl IotModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(IotState::new()),
        }
    }

    /// Shared state, for tests and for embedding.
    pub fn state(&self) -> Arc<IotState> {
        self.state.clone()
    }
}

impl Default for IotModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for IotModule {
    fn id(&self) -> &'static str {
        "iot"
    }

    fn name(&self) -> &'static str {
        "IoT Device Manager"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn description(&self) -> &'static str {
        "IoT device manager: per-tenant device registry with per-device API keys, HTTP telemetry ingestion with rate limits and aggregates, device twins, command queue, alert rules, and fleet health. In-memory store; no MQTT broker embedded."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &[]
    }

    /// No external listeners. The well-known core events in
    /// `unified_core::hooks::events` (app lifecycle, auth, user,
    /// module, SSL, geo, license, download, sync) contain no device
    /// or telemetry event this module could honestly consume, so
    /// nothing is registered here. This module only emits its own
    /// events (`iot.device.provisioned`, `iot.device.decommissioned`,
    /// `iot.alert.triggered`).
    fn register_hooks(&self, _bus: &HookBus) {}

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/iot")
                // Literal segments first: they must win over
                // `/devices/{id}`.
                .route("/device-types", web::get().to(list_device_types))
                .route("/device-types", web::post().to(create_device_type))
                .route("/health", web::get().to(fleet_health))
                .route("/alert-rules", web::get().to(list_alert_rules))
                .route("/alert-rules", web::post().to(create_alert_rule))
                .route("/alert-rules/{id}", web::delete().to(delete_alert_rule))
                .route("/alerts", web::get().to(list_alerts))
                .route("/devices", web::get().to(list_devices))
                .route("/devices", web::post().to(create_device))
                .route("/devices/{id}", web::get().to(get_device))
                .route("/devices/{id}", web::put().to(update_device))
                .route("/devices/{id}", web::delete().to(delete_device))
                .route("/devices/{id}/telemetry", web::post().to(ingest_telemetry))
                .route("/devices/{id}/telemetry", web::get().to(get_telemetry))
                .route("/devices/{id}/twin", web::get().to(get_twin))
                .route("/devices/{id}/twin", web::put().to(update_twin))
                .route(
                    "/devices/{id}/twin/reported",
                    web::post().to(report_twin),
                )
                .route("/devices/{id}/commands", web::post().to(queue_command))
                .route("/devices/{id}/commands", web::get().to(list_commands))
                .route(
                    "/devices/{id}/commands/{cmd}/ack",
                    web::post().to(ack_command),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/iot/device-types", owner: "iot", description: "List the configured device types.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/iot/device-types", owner: "iot", description: "Add a device type to the configured list.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/iot/health", owner: "iot", description: "Per-tenant fleet health: counts by status, silent devices, out-of-sync twins, open alerts.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/iot/alert-rules", owner: "iot", description: "List alert rules for a tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/iot/alert-rules", owner: "iot", description: "Create a metric threshold alert rule for a device or device type.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/iot/alert-rules/{id}", owner: "iot", description: "Delete an alert rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/iot/alerts", owner: "iot", description: "List alerts for a tenant, optionally filtered by device or severity.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/iot/devices", owner: "iot", description: "List devices for a tenant, optionally filtered by status or device type.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/iot/devices", owner: "iot", description: "Provision a device; returns the per-device API key exactly once.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/iot/devices/{id}", owner: "iot", description: "Get one device.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/iot/devices/{id}", owner: "iot", description: "Update a device, including status lifecycle transitions.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/iot/devices/{id}", owner: "iot", description: "Delete a device and its telemetry, twin, and commands.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/iot/devices/{id}/telemetry", owner: "iot", description: "Ingest one telemetry point, authenticated with the device API key; rate-limited to 60/minute per device.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/iot/devices/{id}/telemetry", owner: "iot", description: "Query stored telemetry points with min/max/avg/count aggregates.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/iot/devices/{id}/twin", owner: "iot", description: "Get the device twin (desired, reported, in_sync).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/iot/devices/{id}/twin", owner: "iot", description: "Update the twin desired state (admin side).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/iot/devices/{id}/twin/reported", owner: "iot", description: "Update the twin reported state from the device, authenticated with the device API key.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/iot/devices/{id}/commands", owner: "iot", description: "Queue a command for a device, with a TTL.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/iot/devices/{id}/commands", owner: "iot", description: "List queued commands for a device to poll, authenticated with the device API key.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/iot/devices/{id}/commands/{cmd}/ack", owner: "iot", description: "Acknowledge a command (acked/failed) with an optional result payload.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    /// Attach the hook bus so emitted events reach other modules.
    /// Runs after the core has built `AppContext`, before routes are
    /// served. The store itself needs no startup work.
    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        self.state.set_hooks(ctx.hooks.clone());
        Ok(())
    }
}

/// The one function the server calls. Same shape as every built-in
/// module's `register` function.
///
/// Wiring (done by the server, not by this crate): remove the
/// standalone `[workspace]` table from this crate's `Cargo.toml`,
/// add `crates/external-iot` to the root workspace `members`, add
/// `external-iot = { path = "crates/external-iot" }` to the root
/// `[dependencies]`, and add one line in the external-modules section
/// of `src/modules/mod.rs::register_all`:
/// `external_iot::register(registry);`
pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(IotModule::new()));
}

/// Convenience accessor for tests and for embedding.
pub fn module() -> Arc<dyn Module> {
    Arc::new(IotModule::new())
}
