//! Server integration: the `scripting` manager module, one
//! `TsModule` per TypeScript module, the dispatcher, and the admin
//! reload endpoints.
//!
//! How it fits together:
//!
//! * `ScriptingModule` (id `scripting`, not a core module) is
//!   registered like any external module (see `register` below and
//!   the one line in `src/modules/mod.rs`). Its `init` builds the
//!   host from the `AppContext`, starts the engine, loads every
//!   module in the modules directory, and registers one `TsModule`
//!   per loaded module plus the HookBus forwarders for its events.
//! * `TsModule` is a real `Module` in the registry, so a TypeScript
//!   module appears in the module manager and the route catalog
//!   exactly like a native module. Its `configure_routes` is a
//!   no-op: HTTP is served by the shared dispatcher below, and
//!   its routes are updated in place on reload (the registry
//!   refuses duplicate ids, so re-registering is impossible).
//! * The dispatcher (`/api/v1/ts/{module}/{tail:.*}`) looks the
//!   module up by id and hands the request to the engine. Route
//!   handlers run on the scripting worker thread via `web::block`,
//!   never on a tokio worker.
//!
//! Authentication: the dispatcher and the list endpoint declare
//! `RouteAuth::Authenticated` (enforced by the global auth
//! middleware once it lands — inspection C1); the reload
//! endpoints additionally require the `scripting:manage`
//! capability in the handler itself (`caller_from`), so they fail
//! closed (403) while the middleware is still being wired.

use actix_web::{HttpRequest, HttpResponse, web};
use futures_util::StreamExt;
use parking_lot::RwLock;
use serde_json::{Value as JsonValue, json};
use std::collections::HashMap;
use std::sync::Arc;
use unified_core::auth::{RouteAuth, caller_from};
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json::{error_response, success_response};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use uuid::Uuid;

use crate::engine::{RouteRequest, ScriptLimits, ScriptingEngine, TsModuleState};
use crate::host::{
    build_host, register_hook_forwarders, resolve_paths, unregister_hook_forwarders,
};
use crate::loader::{ModuleMeta, load_all, reload_one};

/// Maximum request body the dispatcher reads (10 MiB). The global
/// request-size middleware caps bodies earlier; this is the
/// dispatcher-local backstop.
pub const MAX_DISPATCH_BODY_BYTES: usize = 10 * 1024 * 1024;

/// The manager module: owns the engine and loads the TypeScript
/// modules at startup.
pub struct ScriptingModule {
    engine: RwLock<Option<Arc<ScriptingEngine>>>,
}

impl ScriptingModule {
    pub fn new() -> Self {
        Self {
            engine: RwLock::new(None),
        }
    }

    fn engine(&self) -> Option<Arc<ScriptingEngine>> {
        self.engine.read().clone()
    }
}

