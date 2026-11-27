// Maintenance module - maintenance window and mode HTTP handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;

// Handlers

pub(crate) async fn list_windows(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MAINTENANCE_VIEW)
    })() {
        return json::error_response(&error, ctx.is_debug());
    }
    state.refresh(Some(&ctx));
    let params = query.into_inner();
    let mut items: Vec<MaintenanceWindow> = state
        .windows
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|window| {
            matches_search(&format!("{} {}", window.title, window.description), &params)
                && params
                    .filters
                    .get("status")
                    .map_or(true, |status| window.status == *status)
                && params.filters.get("service").map_or(true, |service| {
                    window.affected_services.iter().any(|name| name == service)
                })
        })
        .collect();
    items = sort_windows(items, &params);
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_window(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateWindowRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MAINTENANCE_MANAGE)
    })() {
        return json::error_response(&error, debug);
    }
    state.refresh(Some(&ctx));
    let input = body.into_inner();
    let result: UnifiedResult<MaintenanceWindow> = (|| {
        validate_window_input(
            &input.title,
            input.starts_at,
            input.ends_at,
            &input.affected_services,
        )?;
        let now = Utc::now();
        let window = MaintenanceWindow {
            id: Uuid::now_v7(),
            title: input.title.trim().to_string(),
            description: input.description.trim().to_string(),
            starts_at: input.starts_at,
            ends_at: input.ends_at,
            affected_services: input
                .affected_services
                .iter()
                .map(|service| service.trim().to_string())
                .collect(),
            status: STATUS_SCHEDULED.to_string(),
            created_at: now,
            updated_at: now,
            activated_at: None,
            completed_at: None,
        };
        state.windows.insert(window.id, window.clone());
        state.record_history(
            window.id,
            "scheduled",
            format!(
                "Window \"{}\" scheduled from {} to {}",
                window.title,
                window.starts_at.to_rfc3339(),
                window.ends_at.to_rfc3339()
            ),
        );
        state.emit_window_event("maintenance.scheduled", &window);
        Ok(window)
    })();
    match result {
        Ok(window) => json::success_response(window, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_window(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MAINTENANCE_VIEW)
    })() {
        return json::error_response(&error, debug);
    }
    state.refresh(Some(&ctx));
    let result: UnifiedResult<MaintenanceWindow> = (|| {
        let id = parse_window_id(path.as_str())?;
        state
            .windows
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Maintenance window not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_window(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateWindowRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MAINTENANCE_MANAGE)
    })() {
        return json::error_response(&error, debug);
    }
    state.refresh(Some(&ctx));
    let result: UnifiedResult<MaintenanceWindow> = (|| {
        let id = parse_window_id(path.as_str())?;
        let input = body.into_inner();
        let mut window = state
            .windows
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Maintenance window not found"))?;
        if window.status == STATUS_ACTIVE {
            return Err(UnifiedError::conflict(
                "An active maintenance window cannot be edited; deactivate it first",
            ));
        }
        if window.status == STATUS_COMPLETED || window.status == STATUS_CANCELLED {
            return Err(UnifiedError::conflict(
                "A completed or cancelled maintenance window cannot be edited",
            ));
        }
        let title = input.title.as_deref().unwrap_or(&window.title).to_string();
        let starts_at = input.starts_at.unwrap_or(window.starts_at);
        let ends_at = input.ends_at.unwrap_or(window.ends_at);
        let affected = input
            .affected_services
            .clone()
            .unwrap_or_else(|| window.affected_services.clone());
        validate_window_input(&title, starts_at, ends_at, &affected)?;
        window.title = title.trim().to_string();
        if let Some(description) = input.description {
            window.description = description.trim().to_string();
        }
        window.starts_at = starts_at;
        window.ends_at = ends_at;
        window.affected_services = affected
            .iter()
            .map(|service| service.trim().to_string())
            .collect();
        window.updated_at = Utc::now();
        let snapshot = window.clone();
        drop(window);
        state.record_history(
            id,
            "scheduled",
            format!("Window \"{}\" updated", snapshot.title),
        );
        Ok(snapshot)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_window(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MAINTENANCE_MANAGE)
    })() {
        return json::error_response(&error, debug);
    }
    state.refresh(Some(&ctx));
    let result: UnifiedResult<Value> = (|| {
        let id = parse_window_id(path.as_str())?;
        let status = state
            .windows
            .get(&id)
            .map(|entry| entry.value().status.clone())
            .ok_or_else(|| UnifiedError::not_found("Maintenance window not found"))?;
        if status == STATUS_ACTIVE {
            return Err(UnifiedError::conflict(
                "An active maintenance window cannot be deleted; deactivate or cancel it first",
            ));
        }
        // Removal records a history entry and emits
        // `maintenance.window_deleted`, like the other lifecycle
        // transitions.
        state
            .remove_window(id)
            .ok_or_else(|| UnifiedError::not_found("Maintenance window not found"))?;
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn activate_window(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MAINTENANCE_MANAGE)
    })() {
        return json::error_response(&error, debug);
    }
    state.refresh(Some(&ctx));
    let result: UnifiedResult<MaintenanceWindow> = (|| {
        let id = parse_window_id(path.as_str())?;
        let mut window = state
            .windows
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Maintenance window not found"))?;
        if window.status == STATUS_ACTIVE {
            return Err(UnifiedError::conflict(
                "Maintenance window is already active",
            ));
        }
        if window.status != STATUS_SCHEDULED {
            return Err(UnifiedError::conflict(format!(
                "Only a scheduled window can be activated (current status: {})",
                window.status
            )));
        }
        window.status = STATUS_ACTIVE.to_string();
        window.activated_at = Some(Utc::now());
        window.updated_at = Utc::now();
        let snapshot = window.clone();
        drop(window);
        state.record_history(
            id,
            "activated",
            format!("Window \"{}\" activated manually", snapshot.title),
        );
        state.emit_window_event("maintenance.started", &snapshot);
        ctx.set_maintenance(true);
        state.emit(
            unified_core::hooks::events::APP_MAINTENANCE,
            json!({ "maintenance": true, "window_id": id }),
        );
        Ok(snapshot)
    })();
    json::respond(result, debug)
}

