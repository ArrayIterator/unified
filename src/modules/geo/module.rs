//! geo - module.

// GEO module - Geo location submodule of the Network module.
//
// Resolution order (config `geo.settings_override` wins by default):
//   1. Settings overrides - CIDR / IP-range matches, IPv4 and IPv6.
//   2. MaxMind / GeoLite database file (`geo.database_path`) via maxminddb.
//   3. Otherwise `{ country: null, source: "unknown" }`.
//
// Every lookup updates the global `AppContext` Geo variable and emits the
// `geo.lookup` hook event so other modules (security, audit, network) can
// react without calling this module directly.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::web;
use serde_json::Value;
use std::sync::Arc;
use super::{store::*, cidr::*, classification::*, lookup::*, db::*, types::*, handlers::*};



// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------
pub struct GeoModule;


impl Module for GeoModule {
    fn id(&self) -> &'static str {
        "geo"
    }

    fn name(&self) -> &'static str {
        "Geo Location"
    }

    fn description(&self) -> &'static str {
        "Geo location lookups: settings CIDR overrides, MaxMind/GeoLite database, ASN checks, and database downloads."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["network"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let _ = GEO_CTX.set(Arc::new(AppContext::new(
            ctx.config.read().expect("config lock poisoned").clone(),
            Arc::clone(&ctx.hooks),
            Arc::clone(&ctx.modules),
        )));
        // Keep the stashed context in sync with the live one by storing the
        // real context when possible: the hook handler only needs config
        // and the override store, both of which are reachable here.
        Ok(())
    }

    fn register_hooks(&self, bus: &unified_core::hooks::HookBus) {
        // Synchronous `geo.lookup` handler: other modules emit this event
        // with `{ "ip": "..." }` and read the first result. The lookup
        // itself is synchronous (in-memory overrides + maxminddb file).
        bus.register(
            events::GEO_LOOKUP,
            "geo",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                let Some(ip) = context
                    .get("ip")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                else {
                    return;
                };
                let result = match GEO_CTX.get() {
                    Some(ctx) => lookup_geo(ctx, &ip),
                    None => Err(UnifiedError::internal("Geo module context is not initialised")),
                };
                match result {
                    Ok(info) => {
                        context.add_result(
                            serde_json::to_value(&info).unwrap_or(Value::Null),
                        );
                    }
                    Err(error) => {
                        context.add_result(serde_json::json!({ "error": error.message }));
                    }
                }
            }),
        );

        // `geo.update_requested` (from the Scheduler's `geo.update` job):
        // run the existing database download on a worker thread with its
        // own runtime - the download takes minutes and hook handlers are
        // synchronous - and emit `geo.update` on success.
        bus.register(
            "geo.update_requested",
            "geo",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                let url = context
                    .get("url")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        context
                            .get("download_url")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    });
                let Some(app_ctx) = GEO_CTX.get().cloned() else {
                    context.add_result(serde_json::json!({
                        "started": false,
                        "error": "Geo module context is not initialised",
                    }));
                    return;
                };
                std::thread::spawn(move || {
                    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    else {
                        log::error!("Geo update: cannot start async runtime");
                        return;
                    };
                    match runtime.block_on(download_database(&app_ctx, url)) {
                        Ok(result) => {
                            log::info!("Geo database updated: {result}");
                            app_ctx.hooks.emit(
                                "geo.update",
                                serde_json::json!({ "updated": true, "result": result }),
                            );
                        }
                        Err(error) => {
                            log::error!("Geo database update failed: {}", error.message);
                        }
                    }
                });
                context.add_result(serde_json::json!({ "started": true }));
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/geo/lookup", owner: "geo", description: "Lookup handler", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/geo/overrides", owner: "geo", description: "List overrides", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/geo/overrides", owner: "geo", description: "Create override", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/geo/overrides/{id}", owner: "geo", description: "Get override", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/geo/overrides/{id}", owner: "geo", description: "Update override", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/geo/overrides/{id}", owner: "geo", description: "Delete override", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/geo/asn-check", owner: "geo", description: "Asn check handler", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/geo/download", owner: "geo", description: "Download handler", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/geo/settings", owner: "geo", description: "Settings handler", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/geo")
                .route("/lookup", web::get().to(lookup_handler))
                .route("/overrides", web::get().to(list_overrides))
                .route("/overrides", web::post().to(create_override))
                .route("/overrides/{id}", web::get().to(get_override))
                .route("/overrides/{id}", web::put().to(update_override))
                .route("/overrides/{id}", web::delete().to(delete_override))
                .route("/asn-check", web::post().to(asn_check_handler))
                .route("/download", web::post().to(download_handler))
                .route("/settings", web::get().to(settings_handler)),
        );
    }
}


pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(GeoModule));
}
