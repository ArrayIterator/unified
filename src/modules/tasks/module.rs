// tasks - Module registration: struct, defaults, routes, and hooks.

use super::*;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use crate::modules::ai;
use actix_web::web;
use dashmap::DashMap;
use std::sync::Arc;

// Module.

pub struct TasksModule {
    store: TasksStore,
}

impl TasksModule {
    pub fn new() -> Self {
        Self {
            store: Arc::new(DashMap::new()),
        }
    }
}

impl Default for TasksModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for TasksModule {
    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("tasks", AI_CAPABILITIES);
        Ok(())
    }

    fn id(&self) -> &'static str {
        "tasks"
    }

    fn name(&self) -> &'static str {
        "Tasks"
    }

    fn description(&self) -> &'static str {
        "Task management: status workflow, assignees, due dates, tags, comments, checklist items, and time tracking. Persists in memory; the database tables are the persistence target (see the module header)."
    }

    fn requires(&self) -> &[&'static str] {
        &["users"]
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/tasks", owner: "tasks", description: "List tasks with pagination and filters.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tasks", owner: "tasks", description: "Create a task.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tasks/{id}", owner: "tasks", description: "Get one task.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/tasks/{id}", owner: "tasks", description: "Update a task.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/tasks/{id}", owner: "tasks", description: "Delete a task.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tasks/{id}/status", owner: "tasks", description: "Change a task status (validated transition).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tasks/{id}/assign", owner: "tasks", description: "Assign or unassign a task.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tasks/{id}/time", owner: "tasks", description: "Log time spent on a task.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tasks/{id}/comments", owner: "tasks", description: "List comments on a task.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tasks/{id}/comments", owner: "tasks", description: "Add a comment to a task.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tasks/{id}/checklist", owner: "tasks", description: "Add a checklist item to a task.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tasks/{id}/checklist/{item_id}/toggle", owner: "tasks", description: "Toggle a checklist item.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/tasks/{id}/checklist/{item_id}", owner: "tasks", description: "Delete a checklist item.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let store = self.store.clone();
        cfg.service(
            web::scope("/api/v1/tasks")
                .app_data(web::Data::new(store))
                .route("", web::get().to(list_tasks))
                .route("", web::post().to(create_task))
                .route("/{id}", web::get().to(get_task))
                .route("/{id}", web::put().to(update_task))
                .route("/{id}", web::delete().to(delete_task))
                .route("/{id}/status", web::post().to(change_task_status))
                .route("/{id}/assign", web::post().to(assign_task))
                .route("/{id}/time", web::post().to(log_time))
                .route("/{id}/comments", web::get().to(list_comments))
                .route("/{id}/comments", web::post().to(add_comment))
                .route("/{id}/checklist", web::post().to(add_checklist_item))
                .route(
                    "/{id}/checklist/{item_id}/toggle",
                    web::post().to(toggle_checklist_item),
                )
                .route(
                    "/{id}/checklist/{item_id}",
                    web::delete().to(delete_checklist_item),
                ),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(TasksModule::new()));
}
