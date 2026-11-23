//! The `Unified` global SDK object.
//!
//! Every TypeScript module runs in its own QuickJS context with one
//! global injected before its script evaluates:
//!
//! ```js
//! Unified = {
//!   version: "0.1.0",
//!   module: { id, name, version },
//!   log:     { debug, info, warn, error },
//!   config:  { get, keys },               // read-only settings snapshot
//!   hooks:   { on, emit },                // the HookBus
//!   events:  { on, emit },                // same bus, same handlers
//!   routes:  { get, post, put, patch, delete },
//!   http:    { fetch },                   // SSRF-safe, synchronous
//!   storage: { read, write, list, delete }, // module directory only
//!   db:      { kv: { get, set, delete, list }, query }, // tenant-scoped
//! }
//! ```
//!
//! The shape mirrors Bun's `Bun` global: one object holding every
//! capability the host offers, nothing else. There is no `fs`,
//! no `process`, no `require`, and no raw network access: a module
//! can only do what the functions above allow.
//!
//! Handler functions declared by a module (`routes.*`, `hooks.on`)
//! are stored in JavaScript-side tables by the bootstrap script
//! below, so Rust never holds a JS function value across calls
//! (no self-referential lifetimes). Rust keeps only the metadata
//! (method/path/event) in `ModuleDeclarations`, which the loader
//! reads after evaluation to build the module summary and the
//! HookBus forwarders.

use parking_lot::Mutex;
use rquickjs::{Ctx, Function, Object, Result as QjsResult, Value};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use unified_core::db::DbPool;
use unified_core::error::UnifiedResult;
use unified_core::hooks::HookBus;
use uuid::Uuid;

use crate::error::format_qjs_error;
use crate::sdk_db::{KvStore, db_query};
use crate::sdk_http::{fetch_blocking, parse_fetch_options};
use crate::sdk_storage::{storage_delete, storage_list, storage_read, storage_write};

/// Version of the `Unified` SDK surface. Bumped when a function is
/// added or its shape changes.
pub const SDK_VERSION: &str = "0.1.0";

// Tenant of the call currently executing on the scripting worker
// thread. Set by the worker before invoking JavaScript (from the
// HTTP request's tenant, or from the event payload) and cleared
// after; `Unified.db` reads it and fails closed without one.
// Thread-local because all JavaScript runs on the one worker
// thread — no lock, no cross-call leakage.
thread_local! {
    static CURRENT_TENANT: Cell<Option<Uuid>> = const { Cell::new(None) };
}

/// Tenant of the JavaScript call in progress on this thread, if any.
pub fn current_tenant() -> Option<Uuid> {
    CURRENT_TENANT.with(|cell| cell.get())
}

/// Set the tenant for the JavaScript call about to run on the
/// worker thread. Called by the engine only.
pub(crate) fn set_current_tenant(tenant: Option<Uuid>) {
    CURRENT_TENANT.with(|cell| cell.set(tenant));
}

// ------------------------------------------------------------ host

/// Host services the SDK functions are backed by. Built once by
/// the integration layer from the `AppContext` (see `host.rs`) or
/// directly by tests. Everything behind it is the real server:
/// the real hook bus, the real database pool, the real filesystem
/// root — no fakes.
pub struct SdkHost {
    /// The server hook bus. `hooks.emit` emits on it;
    /// `hooks.on` registrations are forwarded to it by the loader.
    pub bus: Arc<HookBus>,
    /// Read-only settings snapshot (curated keys only — never
    /// secrets). Built at startup from the application config.
    pub config: HashMap<String, JsonValue>,
    /// Database pool, when a database is configured. `None` makes
    /// `db.query` fail with a clear error instead of panicking.
    /// `std::sync::RwLock` (not parking_lot): the pool is shared
    /// with `AppContext.db`, which is a std lock.
    pub db: Arc<std::sync::RwLock<Option<DbPool>>>,
    /// Root of the per-module storage directories.
    /// `Unified.storage` is confined to `<root>/<module-id>/`.
    pub storage_root: PathBuf,
    /// Extra hosts allowed to skip the public-IP check in
    /// `http.fetch` (admin-configured internal services).
    pub http_allowlist: Vec<String>,
    /// Development exception: allow loopback fetch targets.
    /// Default `false`; never enabled by a module.
    pub http_allow_loopback: bool,
    /// Shared module key/value store (see `sdk_db::KvStore`).
    pub kv: Arc<KvStore>,
}