impl Module for ScriptingModule {
    fn id(&self) -> &'static str {
        "scripting"
    }

    fn name(&self) -> &'static str {
        "TypeScript Scripting"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn description(&self) -> &'static str {
        "TypeScript external-module runtime: QuickJS execution with the Unified global SDK."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        // `init` runs once at startup. A second call (config
        // reload re-running init) must not create a second engine
        // and orphan the first worker: the admin reload endpoints
        // are the way to refresh modules, not re-init.
        if self.engine.read().is_some() {
            return Ok(());
        }
        let paths = resolve_paths();
        let host = build_host(ctx, &paths);
        let engine = ScriptingEngine::new(host, ScriptLimits::default(), paths.modules_dir.clone());
        let reports = load_all(&engine, &paths.modules_dir);
        for report in &reports {
            match &report.summary {
                Some(summary) => {
                    let Some(state) = engine.ts_state(&summary.id) else {
                        continue;
                    };
                    let meta = ModuleMeta {
                        id: summary.id.clone(),
                        name: summary.name.clone(),
                        version: summary.version.clone(),
                        description: summary.description.clone(),
                        dir: paths.modules_dir.join(&summary.id),
                    };
                    // Skip when a module with this id is already
                    // registered (a previous init): re-registering
                    // panics, and the forwarders are already there.
                    if ctx.modules.get(&summary.id).is_none() {
                        ctx.modules.register(Arc::new(TsModule::new(meta, state)));
                        register_hook_forwarders(&engine, &ctx.hooks, &summary.id, &summary.hooks);
                    }
                    log::info!(
                        "Loaded TypeScript module {} v{} ({} routes, {} hooks)",
                        summary.id,
                        summary.version,
                        summary.routes.len(),
                        summary.hooks.len()
                    );
                }
                None => {
                    log::warn!(
                        "Failed to load TypeScript module {}: {}",
                        report.id,
                        report.error.as_deref().unwrap_or("unknown error")
                    );
                }
            }
        }
        *self.engine.write() = Some(engine);
        Ok(())
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        if let Some(engine) = self.engine() {
            cfg.app_data(web::Data::new(engine));
        }
        cfg.route(
            "/api/v1/ts/{module}/{tail:.*}",
            web::route().to(dispatch_ts_route),
        );
        cfg.route("/api/v1/ts/{module}", web::route().to(dispatch_ts_route));
        cfg.route("/api/v1/scripting/modules", web::get().to(list_ts_modules));
        cfg.route(
            "/api/v1/scripting/modules/{id}/reload",
            web::post().to(reload_ts_module),
        );
        cfg.route(
            "/api/v1/scripting/modules/reload",
            web::post().to(reload_all_ts_modules),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "*",
                path: "/api/v1/ts/{module}/{tail:.*}",
                owner: "scripting",
                description: "Dispatches a request to a TypeScript module route handler.",
                auth: RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/scripting/modules",
                owner: "scripting",
                description: "List loaded TypeScript modules.",
                auth: RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/scripting/modules/{id}/reload",
                owner: "scripting",
                description: "Reload one TypeScript module (re-transpile + re-run, no restart).",
                auth: RouteAuth::Capability("scripting:manage"),
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/scripting/modules/reload",
                owner: "scripting",
                description: "Reload all TypeScript modules (re-transpile + re-run, no restart).",
                auth: RouteAuth::Capability("scripting:manage"),
            },
        ]
    }
}

/// One TypeScript module as the `ModuleRegistry` sees it. The
/// `&'static str` metadata is leaked once per module on purpose:
/// the trait requires static strings and module metadata lives
/// for the process lifetime. Route entries are cached and rebuilt
/// only when the routes change (on reload), so a catalog read
/// never leaks.
pub struct TsModule {
    id: &'static str,
    name: &'static str,
    version: &'static str,
    description: &'static str,
    state: Arc<TsModuleState>,
    cached: RwLock<Option<(Vec<crate::sdk::RouteEntry>, Vec<RouteInfo>)>>,
}

impl TsModule {
    pub fn new(meta: ModuleMeta, state: Arc<TsModuleState>) -> Self {
        Self {
            id: Box::leak(meta.id.into_boxed_str()),
            name: Box::leak(meta.name.into_boxed_str()),
            version: Box::leak(meta.version.into_boxed_str()),
            description: Box::leak(meta.description.into_boxed_str()),
            state,
            cached: RwLock::new(None),
        }
    }
}

impl Module for TsModule {
    fn id(&self) -> &'static str {
        self.id
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn version(&self) -> &'static str {
        self.version
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn is_core(&self) -> bool {
        false
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        let routes = self.state.routes.read().clone();
        if let Some((cached_routes, cached_info)) = self.cached.read().as_ref() {
            if *cached_routes == routes {
                return cached_info.clone();
            }
        }
        let info: Vec<RouteInfo> = routes
            .iter()
            .map(|route| {
                let path = format!("/api/v1/ts/{}{}", self.id, route.pattern);
                RouteInfo {
                    method: Box::leak(route.method.clone().into_boxed_str()),
                    path: Box::leak(path.into_boxed_str()),
                    owner: self.id,
                    description: Box::leak(
                        format!(
                            "TypeScript module {} route {} {}",
                            self.id, route.method, route.pattern
                        )
                        .into_boxed_str(),
                    ),
                    auth: RouteAuth::Authenticated,
                }
            })
            .collect();
        *self.cached.write() = Some((routes, info.clone()));
        info
    }
}

