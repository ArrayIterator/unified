// Maintenance module — types, status constants, and shared state.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;

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
    /// (manual), `deactivated` (manual), or `deleted`.
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

/// Decide what `refresh` should do with the global maintenance
/// flag. Returns `Some(desired)` when the flag must be synced to
/// the window state, `None` when a manual override owns the flag
/// and it must be left alone. An active window always wins over a
/// manual override (the flag stays on); with no window active and
/// no override, the flag is synced off.
pub(crate) fn flag_sync_target(any_active: bool, manual_override: bool) -> Option<bool> {
    if manual_override && !any_active {
        None
    } else {
        Some(any_active)
    }
}

pub struct MaintenanceState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) windows: DashMap<Uuid, MaintenanceWindow>,
    pub(crate) history: DashMap<Uuid, MaintenanceHistoryEntry>,
    /// True while a manual maintenance-mode override (set through
    /// POST /mode) owns the global flag. While set, `refresh` never
    /// syncs the flag back to the window state, so a manual enable
    /// survives the next handler call; it is cleared only by an
    /// explicit manual disable.
    pub(crate) manual_override: AtomicBool,
}

impl MaintenanceState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            windows: DashMap::new(),
            history: DashMap::new(),
            manual_override: AtomicBool::new(false),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    pub(crate) fn record_history(&self, window_id: Uuid, event: &str, message: String) {
        let entry = MaintenanceHistoryEntry {
            id: Uuid::now_v7(),
            window_id,
            event: event.to_string(),
            message,
            created_at: Utc::now(),
        };
        self.history.insert(entry.id, entry);
    }

    /// Remove a window, recording a `deleted` history entry and
    /// emitting `maintenance.window_deleted`, like the other
    /// lifecycle transitions. Returns the removed window, or `None`
    /// when it did not exist.
    pub(crate) fn remove_window(&self, id: Uuid) -> Option<MaintenanceWindow> {
        let (_, removed) = self.windows.remove(&id)?;
        self.record_history(
            id,
            "deleted",
            format!("Window \"{}\" deleted", removed.title),
        );
        self.emit_window_event("maintenance.window_deleted", &removed);
        Some(removed)
    }

    /// Notify subscribers about a lifecycle event through the standard
    /// `notification.requested` channel (kind `maintenance`).
    pub(crate) fn notify_subscribers(&self, window: &MaintenanceWindow, event: &str) {
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

    pub(crate) fn emit_window_event(&self, event: &str, window: &MaintenanceWindow) {
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
    pub(crate) fn refresh(&self, ctx: Option<&AppContext>) -> usize {
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
                    // Missed window: its start and end both passed
                    // while it was still scheduled. It transitions
                    // through `started` first and then `ended`, so
                    // subscribers see the full lifecycle instead of
                    // an end without a start. Both snapshots are
                    // taken before the guard is dropped; history and
                    // events are emitted after it.
                    window.status = STATUS_ACTIVE.to_string();
                    window.activated_at = Some(now);
                    window.updated_at = now;
                    let started_snapshot = window.clone();
                    window.status = STATUS_COMPLETED.to_string();
                    window.completed_at = Some(now);
                    window.updated_at = now;
                    let snapshot = window.clone();
                    drop(window);
                    changed += 1;
                    self.record_history(
                        id,
                        "started",
                        format!(
                            "Window \"{}\" started on schedule (missed: end time had already passed)",
                            started_snapshot.title
                        ),
                    );
                    self.emit_window_event("maintenance.started", &started_snapshot);
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
            // A manual override owns the flag while it is set: skip
            // syncing so a manual enable is never wiped by the next
            // refresh. Otherwise the flag follows the window state.
            let manual = self.manual_override.load(Ordering::SeqCst);
            if let Some(target) = flag_sync_target(any_active, manual) {
                let was = ctx.is_maintenance();
                if was != target {
                    ctx.set_maintenance(target);
                    self.emit(
                        unified_core::hooks::events::APP_MAINTENANCE,
                        json!({ "maintenance": target }),
                    );
                }
            }
        }
        changed
    }

    pub(crate) fn active_window(&self) -> Option<MaintenanceWindow> {
        let mut active: Vec<MaintenanceWindow> = self
            .windows
            .iter()
            .filter(|entry| entry.value().status == STATUS_ACTIVE)
            .map(|entry| entry.value().clone())
            .collect();
        active.sort_by(|a, b| a.starts_at.cmp(&b.starts_at));
        active.into_iter().next()
    }

    pub(crate) fn next_scheduled_window(&self) -> Option<MaintenanceWindow> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn window(title: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> MaintenanceWindow {
        let now = Utc::now();
        MaintenanceWindow {
            id: Uuid::new_v4(),
            title: title.to_string(),
            description: String::new(),
            starts_at: start,
            ends_at: end,
            affected_services: Vec::new(),
            status: STATUS_SCHEDULED.to_string(),
            created_at: now,
            updated_at: now,
            activated_at: None,
            completed_at: None,
        }
    }

    fn test_ctx() -> AppContext {
        use unified_core::config::AppConfig;
        use unified_core::modules::ModuleRegistry;
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn capture_events(state: &MaintenanceState, seen: Arc<Mutex<Vec<&'static str>>>) {
        for (event, marker) in [
            ("maintenance.started", "started"),
            ("maintenance.ended", "ended"),
            ("maintenance.window_deleted", "deleted"),
        ] {
            let seen = seen.clone();
            state.bus().register(
                event,
                "test",
                Arc::new(move |_ctx: &mut HookContext| {
                    seen.lock().expect("event capture lock poisoned").push(marker);
                }),
            );
        }
    }

    #[test]
    fn flag_sync_target_keeps_a_manual_override() {
        assert_eq!(flag_sync_target(false, true), None);
        assert_eq!(flag_sync_target(false, false), Some(false));
        assert_eq!(flag_sync_target(true, true), Some(true));
        assert_eq!(flag_sync_target(true, false), Some(true));
    }

    #[test]
    fn refresh_keeps_a_manual_enable_when_no_window_is_active() {
        let state = MaintenanceState::new();
        let ctx = test_ctx();
        // Simulate a POST /mode enable: flag on, override set.
        ctx.set_maintenance(true);
        state.manual_override.store(true, Ordering::SeqCst);
        // A window that already activated once: the old sync logic
        // wiped the manual flag as soon as any window had ever
        // activated.
        let now = Utc::now();
        let mut done = window(
            "done",
            now - chrono::Duration::hours(3),
            now - chrono::Duration::hours(2),
        );
        done.status = STATUS_COMPLETED.to_string();
        done.activated_at = Some(now - chrono::Duration::hours(3));
        done.completed_at = Some(now - chrono::Duration::hours(2));
        state.windows.insert(done.id, done);
        state.refresh(Some(&ctx));
        assert!(
            ctx.is_maintenance(),
            "a manual override must survive refresh"
        );
    }

    #[test]
    fn refresh_clears_the_flag_without_a_manual_override() {
        let state = MaintenanceState::new();
        let ctx = test_ctx();
        ctx.set_maintenance(true);
        state.refresh(Some(&ctx));
        assert!(!ctx.is_maintenance());
    }

    #[test]
    fn refresh_emits_started_then_ended_for_a_missed_window() {
        let state = MaintenanceState::new();
        let seen: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        capture_events(&state, seen.clone());
        let now = Utc::now();
        let missed = window(
            "missed",
            now - chrono::Duration::hours(2),
            now - chrono::Duration::hours(1),
        );
        let id = missed.id;
        state.windows.insert(id, missed);
        let changed = state.refresh(None);
        assert_eq!(changed, 1);
        let finished = state.windows.get(&id).expect("window must still exist");
        assert_eq!(finished.status, STATUS_COMPLETED);
        assert!(finished.activated_at.is_some());
        assert!(finished.completed_at.is_some());
        let events = seen.lock().expect("event capture lock poisoned").clone();
        assert_eq!(events, vec!["started", "ended"]);
        let mut history: Vec<String> = state
            .history
            .iter()
            .filter(|entry| entry.value().window_id == id)
            .map(|entry| entry.value().event.clone())
            .collect();
        history.sort();
        assert_eq!(history, vec!["ended".to_string(), "started".to_string()]);
    }

    #[test]
    fn remove_window_records_history_and_emits_the_deleted_event() {
        let state = MaintenanceState::new();
        let seen: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        capture_events(&state, seen.clone());
        let now = Utc::now();
        let target = window(
            "old",
            now + chrono::Duration::hours(1),
            now + chrono::Duration::hours(2),
        );
        let id = target.id;
        state.windows.insert(id, target);
        let removed = state.remove_window(&id).expect("window must be removed");
        assert_eq!(removed.id, id);
        assert!(state.windows.get(&id).is_none());
        assert!(state.remove_window(&id).is_none());
        let events = seen.lock().expect("event capture lock poisoned").clone();
        assert_eq!(events, vec!["deleted"]);
        assert!(state.history.iter().any(|entry| entry.value().window_id == id
            && entry.value().event == "deleted"));
    }
}
