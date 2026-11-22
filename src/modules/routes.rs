// Routes module — admin API for customizable page routes.
//
// HTTP API under `/api/v1/routes`:
//
//   GET    /                    list named routes (paginated, filterable)
//   GET    /{name}              one named route
//   PUT    /{name}              set a settings override
//                                {"path": "/app", "actor": "admin"}
//   DELETE /{name}?source=...   clear an override (default source=settings)
//   GET    /redirects           list active old-path redirects (paginated)
//
// Every override change is announced on the hook bus as
// `route.changed` with `{"route", "old_path", "new_path", "source",
// "actor", "changed_at"}`. The audit module records the event once
// `"route.changed"` is added to its audited event list (see the
// snippet in the coordinator report); the payload already carries
// `actor`, `route`, `old_path`, and `new_path`, which is exactly what
// the audit recorder reads. No credential is involved in any route
// override, and none is ever logged or returned.
//
// The registry itself is the store: overrides live in memory in the
// `RouteRegistry` (same durability as the settings module's store).
// There is exactly one registry: the shared instance on `AppContext`
// (`ctx.routes`), seeded at startup from the `routes:` config
// section (`RoutesSection`) with the legacy `app.dashboard_path`
// config value as a deprecated fallback for `user_dashboard`
// (`routes.user_dashboard` is canonical; the legacy value only
// applies when the canonical one is unset — see
// `RouteRegistry::apply_legacy_dashboard_path`). `init` attaches
// that shared instance to this module, so the admin API, the server
// redirect handler, and the theme engine all read and write the
// same overrides.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::routes::{
    OverrideSource, RedirectEntry, RouteChange, RouteEntry, RouteRegistry,
    ROUTE_CHANGED_EVENT,
};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{Arc, RwLock};

// ---------------------------------------------------------------------------
// Views.
// ---------------------------------------------------------------------------

/// One route as listed by the API: registry data plus the override
/// value currently in force at each layer, so the admin UI can show
/// what clearing an override falls back to.
#[derive(Debug, Clone, Serialize)]
struct RouteView {
    name: String,
    default_path: String,
    current_path: String,
    source: OverrideSource,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_path: Option<String>,
}

impl From<RouteEntry> for RouteView {
    fn from(entry: RouteEntry) -> Self {
        Self {
            name: entry.name,
            default_path: entry.default_path,
            current_path: entry.current_path,
            source: entry.source,
            description: entry.description,
            updated_at: entry.updated_at,
            previous_path: entry.previous_path,
        }
    }
}

/// One override change as returned by PUT / DELETE, including the
/// redirect the old path now serves (when the path actually changed).
#[derive(Debug, Clone, Serialize)]
struct RouteChangeView {
    route: String,
    old_path: String,
    new_path: String,
    source: OverrideSource,
    changed: bool,
    changed_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    redirect_from: Option<String>,
}