impl SdkHost {
    /// A host for tests and for servers without a database: a fresh
    /// bus, empty config, no pool, a temporary storage root.
    pub fn for_tests(storage_root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            bus: Arc::new(HookBus::new()),
            config: HashMap::new(),
            db: Arc::new(std::sync::RwLock::new(None)),
            storage_root,
            http_allowlist: Vec::new(),
            http_allow_loopback: false,
            kv: Arc::new(KvStore::new()),
        })
    }
}

// ------------------------------------------------------- declarations

/// One route a module declared: the HTTP method and the path
/// pattern (`/users/:id` — `:name` segments match one path
/// segment and land in `req.params`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteEntry {
    pub method: String,
    pub pattern: String,
}

/// What a module declared while its script evaluated. The worker
/// reads this after evaluation to build the summary, the route
/// catalog entries, and the HookBus forwarders. Plain data only —
/// the handler functions live in the JavaScript-side tables.
#[derive(Debug, Default)]
pub struct ModuleDeclarations {
    pub routes: Vec<RouteEntry>,
    pub hooks: Vec<String>,
}

impl ModuleDeclarations {
    fn add_route(&mut self, method: &str, pattern: &str) {
        let entry = RouteEntry {
            method: method.to_string(),
            pattern: pattern.to_string(),
        };
        if !self.routes.contains(&entry) {
            self.routes.push(entry);
        }
    }

    fn add_hook(&mut self, event: &str) {
        if !self.hooks.iter().any(|item| item == event) {
            self.hooks.push(event.to_string());
        }
    }
}

/// Per-module state captured by every SDK function closure.
pub struct SdkState {
    pub module_id: String,
    pub module_name: String,
    pub module_version: String,
    pub host: Arc<SdkHost>,
    pub declarations: Arc<Mutex<ModuleDeclarations>>,
}

impl SdkState {
    pub fn new(
        module_id: &str,
        module_name: &str,
        module_version: &str,
        host: Arc<SdkHost>,
    ) -> Arc<Self> {
        Arc::new(Self {
            module_id: module_id.to_string(),
            module_name: module_name.to_string(),
            module_version: module_version.to_string(),
            host,
            declarations: Arc::new(Mutex::new(ModuleDeclarations::default())),
        })
    }

    /// The module's private storage directory.
    pub fn storage_dir(&self) -> PathBuf {
        self.host.storage_root.join(&self.module_id)
    }
}

// ------------------------------------------------------- JSON bridge

/// Convert a Rust JSON value into a JavaScript value.
pub fn js_from_json<'a>(ctx: &Ctx<'a>, value: &JsonValue) -> QjsResult<Value<'a>> {
    let text = serde_json::to_string(value)
        .map_err(|error| raise(ctx, &format!("Cannot serialize value: {error}")))?;
    ctx.json_parse(text)
}

/// Convert a JavaScript value into a Rust JSON value
/// (`undefined` becomes `null`).
pub fn json_from_js<'a>(ctx: &Ctx<'a>, value: Value<'a>) -> QjsResult<JsonValue> {
    match ctx.json_stringify(value)? {
        Some(text) => {
            let rendered = text
                .to_string()
                .map_err(|error| raise(ctx, &format!("Cannot read JavaScript value: {error}")))?;
            serde_json::from_str(&rendered)
                .map_err(|error| raise(ctx, &format!("Cannot parse JavaScript value: {error}")))
        }
        None => Ok(JsonValue::Null),
    }
}

/// Render one JavaScript value as a short string for log lines:
/// strings as-is, everything else JSON.
fn js_to_log_string<'a>(ctx: &Ctx<'a>, value: Value<'a>) -> QjsResult<String> {
    if let Some(text) = value.as_string() {
        return text
            .to_string()
            .map_err(|error| raise(ctx, &format!("Cannot read string: {error}")));
    }
    match ctx.json_stringify(value)? {
        Some(text) => text
            .to_string()
            .map_err(|error| raise(ctx, &format!("Cannot read value: {error}"))),
        None => Ok("undefined".to_string()),
    }
}

