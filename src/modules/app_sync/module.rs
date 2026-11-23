//! app_sync - module.

// App Sync module - application connection and sync for desktop apps.
//
// An authenticated caller holding `app-sync:manage` registers a device
// and receives an HMAC-signed sync token; the device is bound to that
// caller's identity. Every sync call validates the token against the
// server secret (`app.secret_key`) before touching any data. Sync state
// is kept in memory per device: each push appends a versioned record (a
// monotonic per-device version plus an `updated_at` timestamp), and pull
// returns every record newer than the client's cursor - either a version
// number or an RFC 3339 timestamp. The first successful sync emits the
// `sync.connected` hook event.

use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::web;
use std::sync::Arc;
use super::{constants::*, helpers::*, types::*, tokens::*, inputs::*, handlers::*};



// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------
pub struct AppSyncModule;


impl Module for AppSyncModule {
    fn id(&self) -> &'static str {
        "app-sync"
    }

    fn name(&self) -> &'static str {
        "Application Connection & Sync"
    }

    fn description(&self) -> &'static str {
        "Desktop-app integration: HMAC-signed device tokens and versioned push/pull sync per device. Push accepts an optional idempotency_key so a retried push returns the original record instead of appending a duplicate."
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "POST", path: "/api/v1/app-sync/devices/register", owner: "app-sync", description: "Register device (requires app-sync:manage; device is bound to the caller)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/app-sync/devices", owner: "app-sync", description: "List devices", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/app-sync/devices/{id}", owner: "app-sync", description: "Get device", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/app-sync/devices/{id}/revoke", owner: "app-sync", description: "Revoke device", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/app-sync/sync/push", owner: "app-sync", description: "Sync push (optional idempotency_key in the body; a replay returns the original record with HTTP 200).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/app-sync/sync/pull", owner: "app-sync", description: "Sync pull", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/app-sync")
                .route("/devices/register", web::post().to(register_device))
                .route("/devices", web::get().to(list_devices))
                .route("/devices/{id}", web::get().to(get_device))
                .route("/devices/{id}/revoke", web::post().to(revoke_device))
                .route("/sync/push", web::post().to(sync_push))
                .route("/sync/pull", web::post().to(sync_pull)),
        );
    }
}


pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AppSyncModule));
}
