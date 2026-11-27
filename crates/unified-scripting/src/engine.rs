//! The scripting engine: one worker thread owns every QuickJS
//! runtime; everything else talks to it through a channel.
//!
//! Threading model (deliberate, and the reason this file exists):
//!
//! * QuickJS runtimes/contexts live on ONE dedicated OS thread
//!   (the worker). They are never sent across threads, so no
//!   `Send`/`Sync` gymnastics and no concurrent access to a JS
//!   heap are possible at all.
//! * Callers (HTTP handlers, hook forwarders, tests) hold an
//!   `Arc<ScriptingEngine>` and send requests over a channel. Sync
//!   calls (`dispatch_route`, `call_hook`, `load_module`) block
//!   with a timeout waiting for the reply; hook emissions from the
//!   bus are fire-and-forget (`emit_hook_async`) so an event never
//!   blocks on a script.
//! * Each module gets its OWN `Runtime` + `Context` (own globals,
//!   own memory limit). A module cannot see another module's
//!   variables, functions, or `Unified` state.
//! * Resource limits are enforced by QuickJS itself: a per-module
//!   memory cap (`set_memory_limit`) and an execution deadline
//!   enforced by an interrupt handler (`set_interrupt_handler`).
//!   A runaway loop or a memory hog aborts the call instead of
//!   hanging or OOMing the server. The module stays loaded; the
//!   next call runs fresh.
//!
//! The compiled JavaScript of every loaded module is held in RAM
//! (`LoadedModule::compiled_js`) - the server never reads a `.js`
//! file from disk.

use parking_lot::RwLock;
use rquickjs::{Context, Function, Runtime, Value};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::error::{format_qjs_error, qjs_error};
use crate::loader::ModuleMeta;
use crate::sdk::{
    BOOTSTRAP_JS, ModuleDeclarations, RouteEntry, SdkHost, SdkState, build_unified_global,
    current_tenant, set_current_tenant,
};

/// Resource limits for one script module. All three are enforced
/// by QuickJS, not by convention.
#[derive(Debug, Clone)]
pub struct ScriptLimits {
    /// Maximum QuickJS heap per module (default 64 MiB).
    pub memory_bytes: usize,
    /// Maximum JS call-stack size per module (default 512 KiB).
    pub max_stack_bytes: usize,
    /// Maximum wall time of one JavaScript call (default 5 s).
    /// Enforced by the interrupt handler; a call that exceeds it
    /// is aborted and reported as an error.
    pub execution_timeout: Duration,
}

impl Default for ScriptLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 64 * 1024 * 1024,
            max_stack_bytes: 512 * 1024,
            execution_timeout: Duration::from_secs(5),
        }
    }
}

/// One module as the admin API and the module manager see it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    /// `"loaded"` or `"error"`.
    pub status: String,
    /// Last load error, when `status` is `"error"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub routes: Vec<RouteEntry>,
    pub hooks: Vec<String>,
    /// Consecutive failed calls (hook or route). Resets on the
    /// next success; at 10 the status flips to `"error"`.
    pub failures: u32,
}

/// One HTTP request as a script route handler sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRequest {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    pub body: JsonValue,
}

/// What a script route handler returned: the HTTP status and the
/// JSON body (the dispatcher wraps the body in the standard
/// `{"data": ...}` envelope).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteResponse {
    pub status: u16,
    pub body: JsonValue,
}

/// After how many consecutive failures a module is marked
/// `"error"` in the summary (it stays loaded; an admin reloads it).
const MAX_CONSECUTIVE_FAILURES: u32 = 10;

/// Maximum queued fire-and-forget hook emissions. Past this, an
/// emission is dropped and a warning logged instead of growing the
/// queue without bound.
const MAX_QUEUED_ASYNC_EMITS: usize = 1024;

type Reply<T> = mpsc::Sender<Result<T, String>>;