pub(crate) async fn deactivate_window(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MAINTENANCE_MANAGE)
    })() {
        return json::error_response(&error, debug);
    }
    state.refresh(Some(&ctx));
    let result: UnifiedResult<MaintenanceWindow> = (|| {
        let id = parse_window_id(path.as_str())?;
        let mut window = state
            .windows
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Maintenance window not found"))?;
        if window.status != STATUS_ACTIVE {
            return Err(UnifiedError::conflict(format!(
                "Only an active window can be deactivated (current status: {})",
                window.status
            )));
        }
        window.status = STATUS_COMPLETED.to_string();
        window.completed_at = Some(Utc::now());
        window.updated_at = Utc::now();
        let snapshot = window.clone();
        drop(window);
        state.record_history(
            id,
            "deactivated",
            format!("Window \"{}\" ended manually", snapshot.title),
        );
        state.emit_window_event("maintenance.ended", &snapshot);
        if state.active_window().is_none() {
            ctx.set_maintenance(false);
            state.emit(
                unified_core::hooks::events::APP_MAINTENANCE,
                json!({ "maintenance": false, "window_id": id }),
            );
        }
        Ok(snapshot)
    })();
    json::respond(result, debug)
}

pub(crate) async fn cancel_window(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MAINTENANCE_MANAGE)
    })() {
        return json::error_response(&error, debug);
    }
    state.refresh(Some(&ctx));
    let result: UnifiedResult<MaintenanceWindow> = (|| {
        let id = parse_window_id(path.as_str())?;
        let mut window = state
            .windows
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Maintenance window not found"))?;
        if window.status == STATUS_ACTIVE {
            return Err(UnifiedError::conflict(
                "An active window cannot be cancelled; deactivate it first",
            ));
        }
        if window.status == STATUS_CANCELLED {
            return Err(UnifiedError::conflict(
                "Maintenance window is already cancelled",
            ));
        }
        if window.status == STATUS_COMPLETED {
            return Err(UnifiedError::conflict(
                "A completed window cannot be cancelled",
            ));
        }
        window.status = STATUS_CANCELLED.to_string();
        window.updated_at = Utc::now();
        let snapshot = window.clone();
        drop(window);
        state.record_history(
            id,
            "cancelled",
            format!("Window \"{}\" cancelled", snapshot.title),
        );
        state.emit_window_event("maintenance.cancelled", &snapshot);
        Ok(snapshot)
    })();
    json::respond(result, debug)
}

pub(crate) async fn current_status(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MAINTENANCE_VIEW)
    })() {
        return json::error_response(&error, ctx.is_debug());
    }
    state.refresh(Some(&ctx));
    let active = state.active_window();
    let next = state.next_scheduled_window();
    json::success_response(
        json!({
            "maintenance": ctx.is_maintenance(),
            "active_window": active,
            "next_window": next,
        }),
        200,
    )
}

pub(crate) async fn list_history(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MAINTENANCE_VIEW)
    })() {
        return json::error_response(&error, ctx.is_debug());
    }
    state.refresh(Some(&ctx));
    let params = query.into_inner();
    let mut items: Vec<MaintenanceHistoryEntry> = state
        .history
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            matches_search(&entry.message, &params)
                && params.filters.get("window_id").map_or(true, |id| {
                    entry.window_id.to_string() == *id
                })
                && params
                    .filters
                    .get("event")
                    .map_or(true, |event| entry.event == *event)
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn run_tick(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MAINTENANCE_MANAGE)
    })() {
        return json::error_response(&error, ctx.is_debug());
    }
    let changed = state.refresh(Some(&ctx));
    json::success_response(
        json!({
            "transitions": changed,
            "maintenance": ctx.is_maintenance(),
            "active_window": state.active_window(),
        }),
        200,
    )
}

pub(crate) async fn set_maintenance_mode(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<MaintenanceModeRequest>,
) -> HttpResponse {
    if let Err(error) = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MAINTENANCE_MANAGE)
    })() {
        return json::error_response(&error, ctx.is_debug());
    }
    state.refresh(Some(&ctx));
    let input = body.into_inner();
    if input.enabled && state.active_window().is_some() {
        return json::success_response(
            json!({
                "maintenance": true,
                "active_window": state.active_window(),
                "note": "Maintenance mode was already active through a running window",
            }),
            200,
        );
    }
    if !input.enabled && state.active_window().is_some() {
        return json::error_response(
            &UnifiedError::conflict(
                "Maintenance mode cannot be turned off while a window is active; deactivate the window instead",
            ),
            ctx.is_debug(),
        );
    }
    // A manual override owns the flag until it is turned off
    // explicitly: while it is set, `refresh` never syncs the flag
    // back to the (empty) window state.
    state
        .manual_override
        .store(input.enabled, Ordering::SeqCst);
    ctx.set_maintenance(input.enabled);
    state.emit(
        unified_core::hooks::events::APP_MAINTENANCE,
        json!({
            "maintenance": input.enabled,
            "message": input.message,
        }),
    );
    json::success_response(
        json!({
            "maintenance": input.enabled,
            "active_window": state.active_window(),
            "message": input.message,
        }),
        200,
    )
}
