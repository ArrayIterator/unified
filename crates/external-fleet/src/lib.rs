// Fleet and vehicle management external module for the Unified server.
//
// Vehicles, drivers, trips, GPS positions, fuel logs, maintenance
// schedules and cost rollups. All records are per-tenant and the
// store is in-memory; see README.md and
// docs/WORKSPACE_AND_EXTERNAL_MODULES.md (sections 3 and 4) for the
// full contract.
//
// The crate depends only on `unified-core` (plus actix-web, chrono,
// dashmap, regex, serde and uuid for its own handlers and store). It
// never imports the server crate, other modules, the HTTP layer, the
// CLI, or the gateway.

mod handlers;
mod state;
mod validation;

use actix_web::web;
use serde_json::json;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};

pub use state::{
    Driver, DriverView, FleetState, FuelLog, GpsPosition, MaintenanceItem, Trip, Vehicle,
    EVENT_MAINTENANCE_DUE, EVENT_TRIP_COMPLETED, EVENT_VEHICLE_RETIRED,
};

/// The module. Holds the fleet store; one instance per server.
pub struct FleetModule {
    state: Arc<FleetState>,
}

impl FleetModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(FleetState::new()),
        }
    }
}

impl Default for FleetModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for FleetModule {
    fn id(&self) -> &'static str {
        "fleet"
    }

    fn name(&self) -> &'static str {
        "Fleet"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn description(&self) -> &'static str {
        "Fleet and vehicle management: vehicles, drivers, trips, GPS positions, fuel logs, maintenance schedules, and cost rollups. Per-tenant in-memory store."
    }

    /// External modules must return `false`. The default is `true`,
    /// and a module that leaves the default is treated as a core
    /// module: the Extensions Manager refuses to disable it.
    fn is_core(&self) -> bool {
        false
    }

    /// This module needs nothing else, so it declares no requirements.
    fn requires(&self) -> &[&'static str] {
        &[]
    }

    /// One hook listener: when the application starts, contribute a
    /// result to the `app.start` event.
    ///
    /// This module subscribes to no external events. The task brief
    /// suggested listening to `logistics.*`, but no such event names
    /// exist in `unified_core::hooks::events` (verified: the only
    /// well-known events are the 26 core ones such as `app.start`,
    /// `auth.login`, `user.created`, `module.enabled`). Subscribing
    /// to an unverified name would silently never fire, so the
    /// module listens to nothing and only emits its own events:
    /// `fleet.maintenance.due`, `fleet.trip.completed`, and
    /// `fleet.vehicle.retired`.
    fn register_hooks(&self, bus: &HookBus) {
        bus.register(events::APP_START, "fleet", Arc::new(|ctx: &mut HookContext| {
            ctx.add_result(json!({
                "module": "fleet",
                "event": "started",
            }));
        }));
    }

    /// All routes live under `/api/v1/fleet/`.
    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        handlers::configure_routes(cfg, self.state.clone());
    }

    /// Metadata for every route `configure_routes` registers, so the
    /// route catalog (`ModuleRegistry::route_catalog`) lists this
    /// module. Paths and methods must match `configure_routes`.
    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/fleet/vehicle-types", owner: "fleet", description: "List this tenant's configurable vehicle types.", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "PUT", path: "/api/v1/fleet/vehicle-types", owner: "fleet", description: "Replace this tenant's configurable vehicle type list.", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "POST", path: "/api/v1/fleet/vehicles", owner: "fleet", description: "Create a vehicle (plate and VIN validated, unique per tenant).", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "GET", path: "/api/v1/fleet/vehicles", owner: "fleet", description: "List this tenant's vehicles.", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "GET", path: "/api/v1/fleet/vehicles/{id}", owner: "fleet", description: "Get one vehicle (tenant-scoped).", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "PUT", path: "/api/v1/fleet/vehicles/{id}", owner: "fleet", description: "Update a vehicle; status follows active -> maintenance -> retired (retired is terminal) and the odometer never moves backwards.", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "DELETE", path: "/api/v1/fleet/vehicles/{id}", owner: "fleet", description: "Delete a vehicle (refused while it has an active trip).", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "POST", path: "/api/v1/fleet/vehicles/{id}/positions", owner: "fleet", description: "Ingest one GPS fix (lat/lon/speed validated, timestamp at most 5 minutes in the future).", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "GET", path: "/api/v1/fleet/vehicles/{id}/positions", owner: "fleet", description: "Get a vehicle's GPS track (bounded to the last 1000 fixes).", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "GET", path: "/api/v1/fleet/vehicles/{id}/costs", owner: "fleet", description: "Cost rollup for one vehicle: fuel + maintenance + trips with a per-category breakdown.", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "POST", path: "/api/v1/fleet/drivers", owner: "fleet", description: "Create a driver (licence number unique per tenant).", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "GET", path: "/api/v1/fleet/drivers", owner: "fleet", description: "List this tenant's drivers (licence expiry status computed on read).", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "GET", path: "/api/v1/fleet/drivers/{id}", owner: "fleet", description: "Get one driver (tenant-scoped).", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "PUT", path: "/api/v1/fleet/drivers/{id}", owner: "fleet", description: "Update a driver.", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "DELETE", path: "/api/v1/fleet/drivers/{id}", owner: "fleet", description: "Delete a driver (refused while they have an active trip).", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "POST", path: "/api/v1/fleet/trips", owner: "fleet", description: "Create a trip: vehicle and driver must exist, be active, and have no overlapping trip.", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "GET", path: "/api/v1/fleet/trips", owner: "fleet", description: "List this tenant's trips.", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "GET", path: "/api/v1/fleet/trips/{id}", owner: "fleet", description: "Get one trip (tenant-scoped).", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "PATCH", path: "/api/v1/fleet/trips/{id}/status", owner: "fleet", description: "Move a trip planned -> in_progress -> completed | cancelled; completing requires an end odometer >= start odometer and records distance and duration.", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "POST", path: "/api/v1/fleet/fuel", owner: "fleet", description: "Log a fuel fill-up (litres > 0, odometer >= last recorded odometer).", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "GET", path: "/api/v1/fleet/fuel", owner: "fleet", description: "Fuel logs for one vehicle (?vehicle_id=) with per-interval and overall km/litre efficiency (null when fewer than two fill-ups).", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "POST", path: "/api/v1/fleet/maintenance", owner: "fleet", description: "Create a maintenance item due by odometer and/or date.", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "GET", path: "/api/v1/fleet/maintenance", owner: "fleet", description: "List this tenant's maintenance items.", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "GET", path: "/api/v1/fleet/maintenance/due", owner: "fleet", description: "Maintenance items due now (odometer reached OR date passed), computed from live vehicle data.", auth: unified_core::auth::RouteAuth::Authenticated },
            RouteInfo { method: "PATCH", path: "/api/v1/fleet/maintenance/{id}/complete", owner: "fleet", description: "Complete a maintenance item: records the cost and, when next due values are given, creates the follow-up pending item.", auth: unified_core::auth::RouteAuth::Authenticated },
        ]
    }

    /// Runs once at startup. Attaches the hook bus to the store so
    /// handlers can emit `fleet.*` events.
    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        self.state.set_hooks(ctx.hooks.clone());
        Ok(())
    }
}

/// The one function the server calls. Same shape as every built-in
/// module's `register` function.
///
/// Wiring (done by the server, not by this crate): the root
/// `Cargo.toml` gains `external-fleet = { path =
/// "crates/external-fleet" }` (and the crate is added to the
/// `[workspace] members` list), and
/// `src/modules/mod.rs::register_all` gains one line in its
/// external-modules section:
/// `external_fleet::register(registry);`
pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(FleetModule::new()));
}

/// Convenience accessor for tests and for embedding.
pub fn module() -> Arc<dyn Module> {
    Arc::new(FleetModule::new())
}
