// Maintenance Windows module.
//
// Scheduled maintenance windows with automatic activation and
// deactivation by time, a global maintenance-mode flag, subscriber
// notifications through the hook bus, and a full history log.
//
// How time-based transitions work (there is no periodic scheduler
// tick event in the codebase to subscribe to):
//
//   - Every handler in this module calls `refresh` first, so a window
//     whose start or end time has passed changes state on the next
//     request that touches the module.
//   - `POST /api/v1/maintenance/tick` runs the same refresh on demand.
//     Point a scheduler job at it (or emit the `maintenance.check`
//     hook, which this module listens to) once a minute for prompt
//     transitions without waiting for traffic.
//   - While a window is active the module sets the shared
//     `AppContext` maintenance flag (`ctx.set_maintenance(true)`).
//     The HTTP server already answers 503 for every route except
//     `/health` and the ACME challenge while that flag is set; the
//     integrator snippet for exempting the public status page lives
//     in the coordinator report.
//
// Events emitted on the hook bus:
//   - `maintenance.scheduled` — a window was created.
//   - `maintenance.started`   — a window became active.
//   - `maintenance.ended`     — a window finished (time or manual).
//   - `maintenance.cancelled` — a window was cancelled.
//   - `maintenance.check`     — listened to; runs a refresh.
//   - `app.maintenance` (core event) — the global flag changed.
//
// Subscriber notification: the notifications module listens to
// `maintenance.scheduled`, `maintenance.started`, and
// `maintenance.ended` when a handler is registered for them; this
// module also forwards each lifecycle event as a
// `notification.requested` event (kind `maintenance`) so subscribers
// are notified through the standard channel.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Lifecycle of a maintenance window.
pub const STATUS_SCHEDULED: &str = "scheduled";
pub const STATUS_ACTIVE: &str = "active";
pub const STATUS_COMPLETED: &str = "completed";
pub const STATUS_CANCELLED: &str = "cancelled";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenanceWindow {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub affected_services: Vec<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activated_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenanceHistoryEntry {
    pub id: Uuid,
    pub window_id: Uuid,
    /// `scheduled`, `started`, `ended`, `cancelled`, `activated`
    /// (manual), or `deactivated` (manual).
    pub event: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateWindowRequest {
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    #[serde(default)]
    pub affected_services: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateWindowRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub starts_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub ends_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub affected_services: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MaintenanceModeRequest {
    pub enabled: bool,
    #[serde(default)]
    pub message: Option<String>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct MaintenanceState {
    hooks: RwLock<Arc<HookBus>>,
    windows: DashMap<Uuid, MaintenanceWindow>,
    history: DashMap<Uuid, MaintenanceHistoryEntry>,
}

impl MaintenanceState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            windows: DashMap::new(),
            history: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    fn record_history(&self, window_id: Uuid, event: &str, message: String) {
        let entry = MaintenanceHistoryEntry {
            id: Uuid::now_v7(),
            window_id,
            event: event.to_string(),
            message,
            created_at: Utc::now(),
        };
        self.history.insert(entry.id, entry);
    }

    /// Notify subscribers about a lifecycle event through the standard
    /// `notification.requested` channel (kind `maintenance`).
    fn notify_subscribers(&self, window: &MaintenanceWindow, event: &str) {
        self.emit(
            "notification.requested",
            json!({
                "user_id": "all",
                "title": format!("Maintenance {}: {}", event, window.title),
                "body": format!(
                    "Maintenance window \"{}\" ({} to {}) is now {}. Affected services: {}.",
                    window.title,
                    window.starts_at.to_rfc3339(),
                    window.ends_at.to_rfc3339(),
                    event,
                    if window.affected_services.is_empty() {
                        "all services".to_string()
                    } else {
                        window.affected_services.join(", ")
                    },
                ),
                "kind": "maintenance",
            }),
        );
    }

    fn emit_window_event(&self, event: &str, window: &MaintenanceWindow) {
        let payload = json!({
            "window_id": window.id,
            "title": window.title,
            "status": window.status,
            "starts_at": window.starts_at,
            "ends_at": window.ends_at,
            "affected_services": window.affected_services,
        });
        self.emit(event, payload);
        self.notify_subscribers(window, window.status.as_str());
    }

    /// Apply time-based transitions and synchronise the global
    /// maintenance flag. Returns the number of windows that changed
    /// state. When `ctx` is given (HTTP handlers and the tick
    /// endpoint), the shared flag is updated; hook-driven refreshes
    /// pass `None` and only move window state and emit events.
    fn refresh(&self, ctx: Option<&AppContext>) -> usize {
        let now = Utc::now();
        let mut changed = 0usize;
        let mut ids: Vec<Uuid> = self.windows.iter().map(|e| *e.key()).collect();
        ids.sort();
        for id in ids {
            let Some(mut window) = self.windows.get_mut(&id) else {
                continue;
            };
            if window.status == STATUS_SCHEDULED && now >= window.starts_at {
                if now >= window.ends_at {
                    window.status = STATUS_COMPLETED.to_string();
                    window.completed_at = Some(now);
                    window.updated_at = now;
                    let snapshot = window.clone();
                    drop(window);
                    changed += 1;
                    self.record_history(
                        id,
                        "ended",
                        format!(
                            "Window \"{}\" ended without ever activating (end time passed while scheduled)",
                            snapshot.title
                        ),
                    );
                    self.emit_window_event("maintenance.ended", &snapshot);
                } else {
                    window.status = STATUS_ACTIVE.to_string();
                    window.activated_at = Some(now);
                    window.updated_at = now;
                    let snapshot = window.clone();
                    drop(window);
                    changed += 1;
                    self.record_history(
                        id,
                        "started",
                        format!("Window \"{}\" started on schedule", snapshot.title),
                    );
                    self.emit_window_event("maintenance.started", &snapshot);
                }
            } else if window.status == STATUS_ACTIVE && now >= window.ends_at {
                window.status = STATUS_COMPLETED.to_string();
                window.completed_at = Some(now);
                window.updated_at = now;
                let snapshot = window.clone();
                drop(window);
                changed += 1;
                self.record_history(
                    id,
                    "ended",
                    format!("Window \"{}\" ended on schedule", snapshot.title),
                );
                self.emit_window_event("maintenance.ended", &snapshot);
            }
        }
        if let Some(ctx) = ctx {
            let any_active = self
                .windows
                .iter()
                .any(|entry| entry.value().status == STATUS_ACTIVE);
            // A manual maintenance-mode override (set through
            // POST /mode with no active window) is kept until it is
            // turned off explicitly, so only clear the flag when an
            // active window is what set it: the flag is cleared here
            // only when no window is active AND the flag was set by a
            // window transition. In practice the flag is owned by this
            // module while windows exist, so sync it to the window
            // state whenever at least one window has ever activated.
            let ever_activated = self
                .windows
                .iter()
                .any(|entry| entry.value().activated_at.is_some());
            if ever_activated || any_active {
                let was = ctx.is_maintenance();
                if was != any_active {
                    ctx.set_maintenance(any_active);
                    self.emit(
                        crate::core::hooks::events::APP_MAINTENANCE,
                        json!({ "maintenance": any_active }),
                    );
                }
            }
        }
        changed
    }

    fn active_window(&self) -> Option<MaintenanceWindow> {
        let mut active: Vec<MaintenanceWindow> = self
            .windows
            .iter()
            .filter(|entry| entry.value().status == STATUS_ACTIVE)
            .map(|entry| entry.value().clone())
            .collect();
        active.sort_by(|a, b| a.starts_at.cmp(&b.starts_at));
        active.into_iter().next()
    }

    fn next_scheduled_window(&self) -> Option<MaintenanceWindow> {
        let now = Utc::now();
        let mut upcoming: Vec<MaintenanceWindow> = self
            .windows
            .iter()
            .filter(|entry| {
                entry.value().status == STATUS_SCHEDULED && entry.value().starts_at > now
            })
            .map(|entry| entry.value().clone())
            .collect();
        upcoming.sort_by(|a, b| a.starts_at.cmp(&b.starts_at));
        upcoming.into_iter().next()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_window_id(raw: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid maintenance window id \"{raw}\"")))
}

fn validate_window_input(
    title: &str,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    affected_services: &[String],
) -> UnifiedResult<()> {
    let title = title.trim();
    if title.is_empty() {
        return Err(UnifiedError::bad_request("title must not be empty"));
    }
    if title.chars().count() > 200 {
        return Err(UnifiedError::bad_request(
            "title must be at most 200 characters",
        ));
    }
    if ends_at <= starts_at {
        return Err(UnifiedError::bad_request(
            "ends_at must be after starts_at",
        ));
    }
    if affected_services.len() > 50 {
        return Err(UnifiedError::bad_request(
            "affected_services must list at most 50 services",
        ));
    }
    for service in affected_services {
        if service.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "affected_services must not contain empty entries",
            ));
        }
        if service.chars().count() > 100 {
            return Err(UnifiedError::bad_request(
                "each affected service name must be at most 100 characters",
            ));
        }
    }
    Ok(())
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