/// The one function the server calls (see `src/modules/mod.rs`).
pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ScriptingModule::new()));
}

// ---------------------------------------------------------- handlers

fn engine_or_503(
    engine: &Option<web::Data<Arc<ScriptingEngine>>>,
) -> Result<Arc<ScriptingEngine>, UnifiedError> {
    engine
        .as_ref()
        // `Data<T>` derefs to `Arc<T>` (one level too many), so
        // `Arc::clone(data)` would clone the `Arc<Arc<...>>` and
        // return the wrong type (E0308). `get_ref()` reaches the
        // inner `Arc<ScriptingEngine>` directly. `into_inner`
        // takes `self` by value and cannot run on a borrow.
        .map(|data| Arc::clone(data.get_ref()))
        .ok_or_else(|| UnifiedError::internal("Scripting is not initialized"))
}

fn classify_dispatch_error(message: &str) -> UnifiedError {
    if message.starts_with("No such route") || message.starts_with("No loaded script module") {
        UnifiedError::not_found(message)
    } else if message.starts_with("Async route handlers") {
        UnifiedError::bad_request(message)
    } else {
        UnifiedError::internal(message)
    }
}

async fn dispatch_ts_route(
    req: HttpRequest,
    mut payload: web::Payload,
    engine: Option<web::Data<Arc<ScriptingEngine>>>,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let engine = match engine_or_503(&engine) {
        Ok(engine) => engine,
        Err(error) => return error_response(&error, debug),
    };
    let Some(module_id) = req.match_info().get("module").map(str::to_string) else {
        return error_response(&UnifiedError::not_found("No such script module"), debug);
    };
    let tail = req.match_info().get("tail").unwrap_or("");
    let path = format!("/{tail}");
    let method = req.method().as_str().to_string();
    let query: HashMap<String, String> =
        web::Query::<HashMap<String, String>>::from_query(req.query_string())
            .map(|query| query.into_inner())
            .unwrap_or_default();
    let mut headers: HashMap<String, String> = HashMap::new();
    for (name, value) in req.headers().iter() {
        headers.insert(
            name.as_str().to_string(),
            value.to_str().unwrap_or("").to_string(),
        );
    }
    let mut body_bytes = web::BytesMut::new();
    while let Some(chunk) = payload.next().await {
        match chunk {
            Ok(bytes) => {
                if body_bytes.len() + bytes.len() > MAX_DISPATCH_BODY_BYTES {
                    return error_response(
                        &UnifiedError::bad_request("Request body is too large"),
                        debug,
                    );
                }
                body_bytes.extend_from_slice(&bytes);
            }
            Err(_) => {
                return error_response(
                    &UnifiedError::bad_request("Cannot read request body"),
                    debug,
                );
            }
        }
    }
    let body: JsonValue = if body_bytes.is_empty() {
        JsonValue::Null
    } else {
        match serde_json::from_slice::<JsonValue>(&body_bytes) {
            Ok(json) => json,
            Err(_) => JsonValue::String(String::from_utf8_lossy(&body_bytes).into_owned()),
        }
    };
    let tenant = caller_from(&req)
        .tenant_id
        .as_deref()
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let request = RouteRequest {
        method: method.clone(),
        path: path.clone(),
        query,
        headers,
        body,
    };
    // Blocking engine call on the Actix blocking pool, never on a
    // tokio worker.
    let engine_clone = Arc::clone(&engine);
    let result = web::block(move || {
        engine_clone.dispatch_route(&module_id, &method, &path, request, tenant)
    })
    .await;
    match result {
        Ok(Ok(response)) => success_response(response.body, response.status),
        Ok(Err(message)) => error_response(&classify_dispatch_error(&message), debug),
        Err(_) => error_response(&UnifiedError::internal("Scripting worker failed"), debug),
    }
}

async fn list_ts_modules(
    req: HttpRequest,
    engine: Option<web::Data<Arc<ScriptingEngine>>>,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if !caller_from(&req).is_authenticated() {
        return error_response(
            &UnifiedError::unauthorized("Authentication is required"),
            debug,
        );
    }
    let engine = match engine_or_503(&engine) {
        Ok(engine) => engine,
        Err(error) => return error_response(&error, debug),
    };
    success_response(engine.list_modules(), 200)
}