/// Raise a JavaScript `Error` with `message` from a Rust SDK
/// function, so the module sees a normal throw instead of an
/// engine error.
pub fn raise(ctx: &Ctx<'_>, message: &str) -> rquickjs::Error {
    match rquickjs::Exception::from_message(ctx.clone(), message) {
        Ok(exception) => ctx.throw(exception.into_object().into_inner()),
        Err(error) => error,
    }
}

/// Raise a server error as a JavaScript `Error`.
fn raise_unified(ctx: &Ctx<'_>, error: unified_core::error::UnifiedError) -> rquickjs::Error {
    raise(ctx, &error.to_string())
}

// -------------------------------------------------------- validation

fn validate_event_name(event: &str) -> UnifiedResult<()> {
    if event.is_empty() || event.len() > 128 {
        return Err(unified_core::error::UnifiedError::bad_request(
            "Event name must be 1..=128 characters",
        ));
    }
    if !event
        .chars()
        .all(|char| char.is_ascii_alphanumeric() || matches!(char, '.' | '_' | '-'))
    {
        return Err(unified_core::error::UnifiedError::bad_request(
            "Event name may only contain letters, digits, '.', '_' and '-'",
        ));
    }
    Ok(())
}

fn validate_route_pattern(pattern: &str) -> UnifiedResult<()> {
    if !pattern.starts_with('/') || pattern.len() > 256 {
        return Err(unified_core::error::UnifiedError::bad_request(
            "Route path must start with '/' and be at most 256 characters",
        ));
    }
    if pattern.contains(['?', '#']) {
        return Err(unified_core::error::UnifiedError::bad_request(
            "Route path must not contain a query string or fragment",
        ));
    }
    let segments: Vec<&str> = pattern.split('/').skip(1).collect();
    if segments.is_empty() || segments.len() > 10 {
        return Err(unified_core::error::UnifiedError::bad_request(
            "Route path must have 1..=10 segments",
        ));
    }
    for segment in segments {
        if segment.is_empty() {
            return Err(unified_core::error::UnifiedError::bad_request(
                "Route path must not contain empty segments",
            ));
        }
        if let Some(name) = segment.strip_prefix(':') {
            if name.is_empty()
                || !name
                    .chars()
                    .all(|char| char.is_ascii_alphanumeric() || char == '_')
            {
                return Err(unified_core::error::UnifiedError::bad_request(
                    "Route parameter names may only contain letters, digits and '_'",
                ));
            }
        } else if segment.contains(':') {
            return Err(unified_core::error::UnifiedError::bad_request(
                "':' is only allowed at the start of a route parameter segment",
            ));
        }
    }
    Ok(())
}

// --------------------------------------------------------- bootstrap

/// JavaScript evaluated once per module context, before the module
/// script. Holds the module's declared handlers in JavaScript-side
/// tables and the two dispatcher entry points the Rust worker
/// calls. Route patterns support `:name` segments (matched in
/// order, first match wins); hook handlers run in declaration
/// order. Handlers must be synchronous: an `async` handler (a
/// returned Promise) is refused at dispatch time, because this
/// runtime has no async executor driving promise jobs.
pub const BOOTSTRAP_JS: &str = r#"
globalThis.__unified = { routes: [], hooks: {} };
globalThis.__unified_add_route = function (method, pattern, fn) {
  globalThis.__unified.routes.push({ method: method, pattern: pattern, fn: fn });
};
globalThis.__unified_add_hook = function (event, fn) {
  var list = globalThis.__unified.hooks[event];
  if (!list) { list = []; globalThis.__unified.hooks[event] = list; }
  list.push(fn);
};
globalThis.__unified_match_route = function (pattern, path) {
  var p = pattern.split("/").slice(1);
  var s = path.split("/").slice(1);
  if (p.length !== s.length) return null;
  var params = {};
  for (var i = 0; i < p.length; i++) {
    if (p[i].charAt(0) === ":") { params[p[i].slice(1)] = s[i]; }
    else if (p[i] !== s[i]) { return null; }
  }
  return params;
};
globalThis.__unified_dispatch_route = function (method, path, reqJson) {
  var routes = globalThis.__unified.routes;
  for (var i = 0; i < routes.length; i++) {
    var r = routes[i];
    if (r.method !== method) continue;
    var params = globalThis.__unified_match_route(r.pattern, path);
    if (params === null) continue;
    var req = JSON.parse(reqJson);
    req.params = params;
    var out = r.fn(req);
    if (out && typeof out.then === "function") return JSON.stringify({ __unified_async: true });
    return JSON.stringify(out === undefined ? null : out);
  }
  return JSON.stringify({ __unified_no_route: true });
};
globalThis.__unified_call_hooks = function (event, dataJson) {
  var list = globalThis.__unified.hooks[event] || [];
  var data = JSON.parse(dataJson);
  var results = [];
  for (var i = 0; i < list.length; i++) {
    var out = list[i]({ name: event, data: data });
    if (out && typeof out.then === "function") continue;
    results.push(out === undefined ? null : out);
  }
  return JSON.stringify(results);
};
"#;