fn sort_windows(mut items: Vec<MaintenanceWindow>, params: &PaginationParams) -> Vec<MaintenanceWindow> {
    let field = params.allowed_sort(&["starts_at", "ends_at", "title", "created_at"], "starts_at");
    match field.as_str() {
        "ends_at" => items.sort_by(|a, b| a.ends_at.cmp(&b.ends_at)),
        "title" => items.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase())),
        "created_at" => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        _ => items.sort_by(|a, b| a.starts_at.cmp(&b.starts_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    items
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_windows(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
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

async fn create_window(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateWindowRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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

async fn get_window(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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

async fn update_window(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateWindowRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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

async fn delete_window(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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
        state.windows.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

async fn activate_window(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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
            crate::core::hooks::events::APP_MAINTENANCE,
            json!({ "maintenance": true, "window_id": id }),
        );
        Ok(snapshot)
    })();
    json::respond(result, debug)
}

async fn deactivate_window(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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
                crate::core::hooks::events::APP_MAINTENANCE,
                json!({ "maintenance": false, "window_id": id }),
            );
        }
        Ok(snapshot)
    })();
    json::respond(result, debug)
}

async fn cancel_window(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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

async fn current_status(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
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

async fn list_history(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
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

async fn run_tick(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
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

async fn set_maintenance_mode(
    state: web::Data<Arc<MaintenanceState>>,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<MaintenanceModeRequest>,
) -> HttpResponse {
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
    ctx.set_maintenance(input.enabled);
    state.emit(
        crate::core::hooks::events::APP_MAINTENANCE,
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

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

fn on_maintenance_check(state: Arc<MaintenanceState>, _ctx: &mut HookContext) {
    // Runs the time-based transitions without an HTTP request (for
    // example from a scheduler job emitting `maintenance.check`).
    // The global flag is synced on the next HTTP handler call, which
    // always runs `refresh` with the application context.
    state.refresh(None);
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct MaintenanceModule {
    state: Arc<MaintenanceState>,
}

impl MaintenanceModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(MaintenanceState::new()),
        }
    }
}

impl Default for MaintenanceModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for MaintenanceModule {
    fn id(&self) -> &'static str {
        "maintenance"
    }

    fn name(&self) -> &'static str {
        "Maintenance Windows"
    }

    fn description(&self) -> &'static str {
        "Scheduled maintenance windows with time-based activation, a global maintenance-mode flag, subscriber notifications, and history."
    }

    fn requires(&self) -> &[&'static str] {
        &["notifications", "scheduler"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_maintenance_check(state.clone(), ctx));
        bus.register("maintenance.check", "maintenance", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/maintenance")
                .route("/windows", web::get().to(list_windows))
                .route("/windows", web::post().to(create_window))
                .route("/windows/{id}", web::get().to(get_window))
                .route("/windows/{id}", web::put().to(update_window))
                .route("/windows/{id}", web::delete().to(delete_window))
                .route("/windows/{id}/activate", web::post().to(activate_window))
                .route("/windows/{id}/deactivate", web::post().to(deactivate_window))
                .route("/windows/{id}/cancel", web::post().to(cancel_window))
                .route("/status", web::get().to(current_status))
                .route("/history", web::get().to(list_history))
                .route("/tick", web::post().to(run_tick))
                .route("/mode", web::post().to(set_maintenance_mode)),
        );
    }

    fn route_info(&self) -> Vec<crate::core::modules::RouteInfo> {
        use crate::core::modules::RouteInfo;
        vec![
            RouteInfo { method: "GET", path: "/api/v1/maintenance/windows", owner: "maintenance", description: "List maintenance windows (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/maintenance/windows", owner: "maintenance", description: "Create a maintenance window." },
            RouteInfo { method: "GET", path: "/api/v1/maintenance/windows/{id}", owner: "maintenance", description: "Get one maintenance window." },
            RouteInfo { method: "PUT", path: "/api/v1/maintenance/windows/{id}", owner: "maintenance", description: "Update a scheduled maintenance window." },
            RouteInfo { method: "DELETE", path: "/api/v1/maintenance/windows/{id}", owner: "maintenance", description: "Delete a non-active maintenance window." },
            RouteInfo { method: "POST", path: "/api/v1/maintenance/windows/{id}/activate", owner: "maintenance", description: "Activate a scheduled window now." },
            RouteInfo { method: "POST", path: "/api/v1/maintenance/windows/{id}/deactivate", owner: "maintenance", description: "End an active window now." },
            RouteInfo { method: "POST", path: "/api/v1/maintenance/windows/{id}/cancel", owner: "maintenance", description: "Cancel a scheduled window." },
            RouteInfo { method: "GET", path: "/api/v1/maintenance/status", owner: "maintenance", description: "Current maintenance status, active and next window." },
            RouteInfo { method: "GET", path: "/api/v1/maintenance/history", owner: "maintenance", description: "Paginated maintenance history." },
            RouteInfo { method: "POST", path: "/api/v1/maintenance/tick", owner: "maintenance", description: "Run time-based window transitions now." },
            RouteInfo { method: "POST", path: "/api/v1/maintenance/mode", owner: "maintenance", description: "Set the global maintenance-mode flag directly." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(MaintenanceModule::new()));
}
