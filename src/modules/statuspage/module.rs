//! Module registration: struct, Module impl and register().

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;
// Status Page module.
//
// A PUBLIC status page for reads, authenticated for writes: the
// GET summary / page / component list / uptime history / incident
// list routes need no authentication, but every write route
// (create/update/delete components, record uptime, create/update/
// resolve incidents, add incident updates) requires the caller to
// identify with `X-User-Id` + `X-Role` and hold the
// `statuspage.manage` permission, verified through the `role.check`
// hook (see `require_statuspage_manage`). The application has no
// global auth gate on module routes, so the check lives in each
// write handler.
//
// Overall status is derived, in priority order, from:
//   1. the global maintenance flag (`AppContext::is_maintenance`,
//      set by the Maintenance Windows module while a window is
//      active) -> `maintenance`;
//   2. open incidents: a critical incident -> `outage`, a major or
//      minor open incident -> `degraded`;
//   3. component states: a `major_outage` component -> `outage`, a
//      `partial_outage` or `degraded` component -> `degraded`;
//   4. otherwise `operational`.
//
// Uptime percentages come from daily aggregates recorded per
// component. Aggregates are recorded through
// `POST /components/{id}/uptime`; open incidents also add downtime
// minutes to their affected components automatically when they are
// created and resolved, so uptime reflects real incident time even
// when nobody posts aggregates by hand. A component with no recorded
// days in the range reports 100.0 with `data_points: 0`.
//
// The HTML page (`GET /status`) is a clean standalone page rendered
// by this module. The Handlebars theme engine does not fit cleanly
// here: no `statuspage` template exists in the embedded default
// theme, the engine is not reachable from `AppContext`, and adding a
// template would require editing theme files outside this module's
// scope. The JSON endpoint carries the same data, so a theme can
// render it later without changing this module.
//
// Events emitted on the hook bus:
//   - `statuspage.incident.created`
//   - `statuspage.incident.updated`
//   - `statuspage.incident.resolved`
//   - `statuspage.status.changed` (when the derived overall status changes)


// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------
//
// Note: this module registers no hook handlers. The overall status is
// recomputed with the application context on every request
// (`refresh_overall`), because a hook handler receives no AppContext
// and therefore cannot read the maintenance flag. Registering a
// no-op handler for `app.maintenance` would only pretend to react.

pub struct StatusPageModule {
    state: Arc<StatusPageState>,
}

impl StatusPageModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(StatusPageState::new()),
        }
    }
}

impl Default for StatusPageModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for StatusPageModule {
    fn id(&self) -> &'static str {
        "statuspage"
    }

    fn name(&self) -> &'static str {
        "Status Page"
    }

    fn description(&self) -> &'static str {
        "Public status page: derived overall status, components with 30/90-day uptime, incidents with timeline updates, and daily uptime history."
    }

    fn requires(&self) -> &[&'static str] {
        &["service-monitor", "maintenance"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("statuspage", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        // PUBLIC: the HTML status page. Registered outside /api so it
        // stays reachable (and bookmarkable) during maintenance; the
        // server integrator must exempt /status and
        // /api/v1/statuspage from the maintenance 503 block (see the
        // coordinator report snippet).
        cfg.route("/status", web::get().to(public_page));
        cfg.service(
            web::scope("/api/v1/statuspage")
                .route("", web::get().to(public_summary))
                .route("/components", web::get().to(list_components))
                .route("/components", web::post().to(create_component))
                .route("/components/{id}", web::get().to(get_component))
                .route("/components/{id}", web::put().to(update_component))
                .route("/components/{id}", web::delete().to(delete_component))
                .route("/components/{id}/uptime", web::post().to(record_uptime))
                .route("/uptime", web::get().to(uptime_history))
                .route("/incidents", web::get().to(list_incidents))
                .route("/incidents", web::post().to(create_incident))
                .route("/incidents/{id}", web::get().to(get_incident))
                .route("/incidents/{id}", web::put().to(update_incident))
                .route("/incidents/{id}/resolve", web::post().to(resolve_incident))
                .route("/incidents/{id}/updates", web::get().to(list_incident_updates))
                .route("/incidents/{id}/updates", web::post().to(add_incident_update)),
        );
    }

    fn route_info(&self) -> Vec<unified_core::modules::RouteInfo> {
        use unified_core::modules::RouteInfo;
        vec![
            RouteInfo { method: "GET", path: "/status", owner: "statuspage", description: "PUBLIC: status page as HTML.", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/statuspage", owner: "statuspage", description: "PUBLIC: overall status summary as JSON.", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/statuspage/components", owner: "statuspage", description: "PUBLIC: components with 30/90-day uptime (paginated).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/statuspage/components", owner: "statuspage", description: "Create a component. Requires statuspage.manage.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/statuspage/components/{id}", owner: "statuspage", description: "Get one component with uptime.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/statuspage/components/{id}", owner: "statuspage", description: "Update a component. Requires statuspage.manage.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/statuspage/components/{id}", owner: "statuspage", description: "Delete a component and its uptime records. Requires statuspage.manage.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/statuspage/components/{id}/uptime", owner: "statuspage", description: "Record a daily uptime aggregate for a component. Requires statuspage.manage.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/statuspage/uptime", owner: "statuspage", description: "PUBLIC: daily uptime history (paginated).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/statuspage/incidents", owner: "statuspage", description: "PUBLIC: incident history (paginated).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/statuspage/incidents", owner: "statuspage", description: "Create an incident. Requires statuspage.manage.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/statuspage/incidents/{id}", owner: "statuspage", description: "Get one incident.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/statuspage/incidents/{id}", owner: "statuspage", description: "Update an incident. Requires statuspage.manage.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/statuspage/incidents/{id}/resolve", owner: "statuspage", description: "Resolve an incident. Requires statuspage.manage.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/statuspage/incidents/{id}/updates", owner: "statuspage", description: "PUBLIC: incident timeline updates (paginated).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/statuspage/incidents/{id}/updates", owner: "statuspage", description: "Add a timeline update to an incident. Requires statuspage.manage.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(StatusPageModule::new()));
}