enum EngineRequest {
    Load {
        meta: ModuleMeta,
        js: String,
        reply: Reply<ModuleSummary>,
    },
    Reload {
        id: String,
        js: String,
        reply: Reply<ModuleSummary>,
    },
    Unload {
        id: String,
        reply: Reply<()>,
    },
    CallHook {
        module_id: String,
        event: String,
        data: JsonValue,
        tenant: Option<Uuid>,
        reply: Reply<Vec<JsonValue>>,
    },
    DispatchRoute {
        module_id: String,
        method: String,
        path: String,
        request: RouteRequest,
        tenant: Option<Uuid>,
        reply: Reply<RouteResponse>,
    },
    EmitHookAsync {
        module_id: String,
        event: String,
        data: JsonValue,
        tenant: Option<Uuid>,
    },
    List {
        reply: Reply<Vec<ModuleSummary>>,
    },
}

#[derive(Default)]
struct EngineShared {
    summaries: RwLock<HashMap<String, ModuleSummary>>,
    queued_async: AtomicUsize,
    /// Mutable per-module metadata shared with the `TsModule`
    /// registered in the `ModuleRegistry` (its `route_info` reads
    /// this, so a reload updates the catalog without
    /// re-registering the module - the registry refuses duplicate
    /// ids).
    ts_state: RwLock<HashMap<String, Arc<TsModuleState>>>,
}

/// Mutable metadata of one TypeScript module, shared between the
/// engine (updated on load/reload) and the `TsModule` in the
/// `ModuleRegistry` (read for the route catalog).
pub struct TsModuleState {
    pub routes: RwLock<Vec<RouteEntry>>,
    pub hooks: RwLock<Vec<String>>,
}

impl TsModuleState {
    fn from_summary(summary: &ModuleSummary) -> Arc<Self> {
        Arc::new(Self {
            routes: RwLock::new(summary.routes.clone()),
            hooks: RwLock::new(summary.hooks.clone()),
        })
    }
}

/// One loaded module, owned by the worker thread only.
struct LoadedModule {
    meta: ModuleMeta,
    runtime: Runtime,
    context: Context,
    state: Arc<SdkState>,
    /// Execution deadline in millis since the Unix epoch; `0`
    /// means no deadline. Read by the interrupt handler.
    deadline: Arc<AtomicI64>,
    /// Compiled JavaScript, held in RAM (the server never reads a
    /// `.js` file from disk).
    compiled_js: String,
    summary: ModuleSummary,
}

/// The scripting engine handle. Cheap to clone; every method sends
/// a request to the worker thread and blocks (with a timeout) for
/// the reply, except `emit_hook_async`, which never blocks.
pub struct ScriptingEngine {
    sender: mpsc::Sender<EngineRequest>,
    shared: Arc<EngineShared>,
    host: Arc<SdkHost>,
    limits: ScriptLimits,
    /// Directory script modules are loaded from (also used by the
    /// admin reload endpoints to re-read a module).
    modules_dir: std::path::PathBuf,
}

impl ScriptingEngine {
    /// Start the worker thread and return the engine handle.
    pub fn new(
        host: Arc<SdkHost>,
        limits: ScriptLimits,
        modules_dir: std::path::PathBuf,
    ) -> Arc<Self> {
        let (sender, receiver) = mpsc::channel::<EngineRequest>();
        let shared = Arc::new(EngineShared::default());
        let worker_shared = Arc::clone(&shared);
        let worker_host = Arc::clone(&host);
        let worker_limits = limits.clone();
        std::thread::Builder::new()
            .name("unified-scripting".to_string())
            .spawn(move || {
                worker_loop(receiver, worker_host, worker_limits, worker_shared);
            })
            .expect("cannot start the scripting worker thread");
        Arc::new(Self {
            sender,
            shared,
            host,
            limits,
            modules_dir,
        })
    }

    /// How long a sync call waits for the worker before giving up
    /// (the JavaScript deadline plus a grace period for the reply).
    fn wait_timeout(&self) -> Duration {
        self.limits.execution_timeout + Duration::from_secs(2)
    }

    fn send<T>(&self, build: impl FnOnce(Reply<T>) -> EngineRequest) -> Result<T, String> {
        let (reply, receiver) = mpsc::channel::<Result<T, String>>();
        self.sender
            .send(build(reply))
            .map_err(|_| "Scripting worker is gone".to_string())?;
        receiver
            .recv_timeout(self.wait_timeout())
            .map_err(|_| "Scripting worker did not answer in time".to_string())?
    }

