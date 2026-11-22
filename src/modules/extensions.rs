// Extensions module — the Modules / Extensions Manager.
//
// Lists every registered module with its enabled state, enables and
// disables modules, and lists the hook events registered on the bus.
// Disabling a core module is refused (409). The enabled state is kept in
// memory; it is advisory — it gates the state reported by this API and
// the `module.enabled` / `module.disabled` events, but it does not
// unregister already-mounted routes. Full dynamic unload (route rebuild
// without a restart) is a follow-up; see the TODO note in `set_enabled`.

use crate::core::context::AppContext;
use crate::core::error::UnifiedError;
use crate::core::hooks::events;
use crate::core::json::{error_response, success_response};
use crate::core::modules::{Module, ModuleInfo, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use dashmap::DashMap;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct ExtensionsModule {
    /// Explicit enabled state per module id. Absent means enabled.
    /// This map is shared with the route handlers through `web::Data`,
    /// so the state they read and write is the same one this module owns.
    enabled: Arc<DashMap<String, bool>>,
}

impl ExtensionsModule {
    pub fn new() -> Self {
        Self {
            enabled: Arc::new(DashMap::new()),
        }
    }
}

pub type EnabledState = Arc<DashMap<String, bool>>;

fn is_enabled(state: &EnabledState, id: &str) -> bool {
    state.get(id).map(|entry| *entry.value()).unwrap_or(true)
}

impl Default for ExtensionsModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ExtensionsModule::new()));
}

/// Module summary plus its enabled state.
#[derive(Debug, Clone, Serialize)]
pub struct ExtensionInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    pub description: &'static str,
    pub is_core: bool,
    pub dependencies: Vec<&'static str>,
    pub enabled: bool,
}

impl From<(&ModuleInfo, bool)> for ExtensionInfo {
    fn from((info, enabled): (&ModuleInfo, bool)) -> Self {
        Self {
            id: info.id,
            name: info.name,
            version: info.version,
            description: info.description,
            is_core: info.is_core,
            dependencies: info.dependencies.clone(),
            enabled,
        }
    }
}

/// One hook event and the modules that handle it.
#[derive(Debug, Clone, Serialize)]
pub struct HookEventInfo {
    pub event: String,
    pub owners: Vec<String>,
    pub handler_count: usize,
}