// ------------------------------------------------------------- global

/// Build the `Unified` global object for one module context.
/// Every function below is backed by the real host services in
/// `state`; nothing here is a stub.
pub fn build_unified_global<'a>(ctx: &Ctx<'a>, state: &Arc<SdkState>) -> QjsResult<Object<'a>> {
    let unified = Object::new(ctx.clone())?;
    unified.set("version", SDK_VERSION)?;

    let module_info = Object::new(ctx.clone())?;
    module_info.set("id", state.module_id.as_str())?;
    module_info.set("name", state.module_name.as_str())?;
    module_info.set("version", state.module_version.as_str())?;
    unified.set("module", module_info)?;

    unified.set("log", build_log(ctx, state)?)?;
    unified.set("config", build_config(ctx, state)?)?;
    let hooks = build_hooks(ctx, state)?;
    unified.set("hooks", hooks.clone())?;
    // `events` is the same bus as `hooks`, under the second name
    // the SDK advertises. One object, two keys.
    unified.set("events", hooks)?;
    unified.set("routes", build_routes(ctx, state)?)?;
    unified.set("http", build_http(ctx, state)?)?;
    unified.set("storage", build_storage(ctx, state)?)?;
    unified.set("db", build_db(ctx, state)?)?;
    Ok(unified)
}

fn log_fn<'a>(
    ctx: &Ctx<'a>,
    state: &Arc<SdkState>,
    level: &'static str,
) -> QjsResult<Function<'a>> {
    let state = Arc::clone(state);
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'a>, args: rquickjs::function::Rest<Value<'a>>| {
            let mut parts: Vec<String> = Vec::new();
            for arg in args.0 {
                match js_to_log_string(&ctx, arg) {
                    Ok(text) => parts.push(text),
                    Err(error) => return Err(error),
                }
            }
            let message = parts.join(" ");
            let target = format!("unified::scripting::{}", state.module_id);
            match level {
                "debug" => log::debug!(target: &target, "{message}"),
                "warn" => log::warn!(target: &target, "{message}"),
                "error" => log::error!(target: &target, "{message}"),
                _ => log::info!(target: &target, "{message}"),
            }
            Ok(())
        },
    )
}

fn build_log<'a>(ctx: &Ctx<'a>, state: &Arc<SdkState>) -> QjsResult<Object<'a>> {
    let log = Object::new(ctx.clone())?;
    log.set("debug", log_fn(ctx, state, "debug")?)?;
    log.set("info", log_fn(ctx, state, "info")?)?;
    log.set("warn", log_fn(ctx, state, "warn")?)?;
    log.set("error", log_fn(ctx, state, "error")?)?;
    Ok(log)
}

fn build_config<'a>(ctx: &Ctx<'a>, state: &Arc<SdkState>) -> QjsResult<Object<'a>> {
    let config = Object::new(ctx.clone())?;
    let state_get = Arc::clone(state);
    config.set(
        "get",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'a>, key: String| match state_get.host.config.get(&key) {
                Some(value) => js_from_json(&ctx, value).map(Some),
                None => Ok(None),
            },
        )?,
    )?;
    let state_keys = Arc::clone(state);
    config.set(
        "keys",
        Function::new(
            ctx.clone(),
            move |_ctx: Ctx<'a>| -> QjsResult<Vec<String>> {
                let mut keys: Vec<String> = state_keys.host.config.keys().cloned().collect();
                keys.sort();
                Ok(keys)
            },
        )?,
    )?;
    Ok(config)
}