    /// Load a module from already-transpiled JavaScript. The
    /// module's top-level script runs immediately (that is when it
    /// declares its routes and hooks).
    pub fn load_module(&self, meta: ModuleMeta, js: String) -> Result<ModuleSummary, String> {
        let result = self.send(|reply| EngineRequest::Load { meta, js, reply });
        if let Ok(summary) = &result {
            self.shared
                .ts_state
                .write()
                .insert(summary.id.clone(), TsModuleState::from_summary(summary));
        }
        result
    }

    /// Re-transpile + re-run a loaded module in a fresh context.
    /// The Rust wiring (dispatcher routes, hook forwarders) is
    /// stable across reloads: only the JavaScript implementation
    /// is swapped, so no server restart is needed.
    pub fn reload_module(&self, id: &str, js: String) -> Result<ModuleSummary, String> {
        let result = self.send(|reply| EngineRequest::Reload {
            id: id.to_string(),
            js,
            reply,
        });
        if let Ok(summary) = &result {
            self.shared
                .ts_state
                .write()
                .insert(summary.id.clone(), TsModuleState::from_summary(summary));
        }
        result
    }

    pub fn unload_module(&self, id: &str) -> Result<(), String> {
        let result = self.send(|reply| EngineRequest::Unload {
            id: id.to_string(),
            reply,
        });
        if result.is_ok() {
            self.shared.ts_state.write().remove(id);
        }
        result
    }

    /// Call one module's hook handlers for `event` synchronously
    /// and return their results. Used by tests and by direct
    /// callers; the HookBus path uses `emit_hook_async` instead.
    pub fn call_hook(
        &self,
        module_id: &str,
        event: &str,
        data: JsonValue,
        tenant: Option<Uuid>,
    ) -> Result<Vec<JsonValue>, String> {
        self.send(|reply| EngineRequest::CallHook {
            module_id: module_id.to_string(),
            event: event.to_string(),
            data,
            tenant,
            reply,
        })
    }

