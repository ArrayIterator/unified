// todos - module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use dashmap::{DashMap, DashSet};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::validation::*;
use super::access::*;
use super::service::*;
use super::handlers_lists::*;
use super::handlers_todos::*;
use super::handlers_subtasks::*;
use super::handlers_ai::*;

// Module.

pub struct TodosModule {
    pub(crate) state: TodosState,
}

impl TodosModule {
    pub fn new() -> Self {
        Self {
            state: TodosState::default(),
        }
    }

    // Product declaration: Todos are never a product. The `Module`
    // trait defaults (`available_as_product() -> false`,
    // `product_category() -> None`) already express this, so no
    // override is needed here.
}

impl Default for TodosModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for TodosModule {
    fn id(&self) -> &'static str {
        "todos"
    }

    fn name(&self) -> &'static str {
        "Todos"
    }

    fn description(&self) -> &'static str {
        "Personal per-user todos: lists (including a default Inbox), quick capture, due dates with timezone-aware reminders, priorities, tags, one-level subtasks, storage-key attachments, list sharing (read/edit, ownership never transfers), recurring todos, completion history with computed streaks, and AI draft summaries and day plans. Personal utility, not a product. Strictly separate from the shared/assigned tasks module."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "scheduler"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "scheduler"]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("todos", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_TODOS_CONTEXT_REQUESTED,
            "todos",
            Arc::new(move |ctx: &mut HookContext| {
                on_todos_context_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_REMINDERS_SEND_REQUESTED,
            "todos",
            Arc::new(move |ctx: &mut HookContext| {
                on_reminders_send_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/todos")
                .app_data(state)
                .route("/lists", web::get().to(list_lists))
                .route("/lists", web::post().to(create_list))
                .route("/shared-with-me", web::get().to(list_shared_with_me))
                .route("/lists/{id}", web::get().to(get_list))
                .route("/lists/{id}", web::patch().to(update_list))
                .route("/lists/{id}", web::delete().to(delete_list))
                .route("/lists/{id}/archive", web::post().to(archive_list))
                .route("/lists/{id}/unarchive", web::post().to(unarchive_list))
                .route("/lists/{id}/shares", web::get().to(list_shares))
                .route("/lists/{id}/share", web::post().to(share_list))
                .route(
                    "/lists/{id}/share/{user_id}",
                    web::delete().to(revoke_share),
                )
                .route("", web::get().to(list_todos))
                .route("", web::post().to(create_todo))
                .route("/completed", web::get().to(list_completed_todos))
                .route("/overdue", web::get().to(list_overdue_todos))
                .route("/streaks", web::get().to(get_streaks))
                .route("/{id}", web::get().to(get_todo))
                .route("/{id}", web::patch().to(update_todo))
                .route("/{id}", web::delete().to(delete_todo))
                .route("/{id}/complete", web::post().to(complete_todo))
                .route("/{id}/reopen", web::post().to(reopen_todo))
                .route("/{id}/history", web::get().to(list_todo_history))
                .route("/{id}/subtasks", web::post().to(add_subtask))
                .route(
                    "/{id}/subtasks/{subtask_id}",
                    web::patch().to(update_subtask),
                )
                .route(
                    "/{id}/subtasks/{subtask_id}/toggle",
                    web::post().to(toggle_subtask),
                )
                .route(
                    "/{id}/subtasks/{subtask_id}",
                    web::delete().to(delete_subtask),
                )
                .route("/{id}/attachments", web::post().to(add_attachment))
                .route(
                    "/{id}/attachments/{attachment_id}",
                    web::delete().to(delete_attachment),
                )
                .route("/{id}/reminders", web::post().to(create_todo_reminder))
                .route("/{id}/reminders", web::get().to(list_todo_reminders))
                .route(
                    "/{id}/reminders/{reminder_id}",
                    web::delete().to(cancel_todo_reminder),
                )
                .route("/ai/summarize", web::post().to(ai_summarize))
                .route("/ai/plan-day", web::post().to(ai_plan_day)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/todos/lists", owner: "todos", description: "List the caller's own lists plus lists shared with them (paginated, filter by archived, owner_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/todos/lists", owner: "todos", description: "Create a list (name, allowlisted color/icon, sort order).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/todos/shared-with-me", owner: "todos", description: "Lists shared with the caller, with the granted permission.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/todos/lists/{id}", owner: "todos", description: "Get one list the caller owns or has shared access to.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/todos/lists/{id}", owner: "todos", description: "Update a list (owner only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/todos/lists/{id}", owner: "todos", description: "Delete an empty, non-default list (owner only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/todos/lists/{id}/archive", owner: "todos", description: "Archive a list (owner only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/todos/lists/{id}/unarchive", owner: "todos", description: "Unarchive a list (owner only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/todos/lists/{id}/shares", owner: "todos", description: "Who a list is shared with (owner only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/todos/lists/{id}/share", owner: "todos", description: "Share a list with a user (read or edit permission; owner only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/todos/lists/{id}/share/{user_id}", owner: "todos", description: "Revoke sharing of a list with a user (owner only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/todos", owner: "todos", description: "List own + shared todos (paginated; filters: list_id, status, priority, tag, overdue, due_before, due_after, search).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/todos", owner: "todos", description: "Create a todo; quick capture with title only (defaults to the Inbox list, normal priority, open).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/todos/completed", owner: "todos", description: "Completed todos, most recently completed first (paginated, optional list_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/todos/overdue", owner: "todos", description: "Open todos past their due time, most overdue first (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/todos/streaks", owner: "todos", description: "Computed completion streaks for the caller (current/longest days, total completions).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/todos/{id}", owner: "todos", description: "Get one todo (owner or shared-list viewer).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/todos/{id}", owner: "todos", description: "Update a todo (owner or edit-shared user; moving across owners is refused).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/todos/{id}", owner: "todos", description: "Delete a todo (owner or edit-shared user).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/todos/{id}/complete", owner: "todos", description: "Complete a todo; creates the next occurrence when it recurs.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/todos/{id}/reopen", owner: "todos", description: "Reopen a completed todo.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/todos/{id}/history", owner: "todos", description: "Completion log for one todo (who completed/reopened, when; paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/todos/{id}/subtasks", owner: "todos", description: "Add a one-level subtask to a todo.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/todos/{id}/subtasks/{subtask_id}", owner: "todos", description: "Rename a subtask.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/todos/{id}/subtasks/{subtask_id}/toggle", owner: "todos", description: "Check or uncheck a subtask.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/todos/{id}/subtasks/{subtask_id}", owner: "todos", description: "Delete a subtask.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/todos/{id}/attachments", owner: "todos", description: "Attach a stored object by storage target + key (metadata only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/todos/{id}/attachments/{attachment_id}", owner: "todos", description: "Remove an attachment reference (stored bytes are untouched).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/todos/{id}/reminders", owner: "todos", description: "Add a due-date reminder offset in minutes (requires due_at).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/todos/{id}/reminders", owner: "todos", description: "List reminders for a todo (paginated, filter by status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/todos/{id}/reminders/{reminder_id}", owner: "todos", description: "Cancel a pending reminder.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/todos/ai/summarize", owner: "todos", description: "AI draft summary of the caller's own todos (draft only; 503 when no AI handler answers).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/todos/ai/plan-day", owner: "todos", description: "AI draft day plan from the caller's own todos due that day (draft only; 503 when no AI handler answers).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(TodosModule::new()));
}