fn build_hooks<'a>(ctx: &Ctx<'a>, state: &Arc<SdkState>) -> QjsResult<Object<'a>> {
    let hooks = Object::new(ctx.clone())?;
    let state_on = Arc::clone(state);
    hooks.set(
        "on",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'a>, event: String, handler: Function<'a>| {
                if let Err(error) = validate_event_name(&event) {
                    return Err(raise_unified(&ctx, error));
                }
                state_on.declarations.lock().add_hook(&event);
                let add: Function<'a> = ctx
                    .globals()
                    .get("__unified_add_hook")
                    .map_err(|error| raise(&ctx, &format_qjs_error(&ctx, &error)))?;
                add.call::<_, ()>((event, handler))
                    .map_err(|error| raise(&ctx, &format_qjs_error(&ctx, &error)))?;
                Ok(())
            },
        )?,
    )?;
    let state_emit = Arc::clone(state);
    hooks.set(
        "emit",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'a>, event: String, data: Value<'a>| {
                if let Err(error) = validate_event_name(&event) {
                    return Err(raise_unified(&ctx, error));
                }
                let json = match json_from_js(&ctx, data) {
                    Ok(value) => value,
                    Err(error) => return Err(error),
                };
                // Synchronous emit on the real bus. Rust forwarders
                // enqueue the JavaScript handlers fire-and-forget, so
                // this cannot deadlock even when called from inside a
                // JavaScript hook or route handler.
                let emitted = state_emit.host.bus.emit(&event, json);
                let results = JsonValue::Array(emitted.results.clone());
                js_from_json(&ctx, &results)
            },
        )?,
    )?;
    Ok(hooks)
}

fn build_routes<'a>(ctx: &Ctx<'a>, state: &Arc<SdkState>) -> QjsResult<Object<'a>> {
    let routes = Object::new(ctx.clone())?;
    for method in ["GET", "POST", "PUT", "PATCH", "DELETE"] {
        let state = Arc::clone(state);
        let method_name = method.to_ascii_lowercase();
        routes.set(
            method_name.as_str(),
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'a>, pattern: String, handler: Function<'a>| {
                    if let Err(error) = validate_route_pattern(&pattern) {
                        return Err(raise_unified(&ctx, error));
                    }
                    state.declarations.lock().add_route(method, &pattern);
                    let add: Function<'a> = ctx
                        .globals()
                        .get("__unified_add_route")
                        .map_err(|error| raise(&ctx, &format_qjs_error(&ctx, &error)))?;
                    add.call::<_, ()>((method, pattern, handler))
                        .map_err(|error| raise(&ctx, &format_qjs_error(&ctx, &error)))?;
                    Ok(())
                },
            )?,
        )?;
    }
    Ok(routes)
}

fn build_http<'a>(ctx: &Ctx<'a>, state: &Arc<SdkState>) -> QjsResult<Object<'a>> {
    let http = Object::new(ctx.clone())?;
    let state = Arc::clone(state);
    http.set(
        "fetch",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'a>, url: String, options: Value<'a>| {
                let options_json = match json_from_js(&ctx, options) {
                    Ok(value) => value,
                    Err(error) => return Err(error),
                };
                let request = match parse_fetch_options(&url, &options_json) {
                    Ok(request) => request,
                    Err(error) => return Err(raise_unified(&ctx, error)),
                };
                // Blocking call on the worker thread (a plain OS
                // thread, never a tokio worker) with a timeout.
                let response = match fetch_blocking(
                    &request,
                    &state.host.http_allowlist,
                    state.host.http_allow_loopback,
                ) {
                    Ok(response) => response,
                    Err(error) => return Err(raise_unified(&ctx, error)),
                };
                js_from_json(&ctx, &response)
            },
        )?,
    )?;
    Ok(http)
}

