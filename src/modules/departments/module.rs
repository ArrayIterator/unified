//! Module registration: routes, hooks and metadata.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::{web};
use std::sync::{Arc};
use super::state::DepartmentsState;
use super::handlers_departments::{add_agent, create_department, delete_department, get_department, get_department_sla, list_agents, list_departments, remove_agent, update_department};
use super::handlers_routing::{create_rule, delete_rule, get_rule, list_rules, list_rules_for_department, route_preview, update_rule};
use super::hooks::{on_department_agents_requested, on_department_membership_check, on_department_sla_requested, on_ticket_created, on_ticket_route_requested};
use super::ai_capabilities::AI_CAPABILITIES;

// Module

pub struct DepartmentsModule {
    pub(crate) state: Arc<DepartmentsState>,
}

impl DepartmentsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(DepartmentsState::new()),
        }
    }
}

impl Default for DepartmentsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for DepartmentsModule {
    fn id(&self) -> &'static str {
        "departments"
    }

    fn name(&self) -> &'static str {
        "Departments"
    }

    fn description(&self) -> &'static str {
        "Core support departments: agents, ticket routing rules, per-department SLA policies, working hours, and auto-replies."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["roles", "users", "notifications"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("departments", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_ticket_route_requested(state.clone(), ctx)
        });
        bus.register("ticket.route_requested", "departments", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_department_sla_requested(state.clone(), ctx)
        });
        bus.register("department.sla_requested", "departments", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_department_membership_check(state.clone(), ctx)
        });
        bus.register("department.membership_check", "departments", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_department_agents_requested(state.clone(), ctx)
        });
        bus.register("department.agents_requested", "departments", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_ticket_created(state.clone(), ctx));
        bus.register("ticket.created", "departments", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/departments")
                // Literal segments first: they must win over `/{id}`.
                .route("/rules", web::get().to(list_rules))
                .route("/route-preview", web::post().to(route_preview))
                .route("", web::get().to(list_departments))
                .route("", web::post().to(create_department))
                .route("/{id}", web::get().to(get_department))
                .route("/{id}", web::put().to(update_department))
                .route("/{id}", web::delete().to(delete_department))
                .route("/{id}/sla", web::get().to(get_department_sla))
                .route("/{id}/agents", web::get().to(list_agents))
                .route("/{id}/agents", web::post().to(add_agent))
                .route("/{id}/agents/{user_id}", web::delete().to(remove_agent))
                .route("/{id}/rules", web::get().to(list_rules_for_department))
                .route("/{id}/rules", web::post().to(create_rule))
                .route("/rules/{rule_id}", web::get().to(get_rule))
                .route("/rules/{rule_id}", web::put().to(update_rule))
                .route("/rules/{rule_id}", web::delete().to(delete_rule)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/departments", owner: "departments", description: "List departments (paginated; filter by status, default flag).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/departments", owner: "departments", description: "Create a department.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/departments/{id}", owner: "departments", description: "Get one department.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/departments/{id}", owner: "departments", description: "Update a department.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/departments/{id}", owner: "departments", description: "Delete a department (refused while it still owns tickets).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/departments/{id}/sla", owner: "departments", description: "Get a department's SLA policy (first-response and resolution minutes).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/departments/{id}/agents", owner: "departments", description: "List a department's agent user ids.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/departments/{id}/agents", owner: "departments", description: "Add an agent to a department (optional role validation).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/departments/{id}/agents/{user_id}", owner: "departments", description: "Remove an agent from a department.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/departments/{id}/rules", owner: "departments", description: "List a department's routing rules (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/departments/{id}/rules", owner: "departments", description: "Create a routing rule for a department.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/departments/rules", owner: "departments", description: "List all routing rules (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/departments/rules/{rule_id}", owner: "departments", description: "Get one routing rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/departments/rules/{rule_id}", owner: "departments", description: "Update a routing rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/departments/rules/{rule_id}", owner: "departments", description: "Delete a routing rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/departments/route-preview", owner: "departments", description: "Dry-run ticket routing for a draft ticket.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(DepartmentsModule::new()));
}
