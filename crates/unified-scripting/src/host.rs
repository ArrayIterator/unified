//! Host wiring: build the SDK host from the server context and
//! forward HookBus events to TypeScript modules.
//!
//! Paths and fetch policy come from environment variables with
//! safe defaults (there is no `scripting` section in the server
//! config file yet — a config-file section is the documented
//! follow-up, not a silent default):
//!
//! * `UNIFIED_SCRIPTING_MODULES_DIR` — default `modules`
//! * `UNIFIED_SCRIPTING_STORAGE_DIR` — default `data/scripting-storage`
//! * `UNIFIED_SCRIPTING_HTTP_ALLOWLIST` — comma-separated hosts
//! * `UNIFIED_SCRIPTING_ALLOW_LOOPBACK` — `1` enables the loopback
//!   fetch exception (development only, default off)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::hooks::HookBus;
use uuid::Uuid;

use crate::engine::ScriptingEngine;
use crate::sdk::{SDK_VERSION, SdkHost};

/// Owner name used when a TypeScript module's hook forwarders are
/// registered on the bus (`ts:<module-id>`). Unregistering by this
/// owner removes exactly that module's forwarders.
pub fn hook_owner(module_id: &str) -> String {
    format!("ts:{module_id}")
}

/// Where script modules live on disk and where their storage goes.
#[derive(Debug, Clone)]
pub struct ScriptingPaths {
    pub modules_dir: PathBuf,
    pub storage_root: PathBuf,
}

/// Resolve the scripting paths from the environment (or the
/// defaults above). Relative paths resolve against the process
/// working directory, like the rest of the server.
pub fn resolve_paths() -> ScriptingPaths {
    let modules_dir = std::env::var("UNIFIED_SCRIPTING_MODULES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("modules"));
    let storage_root = std::env::var("UNIFIED_SCRIPTING_STORAGE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data/scripting-storage"));
    ScriptingPaths {
        modules_dir,
        storage_root,
    }
}

/// Extra hosts allowed to skip the public-IP check in
/// `http.fetch`, from the environment (comma-separated, empty by
/// default).
pub fn resolve_http_allowlist() -> Vec<String> {
    std::env::var("UNIFIED_SCRIPTING_HTTP_ALLOWLIST")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether the loopback fetch exception is enabled (development
/// only; default off).
pub fn resolve_allow_loopback() -> bool {
    std::env::var("UNIFIED_SCRIPTING_ALLOW_LOOPBACK")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Build the SDK host from the running server context. The config
/// snapshot is curated: only safe, non-secret values are exposed
/// to scripts (`app.mode`, worker count, the scripting settings
/// themselves). Secrets (`secret_key`, `salt_key`, database
/// credentials) are never in the snapshot.
pub fn build_host(ctx: &AppContext, paths: &ScriptingPaths) -> Arc<SdkHost> {
    let config = ctx.config.read().expect("config lock poisoned");
    let mut snapshot: HashMap<String, serde_json::Value> = HashMap::new();
    snapshot.insert(
        "app.mode".to_string(),
        serde_json::Value::String(config.app.mode.clone()),
    );
    snapshot.insert(
        "app.workers".to_string(),
        serde_json::Value::from(config.app.workers as u64),
    );
    snapshot.insert(
        "scripting.sdk_version".to_string(),
        serde_json::Value::String(SDK_VERSION.to_string()),
    );
    snapshot.insert(
        "scripting.modules_dir".to_string(),
        serde_json::Value::String(paths.modules_dir.to_string_lossy().into_owned()),
    );
    snapshot.insert(
        "scripting.storage_dir".to_string(),
        serde_json::Value::String(paths.storage_root.to_string_lossy().into_owned()),
    );
    drop(config);
    Arc::new(SdkHost {
        bus: Arc::clone(&ctx.hooks),
        config: snapshot,
        db: Arc::clone(&ctx.db),
        storage_root: paths.storage_root.clone(),
        http_allowlist: resolve_http_allowlist(),
        http_allow_loopback: resolve_allow_loopback(),
        kv: Arc::new(crate::sdk_db::KvStore::new()),
    })
}

/// Register one HookBus forwarder per event a module subscribed
/// to. When the event fires, the forwarder enqueues the call on
/// the scripting worker (fire-and-forget: the emitter never waits
/// on a script). The tenant is taken from the event payload's
/// `tenant_id` field when it parses as a UUID, so a script's
/// `db.query` sees the event's tenant; otherwise there is none
/// and `db.query` fails closed.
pub fn register_hook_forwarders(
    engine: &Arc<ScriptingEngine>,
    bus: &Arc<HookBus>,
    module_id: &str,
    events: &[String],
) {
    let owner = hook_owner(module_id);
    for event in events {
        let engine = Arc::clone(engine);
        let module_id = module_id.to_string();
        let event = event.clone();
        // The `move` closure below captures its own copy: `event`
        // is borrowed by `bus.register` first, so moving it into
        // the closure would borrow after move (E0505).
        let event_for_closure = event.clone();
        bus.register(
            &event,
            &owner,
            Arc::new(move |hook_ctx: &mut unified_core::hooks::HookContext| {
                let tenant = hook_ctx
                    .data
                    .get("tenant_id")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|raw| Uuid::parse_str(raw).ok());
                engine.emit_hook_async(&module_id, &event_for_closure, hook_ctx.data.clone(), tenant);
            }),
        );
    }
}

/// Remove a module's hook forwarders (on unload).
pub fn unregister_hook_forwarders(bus: &Arc<HookBus>, module_id: &str, events: &[String]) {
    let owner = hook_owner(module_id);
    for event in events {
        bus.unregister(event, &owner);
    }
}