impl From<RouteChange> for RouteChangeView {
    fn from(change: RouteChange) -> Self {
        let redirect_from = if change.changed {
            Some(change.old_path.clone())
        } else {
            None
        };
        Self {
            route: change.route,
            old_path: change.old_path,
            new_path: change.new_path,
            source: change.source,
            changed: change.changed,
            changed_at: change.changed_at,
            redirect_from,
        }
    }
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SetOverrideInput {
    path: String,
    /// Who made the change, for the audit trail. Defaults to "admin".
    #[serde(default)]
    actor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClearOverrideQuery {
    /// Which override layer to clear: "settings" (default) or "config".
    #[serde(default)]
    source: Option<String>,
}

fn actor_from(input_actor: Option<String>) -> UnifiedResult<String> {
    let actor = input_actor
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "admin".to_string());
    if actor.chars().count() > 128 {
        return Err(UnifiedError::bad_request(
            "actor must be at most 128 characters",
        ));
    }
    Ok(actor)
}

fn parse_source(raw: Option<String>, default: OverrideSource) -> UnifiedResult<OverrideSource> {
    match raw.as_deref() {
        None => Ok(default),
        Some("settings") => Ok(OverrideSource::Settings),
        Some("config") => Ok(OverrideSource::Config),
        Some(other) => Err(UnifiedError::bad_request(format!(
            "source must be \"settings\" or \"config\", got \"{other}\""
        ))),
    }
}

/// Announce an override change on the hook bus. The payload is the
/// audit record: actor, route name, old path, new path, source.
fn emit_route_changed(ctx: &AppContext, change: &RouteChange, actor: &str) {
    ctx.hooks.emit(
        ROUTE_CHANGED_EVENT,
        json!({
            "route": change.route,
            "old_path": change.old_path,
            "new_path": change.new_path,
            "source": change.source.as_str(),
            "actor": actor,
            "changed_at": change.changed_at,
        }),
    );
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn list_routes(
    registry: web::Data<Arc<RouteRegistry>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let pagination = params.normalized();
    let sort = params.allowed_sort(
        &["name", "default_path", "current_path", "source", "updated_at"],
        "name",
    );
    let descending = pagination.order == "desc";
    let source_filter = params.filters.get("source").cloned();
    let name_filter = params.filters.get("name").cloned();
    let search = params
        .search
        .clone()
        .map(|value| value.to_lowercase());
    let mut items: Vec<RouteView> = registry
        .list()
        .into_iter()
        .map(RouteView::from)
        .filter(|entry| {
            if let Some(source) = &source_filter {
                if entry.source.as_str() != source.as_str() {
                    return false;
                }
            }
            if let Some(name) = &name_filter {
                if entry.name != *name {
                    return false;
                }
            }
            if let Some(needle) = &search {
                let haystack = format!(
                    "{} {} {} {}",
                    entry.name, entry.default_path, entry.current_path, entry.description
                )
                .to_lowercase();
                if !haystack.contains(needle.as_str()) {
                    return false;
                }
            }
            true
        })
        .collect();
    items.sort_by(|a, b| match sort.as_str() {
        "default_path" => a.default_path.cmp(&b.default_path),
        "current_path" => a.current_path.cmp(&b.current_path),
        "source" => a.source.as_str().cmp(b.source.as_str()),
        "updated_at" => a.updated_at.cmp(&b.updated_at),
        _ => a.name.cmp(&b.name),
    });
    if descending {
        items.reverse();
    }
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<RouteView> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

async fn get_route(
    registry: web::Data<Arc<RouteRegistry>>,
    ctx: web::Data<Arc<AppContext>>,
    name: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let name = name.into_inner();
    let result: UnifiedResult<RouteView> = registry
        .entry(&name)
        .map(RouteView::from)
        .ok_or_else(|| UnifiedError::not_found(format!("Unknown route \"{name}\"")));
    json::respond(result, debug)
}

async fn set_route_override(
    registry: web::Data<Arc<RouteRegistry>>,
    ctx: web::Data<Arc<AppContext>>,
    name: web::Path<String>,
    body: web::Json<SetOverrideInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let name = name.into_inner();
    let input = body.into_inner();
    let result: UnifiedResult<RouteChangeView> = (|| {
        let actor = actor_from(input.actor)?;
        let change = registry.set_override(&name, &input.path, OverrideSource::Settings)?;
        emit_route_changed(&ctx, &change, &actor);
        Ok(RouteChangeView::from(change))
    })();
    json::respond(result, debug)
}

async fn clear_route_override(
    registry: web::Data<Arc<RouteRegistry>>,
    ctx: web::Data<Arc<AppContext>>,
    name: web::Path<String>,
    query: web::Query<ClearOverrideQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let name = name.into_inner();
    let query = query.into_inner();
    let result: UnifiedResult<RouteChangeView> = (|| {
        let source = parse_source(query.source, OverrideSource::Settings)?;
        let change = registry.clear_override(&name, source)?;
        emit_route_changed(&ctx, &change, "admin");
        Ok(RouteChangeView::from(change))
    })();
    json::respond(result, debug)
}

async fn list_redirects(
    registry: web::Data<Arc<RouteRegistry>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let pagination = params.normalized();
    let sort = params.allowed_sort(&["route", "old_path", "new_path", "changed_at"], "changed_at");
    let descending = pagination.order == "desc";
    let route_filter = params.filters.get("route").cloned();
    let search = params
        .search
        .clone()
        .map(|value| value.to_lowercase());
    let mut items: Vec<RedirectEntry> = registry
        .list_redirects()
        .into_iter()
        .filter(|entry| {
            if let Some(route) = &route_filter {
                if entry.route != *route {
                    return false;
                }
            }
            if let Some(needle) = &search {
                let haystack = format!("{} {} {}", entry.route, entry.old_path, entry.new_path)
                    .to_lowercase();
                if !haystack.contains(needle.as_str()) {
                    return false;
                }
            }
            true
        })
        .collect();
    items.sort_by(|a, b| match sort.as_str() {
        "route" => a.route.cmp(&b.route),
        "old_path" => a.old_path.cmp(&b.old_path),
        "new_path" => a.new_path.cmp(&b.new_path),
        _ => a.changed_at.cmp(&b.changed_at),
    });
    if descending {
        items.reverse();
    }
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<RedirectEntry> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

// ---------------------------------------------------------------------------
// Redirect helper for the server wiring.
//
// `resolve_redirect` is the function the HTTP server calls before
// its normal routes: when it returns `Some(target)`, the server
// answers 301 with a `Location: <target>` header and never reaches
// the normal router. Because every stored redirect target is a
// route's current effective path, a redirect never points at another
// old path and redirects never chain. See the server.rs snippet in
// the coordinator report for the exact installation point.
// ---------------------------------------------------------------------------

/// Resolve an incoming path to its redirect target, if the path is a
/// recorded old path of a renamed route.
pub fn resolve_redirect(registry: &RouteRegistry, path: &str) -> Option<String> {
    registry.resolve_redirect(path)
}

/// Build a 301 response for an old path, or `None` when the path is
/// not a recorded old path.
pub fn redirect_response(registry: &RouteRegistry, path: &str) -> Option<HttpResponse> {
    let target = registry.resolve_redirect(path)?;
    Some(
        HttpResponse::MovedPermanently()
            .insert_header(("Location", target))
            .finish(),
    )
}

/// Resolve a named route to its current effective path. Handlers and
/// templates must use this instead of embedding a literal path.
pub fn route_url(registry: &RouteRegistry, name: &str) -> String {
    registry.route_url(name)
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct RoutesModule {
    /// The registry this module serves. A lock (not a plain field)
    /// so `init` can replace the construction-time registry with the
    /// single shared instance from `AppContext`: the module is built
    /// with a fresh registry by `register`, and the shared instance
    /// only becomes available in `init` through the context.
    registry: RwLock<Arc<RouteRegistry>>,
}

impl RoutesModule {
    pub fn new() -> Self {
        Self {
            registry: RwLock::new(Arc::new(RouteRegistry::new())),
        }
    }

    /// Build the module around a registry the integrator created (for
    /// example the shared registry stored on `AppContext`). `init`
    /// replaces it with `ctx.routes` anyway, so passing anything else
    /// here only matters before `init` runs.
    pub fn with_registry(registry: Arc<RouteRegistry>) -> Self {
        Self {
            registry: RwLock::new(registry),
        }
    }

    /// The registry this module serves, so the server wiring and the
    /// theme engine can share the same instance. After `init` this is
    /// the shared `AppContext` registry.
    pub fn registry(&self) -> Arc<RouteRegistry> {
        self.registry
            .read()
            .expect("route registry lock poisoned")
            .clone()
    }
}

impl Default for RoutesModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for RoutesModule {
    fn id(&self) -> &'static str {
        "routes"
    }

    fn name(&self) -> &'static str {
        "Routes"
    }

    fn description(&self) -> &'static str {
        "Customizable page routes: named auth and dashboard paths with config and settings overrides, validation, and permanent redirects for renamed paths."
    }

    fn requires(&self) -> &[&'static str] {
        &["settings", "audit"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        // Unify on the single shared registry from the context: the
        // same instance `AppContext::new` seeded from the `routes:`
        // config section, the server redirect handler resolves
        // through, and the theme engine is attached to. The registry
        // this module was constructed with held no overrides yet —
        // nothing writes to it before `init` — so replacing it loses
        // nothing, and `routes:` overrides now reach this module's
        // admin API as well as the server and the theme engine.
        // The legacy `app.dashboard_path` fallback was already
        // applied to the shared registry by `AppContext::new`, so
        // there is nothing left to seed here.
        *self.registry.write().expect("route registry lock poisoned") =
            Arc::clone(&ctx.routes);
        Ok(())
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/routes")
                .app_data(web::Data::new(self.registry()))
                // Static segment first: "/redirects" must be registered
                // before "/{name}" or it would match as a route name.
                .route("/redirects", web::get().to(list_redirects))
                .route("", web::get().to(list_routes))
                .route("/{name}", web::get().to(get_route))
                .route("/{name}", web::put().to(set_route_override))
                .route("/{name}", web::delete().to(clear_route_override)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/routes",
                owner: "routes",
                description: "List named routes with current path, default path, override source, and last change (paginated).",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/routes/{name}",
                owner: "routes",
                description: "Get one named route.",
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/routes/{name}",
                owner: "routes",
                description: "Set a settings override for a named route (validated; audit-logged; emits route.changed).",
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/routes/{name}",
                owner: "routes",
                description: "Clear an override for a named route (falls back to config or default).",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/routes/redirects",
                owner: "routes",
                description: "List active old-path redirects (paginated).",
            },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(RoutesModule::new()));
}
