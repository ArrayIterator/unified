// Tickets module — module definition and registration.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct TicketsModule {
    pub(crate) state: Arc<TicketsState>,
}

impl TicketsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(TicketsState::new()),
        }
    }
}

impl Default for TicketsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for TicketsModule {
    fn id(&self) -> &'static str {
        "tickets"
    }

    fn name(&self) -> &'static str {
        "Support Tickets"
    }

    fn description(&self) -> &'static str {
        "Support tickets with comments, assignment, validated status transitions, knowledgebase suggestions, departments, SLA timers, escalation rules, canned responses, internal notes, attachments, merging, and links to orders, licenses, and servers."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["knowledgebase", "notifications", "departments"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("tickets", AI_CAPABILITIES);
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
            Arc::new(move |ctx: &mut HookContext| on_ticket_create_requested(state.clone(), ctx));
        bus.register("ticket.create_requested", "tickets", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_ticket_sla_check_requested(state.clone(), ctx)
        });
        bus.register("ticket.sla.check_requested", "tickets", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_department_delete_check(state.clone(), ctx)
        });
        bus.register("department.delete_check", "tickets", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/tickets")
                // Literal segments first: they must win over `/{id}`.
                .route("/canned-responses", web::get().to(list_canned_responses))
                .route("/canned-responses", web::post().to(create_canned_response))
                .route(
                    "/canned-responses/{id}",
                    web::get().to(get_canned_response),
                )
                .route(
                    "/canned-responses/{id}",
                    web::put().to(update_canned_response),
                )
                .route(
                    "/canned-responses/{id}",
                    web::delete().to(delete_canned_response),
                )
                .route("/escalations/rules", web::get().to(list_escalation_rules))
                .route("/escalations/rules", web::post().to(create_escalation_rule))
                .route(
                    "/escalations/rules/{id}",
                    web::get().to(get_escalation_rule),
                )
                .route(
                    "/escalations/rules/{id}",
                    web::put().to(update_escalation_rule),
                )
                .route(
                    "/escalations/rules/{id}",
                    web::delete().to(delete_escalation_rule),
                )
                .route("/sla/check", web::post().to(sla_check))
                .route("", web::get().to(list_tickets))
                .route("", web::post().to(create_ticket))
                .route("/{id}", web::get().to(get_ticket))
                .route("/{id}", web::put().to(update_ticket))
                .route("/{id}", web::delete().to(delete_ticket))
                .route("/{id}/comments", web::post().to(add_comment))
                .route("/{id}/replies", web::post().to(add_reply))
                .route("/{id}/notes", web::get().to(list_notes))
                .route("/{id}/notes", web::post().to(add_note))
                .route("/{id}/attachments", web::get().to(list_attachments))
                .route("/{id}/attachments", web::post().to(add_attachment))
                .route("/{id}/links", web::post().to(add_link))
                .route("/{id}/links", web::delete().to(remove_link))
                .route("/{id}/merge", web::post().to(merge_ticket))
                .route("/{id}/escalate", web::post().to(escalate_ticket))
                .route("/{id}/apply-canned", web::post().to(apply_canned_response))
                .route("/{id}/assign", web::post().to(assign_ticket))
                .route("/{id}/suggestions", web::get().to(ticket_suggestions)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/tickets", owner: "tickets", description: "List tickets (paginated; filter by status, priority, category, product, department, assignee, SLA breach; viewer-scoped).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tickets", owner: "tickets", description: "Create a ticket (routed to a department, SLA timers set).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tickets/{id}", owner: "tickets", description: "Get one ticket (public view for non-staff viewers).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/tickets/{id}", owner: "tickets", description: "Update a ticket (validated status transitions, department move recomputes SLA).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/tickets/{id}", owner: "tickets", description: "Delete a ticket.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/comments", owner: "tickets", description: "Add a plain comment to a ticket.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/replies", owner: "tickets", description: "Add a customer or agent reply (drives first response and waiting state).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tickets/{id}/notes", owner: "tickets", description: "List staff-only internal notes.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/notes", owner: "tickets", description: "Add a staff-only internal note.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tickets/{id}/attachments", owner: "tickets", description: "List ticket attachments (storage references).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/attachments", owner: "tickets", description: "Attach a file already uploaded through the Storage module.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/links", owner: "tickets", description: "Link a ticket to an order, license, or server.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/tickets/{id}/links", owner: "tickets", description: "Remove a ticket link (?target_type=&target_id=).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/merge", owner: "tickets", description: "Merge a ticket into another ticket.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/escalate", owner: "tickets", description: "Escalate a ticket manually (level, optional department move and reassignment).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/apply-canned", owner: "tickets", description: "Insert a canned response as an agent reply.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/assign", owner: "tickets", description: "Assign a ticket to an agent.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tickets/{id}/suggestions", owner: "tickets", description: "Knowledgebase suggestions for a ticket.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tickets/sla/check", owner: "tickets", description: "Run the SLA breach and escalation scan now.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tickets/canned-responses", owner: "tickets", description: "List canned responses (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tickets/canned-responses", owner: "tickets", description: "Create a canned response.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tickets/canned-responses/{id}", owner: "tickets", description: "Get one canned response.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/tickets/canned-responses/{id}", owner: "tickets", description: "Update a canned response.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/tickets/canned-responses/{id}", owner: "tickets", description: "Delete a canned response.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tickets/escalations/rules", owner: "tickets", description: "List escalation rules (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tickets/escalations/rules", owner: "tickets", description: "Create an escalation rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tickets/escalations/rules/{id}", owner: "tickets", description: "Get one escalation rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/tickets/escalations/rules/{id}", owner: "tickets", description: "Update an escalation rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/tickets/escalations/rules/{id}", owner: "tickets", description: "Delete an escalation rule.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(TicketsModule::new()));
}