fn list_extensions(
    ctx: web::Data<Arc<AppContext>>,
    enabled_state: web::Data<EnabledState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let pagination = query.normalized();
    let search = pagination.search.clone().map(|s| s.to_lowercase());
    let enabled_filter = pagination
        .filters
        .get("enabled")
        .and_then(|value| value.parse::<bool>().ok());
    let core_filter = pagination
        .filters
        .get("is_core")
        .and_then(|value| value.parse::<bool>().ok());

    let mut items: Vec<ExtensionInfo> = ctx
        .modules
        .summary()
        .iter()
        .map(|info| ExtensionInfo::from((info, is_enabled(&enabled_state, info.id))))
        .filter(|info| {
            if let Some(search) = &search {
                let haystack = format!("{} {} {}", info.id, info.name, info.description)
                    .to_lowercase();
                if !haystack.contains(search.as_str()) {
                    return false;
                }
            }
            if let Some(enabled) = enabled_filter {
                if info.enabled != enabled {
                    return false;
                }
            }
            if let Some(is_core) = core_filter {
                if info.is_core != is_core {
                    return false;
                }
            }
            true
        })
        .collect();
    items.sort_by(|a, b| a.id.cmp(b.id));
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start < items.len() {
        items[start..end].to_vec()
    } else {
        Vec::new()
    };
    success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

fn get_extension(
    ctx: web::Data<Arc<AppContext>>,
    enabled_state: web::Data<EnabledState>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let key = id.into_inner();
    match ctx.modules.get(&key) {
        Some(found) => {
            let info = ModuleInfo {
                id: found.id(),
                name: found.name(),
                version: found.version(),
                description: found.description(),
                is_core: found.is_core(),
                dependencies: found.dependencies(),
            };
            success_response(
                ExtensionInfo::from((&info, is_enabled(&enabled_state, &key))),
                200,
            )
        }
        None => error_response(
            &UnifiedError::not_found(format!("Module \"{key}\" not found")),
            debug,
        ),
    }
}

fn set_enabled(
    ctx: web::Data<Arc<AppContext>>,
    enabled_state: web::Data<EnabledState>,
    id: web::Path<String>,
    enabled: bool,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let key = id.into_inner();
    let found = match ctx.modules.get(&key) {
        Some(found) => found,
        None => {
            return error_response(
                &UnifiedError::not_found(format!("Module \"{key}\" not found")),
                debug,
            )
        }
    };
    if !enabled && found.is_core() {
        return error_response(
            &UnifiedError::conflict(format!(
                "Core module \"{key}\" cannot be disabled"
            )),
            debug,
        );
    }
    enabled_state.insert(key.clone(), enabled);
    if enabled {
        ctx.hooks.emit(
            events::MODULE_ENABLED,
            serde_json::json!({ "id": key }),
        );
    } else {
        ctx.hooks.emit(
            events::MODULE_DISABLED,
            serde_json::json!({ "id": key }),
        );
    }
    // TODO: disabling a module currently only flips the reported state
    // and emits the event; routes mounted at startup stay mounted until
    // the process restarts. Full dynamic unload needs a route rebuild
    // (server restart or a reloadable route table) and is a follow-up.
    let info = ModuleInfo {
        id: found.id(),
        name: found.name(),
        version: found.version(),
        description: found.description(),
        is_core: found.is_core(),
        dependencies: found.dependencies(),
    };
    success_response(ExtensionInfo::from((&info, enabled)), 200)
}

fn enable_extension(
    ctx: web::Data<Arc<AppContext>>,
    enabled_state: web::Data<EnabledState>,
    id: web::Path<String>,
) -> HttpResponse {
    set_enabled(ctx, enabled_state, id, true)
}

fn disable_extension(
    ctx: web::Data<Arc<AppContext>>,
    enabled_state: web::Data<EnabledState>,
    id: web::Path<String>,
) -> HttpResponse {
    set_enabled(ctx, enabled_state, id, false)
}

fn list_hooks(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let pagination = query.normalized();
    let search = pagination.search.clone().map(|s| s.to_lowercase());

    let mut by_event: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (event, owner) in ctx.hooks.registrations() {
        by_event.entry(event).or_default().push(owner);
    }
    let mut events: Vec<HookEventInfo> = by_event
        .into_iter()
        .map(|(event, owners)| {
            let handler_count = owners.len();
            let mut owners = owners;
            owners.sort();
            owners.dedup();
            HookEventInfo {
                event,
                owners,
                handler_count,
            }
        })
        .filter(|info| {
            if let Some(search) = &search {
                info.event.to_lowercase().contains(search.as_str())
            } else {
                true
            }
        })
        .collect();
    events.sort_by(|a, b| a.event.cmp(&b.event));
    let total = events.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(events.len());
    let items = if start < events.len() {
        events[start..end].to_vec()
    } else {
        Vec::new()
    };
    success_response(PageResult::new(items, total, &pagination).to_json(), 200)
}

impl Module for ExtensionsModule {
    fn id(&self) -> &'static str {
        "extensions"
    }

    fn name(&self) -> &'static str {
        "Extensions"
    }

    fn description(&self) -> &'static str {
        "Modules / Extensions Manager: lists, enables and disables modules, and lists registered hook events."
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        // The enabled-state map is shared with the handlers through
        // `web::Data`, so the state they read and write is the same map
        // this module instance owns.
        cfg.service(
            web::scope("/api/v1/extensions")
                .app_data(web::Data::new(self.enabled.clone()))
                .route("/hooks", web::get().to(list_hooks))
                .route("", web::get().to(list_extensions))
                .route("/{id}", web::get().to(get_extension))
                .route("/{id}/enable", web::post().to(enable_extension))
                .route("/{id}/disable", web::post().to(disable_extension)),
        );
    }
}
