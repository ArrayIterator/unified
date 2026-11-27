// routes module - Handlers.
//
// Split from the former single-file module `src/modules/routes.rs`.
// The module root (`routes.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::routes::{
    OverrideSource, RedirectEntry, RouteChange, RouteEntry, RouteRegistry,
    ROUTE_CHANGED_EVENT,
};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{Arc, RwLock};

use super::views::{RouteChangeView, RouteView};
use super::helpers::{PERM_ROUTES_MANAGE, PERM_ROUTES_VIEW, caller, require_permission};
use super::requests::{ClearOverrideQuery, SetOverrideInput, emit_route_changed, parse_source};

// Handlers.

pub(crate) async fn list_routes(
    registry: web::Data<Arc<RouteRegistry>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = caller(&req)
        .and_then(|caller| require_permission(&ctx, &caller.role, PERM_ROUTES_VIEW))
    {
        return json::error_response(&error, debug);
    }
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

pub(crate) async fn get_route(
    registry: web::Data<Arc<RouteRegistry>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    name: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let name = name.into_inner();
    let result: UnifiedResult<RouteView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ROUTES_VIEW)?;
        registry
            .entry(&name)
            .map(RouteView::from)
            .ok_or_else(|| UnifiedError::not_found(format!("Unknown route \"{name}\"")))
    })();
    json::respond(result, debug)
}

pub(crate) async fn set_route_override(
    registry: web::Data<Arc<RouteRegistry>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    name: web::Path<String>,
    body: web::Json<SetOverrideInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let name = name.into_inner();
    let input = body.into_inner();
    let result: UnifiedResult<RouteChangeView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ROUTES_MANAGE)?;
        let change = registry.set_override(&name, &input.path, OverrideSource::Settings)?;
        emit_route_changed(&ctx, &change, &caller.actor);
        Ok(RouteChangeView::from(change))
    })();
    json::respond(result, debug)
}

pub(crate) async fn clear_route_override(
    registry: web::Data<Arc<RouteRegistry>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    name: web::Path<String>,
    query: web::Query<ClearOverrideQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let name = name.into_inner();
    let query = query.into_inner();
    let result: UnifiedResult<RouteChangeView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ROUTES_MANAGE)?;
        let source = parse_source(query.source, OverrideSource::Settings)?;
        let change = registry.clear_override(&name, source)?;
        emit_route_changed(&ctx, &change, &caller.actor);
        Ok(RouteChangeView::from(change))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_redirects(
    registry: web::Data<Arc<RouteRegistry>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = caller(&req)
        .and_then(|caller| require_permission(&ctx, &caller.role, PERM_ROUTES_VIEW))
    {
        return json::error_response(&error, debug);
    }
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