async fn reload_ts_module(
    req: HttpRequest,
    engine: Option<web::Data<Arc<ScriptingEngine>>>,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if !caller_from(&req).has_scope("scripting:manage") {
        return error_response(
            &UnifiedError::forbidden("Requires the scripting:manage capability"),
            debug,
        );
    }
    let engine = match engine_or_503(&engine) {
        Ok(engine) => engine,
        Err(error) => return error_response(&error, debug),
    };
    let Some(id) = req.match_info().get("id").map(str::to_string) else {
        return error_response(&UnifiedError::not_found("No such script module"), debug);
    };
    let before = engine
        .list_modules()
        .into_iter()
        .find(|module| module.id == id)
        .map(|module| module.hooks)
        .unwrap_or_default();
    let modules_dir = engine.modules_dir().to_path_buf();
    let engine_clone = Arc::clone(&engine);
    // `id` is borrowed again below (unregister/register the hook
    // forwarders), so the `web::block` closure gets its own clone
    // instead of moving `id` (E0382).
    let reload_id = id.clone();
    let result = web::block(move || reload_one(&engine_clone, &modules_dir, &reload_id)).await;
    match result {
        Ok(Ok(summary)) => {
            // Hook subscriptions may have changed: drop the
            // forwarders for removed events, add them for new ones.
            // The dispatcher routes need no change (they resolve
            // by module id at request time).
            let bus: Arc<HookBus> = Arc::clone(&ctx.hooks);
            let removed: Vec<String> = before
                .iter()
                .filter(|event| !summary.hooks.contains(event))
                .cloned()
                .collect();
            let added: Vec<String> = summary
                .hooks
                .iter()
                .filter(|event| !before.contains(event))
                .cloned()
                .collect();
            unregister_hook_forwarders(&bus, &id, &removed);
            register_hook_forwarders(&engine, &bus, &id, &added);
            success_response(summary, 200)
        }
        Ok(Err(message)) => error_response(&UnifiedError::bad_request(message), debug),
        Err(_) => error_response(&UnifiedError::internal("Scripting worker failed"), debug),
    }
}

async fn reload_all_ts_modules(
    req: HttpRequest,
    engine: Option<web::Data<Arc<ScriptingEngine>>>,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if !caller_from(&req).has_scope("scripting:manage") {
        return error_response(
            &UnifiedError::forbidden("Requires the scripting:manage capability"),
            debug,
        );
    }
    let engine = match engine_or_503(&engine) {
        Ok(engine) => engine,
        Err(error) => return error_response(&error, debug),
    };
    let modules_dir = engine.modules_dir().to_path_buf();
    let engine_clone = Arc::clone(&engine);
    let result = web::block(move || {
        let mut reports: Vec<JsonValue> = Vec::new();
        for module in engine_clone.list_modules() {
            let id = module.id.clone();
            let before = module.hooks.clone();
            match reload_one(&engine_clone, &modules_dir, &id) {
                Ok(summary) => {
                    let bus: Arc<HookBus> = Arc::clone(&engine_clone.host().bus);
                    let removed: Vec<String> = before
                        .iter()
                        .filter(|event| !summary.hooks.contains(event))
                        .cloned()
                        .collect();
                    let added: Vec<String> = summary
                        .hooks
                        .iter()
                        .filter(|event| !before.contains(event))
                        .cloned()
                        .collect();
                    unregister_hook_forwarders(&bus, &id, &removed);
                    register_hook_forwarders(&engine_clone, &bus, &id, &added);
                    reports.push(json!({"id": id, "ok": true, "summary": summary}));
                }
                Err(error) => {
                    reports.push(json!({"id": id, "ok": false, "error": error}));
                }
            }
        }
        reports
    })
    .await;
    match result {
        Ok(reports) => success_response(reports, 200),
        Err(_) => error_response(&UnifiedError::internal("Scripting worker failed"), debug),
    }
}