    /// Enqueue a hook call for a module without waiting for it.
    /// The module's handlers run on the worker thread; errors are
    /// logged and counted, never returned. Past the queue cap the
    /// emission is dropped with a warning.
    pub fn emit_hook_async(
        &self,
        module_id: &str,
        event: &str,
        data: JsonValue,
        tenant: Option<Uuid>,
    ) {
        let queued = self.shared.queued_async.fetch_add(1, Ordering::SeqCst);
        if queued >= MAX_QUEUED_ASYNC_EMITS {
            self.shared.queued_async.fetch_sub(1, Ordering::SeqCst);
            log::warn!("Scripting queue full; dropping async hook {event} for module {module_id}");
            return;
        }
        if self
            .sender
            .send(EngineRequest::EmitHookAsync {
                module_id: module_id.to_string(),
                event: event.to_string(),
                data,
                tenant,
            })
            .is_err()
        {
            self.shared.queued_async.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Dispatch one HTTP request to a module's route handler.
    pub fn dispatch_route(
        &self,
        module_id: &str,
        method: &str,
        path: &str,
        request: RouteRequest,
        tenant: Option<Uuid>,
    ) -> Result<RouteResponse, String> {
        self.send(|reply| EngineRequest::DispatchRoute {
            module_id: module_id.to_string(),
            method: method.to_string(),
            path: path.to_string(),
            request,
            tenant,
            reply,
        })
    }

    /// Summaries of all loaded modules (from shared state, no
    /// worker round-trip).
    pub fn list_modules(&self) -> Vec<ModuleSummary> {
        let mut modules: Vec<ModuleSummary> =
            self.shared.summaries.read().values().cloned().collect();
        modules.sort_by(|a, b| a.id.cmp(&b.id));
        modules
    }

    /// The host services the SDK functions are backed by.
    pub fn host(&self) -> &Arc<SdkHost> {
        &self.host
    }

    /// The configured resource limits.
    pub fn limits(&self) -> &ScriptLimits {
        &self.limits
    }

    /// Directory script modules are loaded from.
    pub fn modules_dir(&self) -> &std::path::Path {
        &self.modules_dir
    }

    /// Mutable metadata of one loaded module, shared with its
    /// `TsModule` in the registry (`None` when not loaded).
    pub fn ts_state(&self, id: &str) -> Option<Arc<TsModuleState>> {
        self.shared.ts_state.read().get(id).cloned()
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Set the execution deadline for the next JavaScript call on
/// this module, run `run`, then clear the deadline again. The
/// interrupt handler aborts the call when the deadline passes.
fn with_deadline<T>(deadline: &Arc<AtomicI64>, timeout: Duration, run: impl FnOnce() -> T) -> T {
    deadline.store(now_millis() + timeout.as_millis() as i64, Ordering::SeqCst);
    let result = run();
    deadline.store(0, Ordering::SeqCst);
    result
}

/// Set the per-call tenant for the JavaScript about to run, run
/// `run`, then clear it. `Unified.db` reads this thread-local.
fn with_tenant<T>(tenant: Option<Uuid>, run: impl FnOnce() -> T) -> T {
    let previous = current_tenant();
    set_current_tenant(tenant);
    let result = run();
    set_current_tenant(previous);
    result
}

fn worker_loop(
    receiver: mpsc::Receiver<EngineRequest>,
    host: Arc<SdkHost>,
    limits: ScriptLimits,
    shared: Arc<EngineShared>,
) {
    let mut modules: HashMap<String, LoadedModule> = HashMap::new();
    while let Ok(request) = receiver.recv() {
        // A panic in one request must not kill the worker (and
        // with it every module): catch it, log it, reply with an
        // error where a reply exists.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle_request(&mut modules, &host, &limits, &shared, request);
        }));
        if let Err(panic) = outcome {
            let message = panic
                .downcast_ref::<&str>()
                .map(|text| (*text).to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            log::error!("Scripting worker caught a panic: {message}");
        }
    }
}

fn handle_request(
    modules: &mut HashMap<String, LoadedModule>,
    host: &Arc<SdkHost>,
    limits: &ScriptLimits,
    shared: &Arc<EngineShared>,
    request: EngineRequest,
) {
    match request {
        EngineRequest::Load { meta, js, reply } => {
            let result = load_module_into(modules, host, limits, shared, meta, js);
            let _ = reply.send(result);
        }
        EngineRequest::Reload { id, js, reply } => {
            let result = reload_module_into(modules, host, limits, shared, &id, js);
            let _ = reply.send(result);
        }
        EngineRequest::Unload { id, reply } => {
            let existed = modules.remove(&id).is_some();
            shared.summaries.write().remove(&id);
            let _ = reply.send(if existed {
                Ok(())
            } else {
                Err(format!("No loaded script module: {id}"))
            });
        }
        EngineRequest::CallHook {
            module_id,
            event,
            data,
            tenant,
            reply,
        } => {
            let result =
                call_module_hook(modules, shared, limits, &module_id, &event, data, tenant);
            let _ = reply.send(result);
        }
        EngineRequest::DispatchRoute {
            module_id,
            method,
            path,
            request,
            tenant,
            reply,
        } => {
            let result = dispatch_module_route(
                modules, shared, limits, &module_id, &method, &path, request, tenant,
            );
            let _ = reply.send(result);
        }
        EngineRequest::EmitHookAsync {
            module_id,
            event,
            data,
            tenant,
        } => {
            shared.queued_async.fetch_sub(1, Ordering::SeqCst);
            match call_module_hook(modules, shared, limits, &module_id, &event, data, tenant) {
                Ok(_) => {}
                Err(error) => {
                    record_failure(modules, shared, &module_id);
                    log::warn!("Script module {module_id} hook {event} failed: {error}");
                }
            }
        }
        EngineRequest::List { reply } => {
            let mut list: Vec<ModuleSummary> = shared.summaries.read().values().cloned().collect();
            list.sort_by(|a, b| a.id.cmp(&b.id));
            let _ = reply.send(Ok(list));
        }
    }
}

/// Create a runtime + context for a module, inject `Unified`,
/// evaluate the bootstrap and the module script, and store the
/// loaded module. Returns the summary built from the module's
/// declarations.
fn load_module_into(
    modules: &mut HashMap<String, LoadedModule>,
    host: &Arc<SdkHost>,
    limits: &ScriptLimits,
    shared: &Arc<EngineShared>,
    meta: ModuleMeta,
    js: String,
) -> Result<ModuleSummary, String> {
    let runtime =
        Runtime::new().map_err(|error| format!("Cannot create script runtime: {error}"))?;
    runtime.set_memory_limit(limits.memory_bytes);
    runtime.set_max_stack_size(limits.max_stack_bytes);
    let deadline = Arc::new(AtomicI64::new(0));
    let handler_deadline = Arc::clone(&deadline);
    runtime.set_interrupt_handler(Some(Box::new(move || {
        let limit = handler_deadline.load(Ordering::SeqCst);
        limit != 0 && now_millis() > limit
    })));
    let context = Context::full(&runtime)
        .map_err(|error| format!("Cannot create script context: {error}"))?;
    let state = SdkState::new(&meta.id, &meta.name, &meta.version, Arc::clone(host));
    let id = meta.id.clone();
    let load = with_deadline(&deadline, limits.execution_timeout, || {
        context.with(|ctx| {
            ctx.eval::<Value, _>(BOOTSTRAP_JS)
                .map_err(|error| qjs_error(&ctx, &id, error).to_string())?;
            let unified = build_unified_global(&ctx, &state)
                .map_err(|error| qjs_error(&ctx, &id, error).to_string())?;
            ctx.globals()
                .set("Unified", unified)
                .map_err(|error| qjs_error(&ctx, &id, error).to_string())?;
            ctx.eval::<Value, _>(js.as_str())
                .map_err(|error| qjs_error(&ctx, &id, error).to_string())?;
            Ok::<(), String>(())
        })
    });
    if let Err(error) = load {
        return Err(error);
    }
    let declarations: ModuleDeclarations = {
        let guard = state.declarations.lock();
        ModuleDeclarations {
            routes: guard.routes.clone(),
            hooks: guard.hooks.clone(),
        }
    };
    let summary = ModuleSummary {
        id: meta.id.clone(),
        name: meta.name.clone(),
        version: meta.version.clone(),
        description: meta.description.clone(),
        status: "loaded".to_string(),
        error: None,
        routes: declarations.routes,
        hooks: declarations.hooks,
        failures: 0,
    };
    shared
        .summaries
        .write()
        .insert(meta.id.clone(), summary.clone());
    modules.insert(
        meta.id.clone(),
        LoadedModule {
            meta,
            runtime,
            context,
            state,
            deadline,
            compiled_js: js,
            summary: summary.clone(),
        },
    );
    Ok(summary)
}

fn reload_module_into(
    modules: &mut HashMap<String, LoadedModule>,
    host: &Arc<SdkHost>,
    limits: &ScriptLimits,
    shared: &Arc<EngineShared>,
    id: &str,
    js: String,
) -> Result<ModuleSummary, String> {
    let Some(old) = modules.remove(id) else {
        return Err(format!("No loaded script module: {id}"));
    };
    // Keep the metadata (name/version/description come from the
    // manifest, which the caller re-read); only the implementation
    // is swapped.
    let meta = old.meta.clone();
    drop(old);
    load_module_into(modules, host, limits, shared, meta, js)
}

/// Call a module's hook handlers for `event` and return their
/// results. The JavaScript dispatcher returns a JSON array.
fn call_module_hook(
    modules: &mut HashMap<String, LoadedModule>,
    shared: &Arc<EngineShared>,
    limits: &ScriptLimits,
    module_id: &str,
    event: &str,
    data: JsonValue,
    tenant: Option<Uuid>,
) -> Result<Vec<JsonValue>, String> {
    let Some(loaded) = modules.get_mut(module_id) else {
        return Err(format!("No loaded script module: {module_id}"));
    };
    let data_text =
        serde_json::to_string(&data).map_err(|error| format!("Cannot serialize event: {error}"))?;
    let outcome = with_tenant(tenant, || {
        with_deadline(&loaded.deadline, limits.execution_timeout, || {
            loaded.context.with(|ctx| {
                let call: Function = ctx
                    .globals()
                    .get("__unified_call_hooks")
                    .map_err(|error| format_qjs_error(&ctx, &error))?;
                let rendered: String = call
                    .call((event, data_text))
                    .map_err(|error| format_qjs_error(&ctx, &error))?;
                serde_json::from_str::<Vec<JsonValue>>(&rendered)
                    .map_err(|error| format!("Hook result is not valid JSON: {error}"))
            })
        })
    });
    match outcome {
        Ok(results) => {
            note_success(modules, shared, module_id);
            Ok(results)
        }
        Err(error) => {
            record_failure(modules, shared, module_id);
            Err(error)
        }
    }
}

/// Dispatch one HTTP request to a module's route handler. The
/// JavaScript dispatcher returns the handler's return value as
/// JSON; a return of exactly `{ status, body }` sets the status,
/// anything else is a 200 body.
fn dispatch_module_route(
    modules: &mut HashMap<String, LoadedModule>,
    shared: &Arc<EngineShared>,
    limits: &ScriptLimits,
    module_id: &str,
    method: &str,
    path: &str,
    request: RouteRequest,
    tenant: Option<Uuid>,
) -> Result<RouteResponse, String> {
    let Some(loaded) = modules.get_mut(module_id) else {
        return Err(format!("No loaded script module: {module_id}"));
    };
    let request_text = serde_json::to_string(&request)
        .map_err(|error| format!("Cannot serialize request: {error}"))?;
    let outcome = with_tenant(tenant, || {
        with_deadline(&loaded.deadline, limits.execution_timeout, || {
            loaded.context.with(|ctx| {
                let call: Function = ctx
                    .globals()
                    .get("__unified_dispatch_route")
                    .map_err(|error| format_qjs_error(&ctx, &error))?;
                let rendered: String = call
                    .call((method, path, request_text))
                    .map_err(|error| format_qjs_error(&ctx, &error))?;
                let value: JsonValue = serde_json::from_str(&rendered)
                    .map_err(|error| format!("Route result is not valid JSON: {error}"))?;
                if value
                    .get("__unified_no_route")
                    .and_then(JsonValue::as_bool)
                    .unwrap_or(false)
                {
                    return Err(format!("No such route: {method} {path}"));
                }
                if value
                    .get("__unified_async")
                    .and_then(JsonValue::as_bool)
                    .unwrap_or(false)
                {
                    return Err(
                        "Async route handlers are not supported; return a plain value".to_string(),
                    );
                }
                Ok(value)
            })
        })
    });
    match outcome {
        Ok(value) => {
            note_success(modules, shared, module_id);
            Ok(split_route_response(value))
        }
        Err(error) => {
            record_failure(modules, shared, module_id);
            Err(error)
        }
    }
}

/// Split a route handler's return value into a status and a body:
/// exactly `{ status, body }` (numeric status 100..=599) sets the
/// status; anything else is a 200 body.
fn split_route_response(value: JsonValue) -> RouteResponse {
    if let JsonValue::Object(map) = &value {
        if map.len() == 2 {
            if let (Some(JsonValue::Number(status)), Some(body)) =
                (map.get("status"), map.get("body"))
            {
                if let Some(code) = status.as_u64() {
                    if (100..=599).contains(&code) {
                        return RouteResponse {
                            status: code as u16,
                            body: body.clone(),
                        };
                    }
                }
            }
        }
    }
    RouteResponse {
        status: 200,
        body: value,
    }
}

fn note_success(
    modules: &mut HashMap<String, LoadedModule>,
    shared: &Arc<EngineShared>,
    module_id: &str,
) {
    if let Some(loaded) = modules.get_mut(module_id) {
        loaded.summary.failures = 0;
        if loaded.summary.status == "error" {
            loaded.summary.status = "loaded".to_string();
            loaded.summary.error = None;
        }
        let summary = loaded.summary.clone();
        shared
            .summaries
            .write()
            .insert(module_id.to_string(), summary);
    }
}

/// Count a failed call against a module; at the cap the summary
/// flips to `"error"` (the module stays loaded - an admin reloads
/// it). Both the worker map and the shared summaries map are
/// updated, so `list_modules` sees the same state.
fn record_failure(
    modules: &mut HashMap<String, LoadedModule>,
    shared: &Arc<EngineShared>,
    module_id: &str,
) {
    if let Some(loaded) = modules.get_mut(module_id) {
        loaded.summary.failures = loaded.summary.failures.saturating_add(1);
        if loaded.summary.failures >= MAX_CONSECUTIVE_FAILURES {
            loaded.summary.status = "error".to_string();
            loaded.summary.error = Some(format!(
                "{} consecutive calls failed; reload the module",
                loaded.summary.failures
            ));
        }
        let summary = loaded.summary.clone();
        shared
            .summaries
            .write()
            .insert(module_id.to_string(), summary);
    }
}