fn build_storage<'a>(ctx: &Ctx<'a>, state: &Arc<SdkState>) -> QjsResult<Object<'a>> {
    let storage = Object::new(ctx.clone())?;
    let state_read = Arc::clone(state);
    storage.set(
        "read",
        Function::new(ctx.clone(), move |ctx: Ctx<'_>, path: String| {
            let dir = state_read.storage_dir();
            match storage_read(&dir, &path) {
                Ok(text) => Ok(text),
                Err(error) => Err(raise_unified(&ctx, error)),
            }
        })?,
    )?;
    let state_write = Arc::clone(state);
    storage.set(
        "write",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'_>, path: String, data: String| {
                let dir = state_write.storage_dir();
                match storage_write(&dir, &path, &data) {
                    Ok(()) => Ok(()),
                    Err(error) => Err(raise_unified(&ctx, error)),
                }
            },
        )?,
    )?;
    let state_list = Arc::clone(state);
    storage.set(
        "list",
        Function::new(ctx.clone(), move |ctx: Ctx<'_>, dir: Option<String>| {
            let base = state_list.storage_dir();
            let sub = dir.unwrap_or_default();
            match storage_list(&base, &sub) {
                Ok(names) => Ok(names),
                Err(error) => Err(raise_unified(&ctx, error)),
            }
        })?,
    )?;
    let state_delete = Arc::clone(state);
    storage.set(
        "delete",
        Function::new(ctx.clone(), move |ctx: Ctx<'_>, path: String| {
            let dir = state_delete.storage_dir();
            match storage_delete(&dir, &path) {
                Ok(deleted) => Ok(deleted),
                Err(error) => Err(raise_unified(&ctx, error)),
            }
        })?,
    )?;
    Ok(storage)
}

fn build_db<'a>(ctx: &Ctx<'a>, state: &Arc<SdkState>) -> QjsResult<Object<'a>> {
    let db = Object::new(ctx.clone())?;
    let kv = Object::new(ctx.clone())?;
    let state_get = Arc::clone(state);
    kv.set(
        "get",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'a>, key: String| match state_get.host.kv.get(
                current_tenant(),
                &state_get.module_id,
                &key,
            ) {
                Ok(Some(value)) => js_from_json(&ctx, &value).map(Some),
                Ok(None) => Ok(None),
                Err(error) => Err(raise_unified(&ctx, error)),
            },
        )?,
    )?;
    let state_set = Arc::clone(state);
    kv.set(
        "set",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'a>, key: String, value: Value<'a>| {
                let json = match json_from_js(&ctx, value) {
                    Ok(value) => value,
                    Err(error) => return Err(error),
                };
                match state_set
                    .host
                    .kv
                    .set(current_tenant(), &state_set.module_id, &key, json)
                {
                    Ok(()) => Ok(()),
                    Err(error) => Err(raise_unified(&ctx, error)),
                }
            },
        )?,
    )?;
    let state_delete = Arc::clone(state);
    kv.set(
        "delete",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'_>, key: String| match state_delete.host.kv.delete(
                current_tenant(),
                &state_delete.module_id,
                &key,
            ) {
                Ok(deleted) => Ok(deleted),
                Err(error) => Err(raise_unified(&ctx, error)),
            },
        )?,
    )?;
    let state_list = Arc::clone(state);
    kv.set(
        "list",
        Function::new(ctx.clone(), move |ctx: Ctx<'_>, prefix: Option<String>| {
            let prefix = prefix.unwrap_or_default();
            match state_list
                .host
                .kv
                .list(current_tenant(), &state_list.module_id, &prefix)
            {
                Ok(keys) => Ok(keys),
                Err(error) => Err(raise_unified(&ctx, error)),
            }
        })?,
    )?;
    db.set("kv", kv)?;
    let state_query = Arc::clone(state);
    db.set(
        "query",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'a>, sql: String, params: Value<'a>| {
                let tenant = match current_tenant() {
                    Some(tenant) => tenant,
                    None => {
                        return Err(raise(
                            &ctx,
                            "db.query needs a tenant context (call it from a tenant request or a tenant event)",
                        ));
                    }
                };
                let pool = match state_query.host.db.read().expect("db lock poisoned").clone() {
                    Some(pool) => pool,
                    None => {
                        return Err(raise(
                            &ctx,
                            "db.query needs a configured database",
                        ));
                    }
                };
                let params_json = match json_from_js(&ctx, params) {
                    Ok(value) => value,
                    Err(error) => return Err(error),
                };
                let params: Vec<JsonValue> = match params_json {
                    JsonValue::Null => Vec::new(),
                    JsonValue::Array(items) => items,
                    other => vec![other],
                };
                // Diesel is blocking; this runs on the worker
                // thread, never on a tokio worker.
                match db_query(&pool, tenant, &sql, &params) {
                    Ok(rows) => js_from_json(&ctx, &JsonValue::Array(rows)),
                    Err(error) => Err(raise_unified(&ctx, error)),
                }
            },
        )?,
    )?;
    Ok(db)
}
