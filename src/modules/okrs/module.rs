// OKR / Goals module — objectives and key results at company,
// team, and personal levels, with cycles / quarters, check-ins,
// cycle-end scoring, computed status, alignment (objective to
// parent objective, key result to project / task), and progress
// roll-up computed from linked work.
//
// Routes live under `/api/v1/okrs`:
//
//   Cycles:
//     GET/POST /cycles — list cycles / create a cycle.
//     GET  /cycles/{id}.
//     POST /cycles/{id}/activate — make it the active cycle
//          (one active cycle at a time per company).
//     POST /cycles/{id}/close — close the cycle and score every
//          objective (0.0–1.0).
//   Objectives:
//     GET/POST /objectives — list / create an objective.
//     GET/PATCH/DELETE /objectives/{id}.
//     GET  /objectives/{id}/alignment — parent chain, children,
//          and linked projects / tasks.
//   Key results:
//     GET/POST /objectives/{id}/key-results — list / add a key result.
//     GET/PATCH/DELETE /key-results/{id}.
//     POST /key-results/{id}/check-ins — periodic progress note +
//          confidence (no value change; see the roll-up note).
//     GET  /key-results/{id}/check-ins — list check-ins.
//     POST /key-results/{id}/override — manual current-value
//          override; requires a reason and writes an audit record.
//     GET/POST /key-results/{id}/links — list / link a project or
//          task.
//     DELETE /key-results/{id}/links/{link_id} — unlink.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory cycles, objectives, key results, check-ins, and links;
// one active cycle per company enforced at write time; key-result
// progress computed as (current - start) / (target - start);
// roll-up from linked tasks refreshed by the `task.completed` and
// `project.task.completed` listeners (a completed linked task
// moves the key result's current value along the start-to-target
// range); manual overrides that require a reason string, are
// stored as an override check-in, and emit the audit-shaped
// `okrs.kr.override` event; status computed from progress versus
// elapsed cycle time; cycle-end scoring (mean key-result progress
// per objective, clamped to 0.0–1.0). What is NOT built here: no
// automatic discovery of a project's total task count — a link of
// type `project` is stored for alignment and shown in the
// alignment view, but the numeric roll-up counts linked *tasks*
// only (a project link with no linked tasks contributes no
// progress, and the alignment view says so); the audit module only
// records events listed in its `AUDITED_EVENTS`, so the integrator
// must add `okrs.kr.override` there for the override to land in
// the audit log (the override check-in row is stored regardless);
// and no notification is sent on cycle close (only
// `okrs.cycle.closed` is emitted).
//
// Public-access note: this module has NO public routes. Every route
// requires the `X-User-Id` / `X-Role` headers and a capability
// check (see the actor-identity note below). Do NOT treat any route
// here as public.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai and events modules. Capability
// checks use the `role.check` hook and fail closed (no answer means
// not allowed).
//
// Events emitted on the hook bus:
//   - `okrs.cycle.created`, `okrs.cycle.closed`
//   - `okrs.objective.created`, `okrs.objective.updated`,
//     `okrs.objective.deleted`
//   - `okrs.key_result.created`, `okrs.key_result.updated`,
//     `okrs.key_result.deleted`
//   - `okrs.kr.checkin`
//   - `okrs.kr.override` (audit-shaped payload: `actor_id`,
//     `resource`, `key_result_id`, `old_value`, `new_value`,
//     `reason` — the audit module records it once the integrator
//     adds the name to `AUDITED_EVENTS` in `audit.rs`).
//   - `okrs.link.created`, `okrs.link.deleted`
// Events this module listens to:
//   - `task.completed` — refreshes roll-ups of key results linked
//     to the completed task (payload: `task_id` / `id`, optional
//     `project_id`, `company_id`).
//   - `project.task.completed` — same, for project tasks (payload:
//     `task_id` / `id`, optional `project_id`).


use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_cycles::*;
use super::handlers_objectives::*;
use super::handlers_key_results::*;
use super::hooks::*;
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct OkrsModule {
    state: OkrState,
}

impl OkrsModule {
    pub fn new() -> Self {
        Self {
            state: OkrState::default(),
        }
    }
}

impl Default for OkrsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for OkrsModule {
    fn id(&self) -> &'static str {
        "okrs"
    }

    fn name(&self) -> &'static str {
        "OKR / Goals"
    }

    fn description(&self) -> &'static str {
        "Objectives and key results at company, team, and personal levels, with cycles / quarters, check-ins, cycle-end scoring, computed status, alignment to parent objectives and linked projects / tasks, and progress roll-up computed from linked task completion."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "projects", "tasks"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "projects", "tasks"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("okrs", AI_CAPABILITIES);
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
            "task.completed",
            "okrs",
            Arc::new(move |ctx: &mut HookContext| on_task_completed(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "project.task.completed",
            "okrs",
            Arc::new(move |ctx: &mut HookContext| on_task_completed(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/okrs")
                .app_data(state)
                .route("/cycles", web::get().to(list_cycles))
                .route("/cycles", web::post().to(create_cycle))
                .route("/cycles/{id}", web::get().to(get_cycle))
                .route("/cycles/{id}/activate", web::post().to(activate_cycle))
                .route("/cycles/{id}/close", web::post().to(close_cycle))
                .route("/objectives", web::get().to(list_objectives))
                .route("/objectives", web::post().to(create_objective))
                .route("/objectives/{id}", web::get().to(get_objective))
                .route("/objectives/{id}", web::patch().to(update_objective))
                .route("/objectives/{id}", web::delete().to(delete_objective))
                .route(
                    "/objectives/{id}/alignment",
                    web::get().to(get_alignment),
                )
                .route(
                    "/objectives/{id}/key-results",
                    web::get().to(list_key_results),
                )
                .route(
                    "/objectives/{id}/key-results",
                    web::post().to(create_key_result),
                )
                .route("/key-results/{id}", web::get().to(get_key_result))
                .route("/key-results/{id}", web::patch().to(update_key_result))
                .route("/key-results/{id}", web::delete().to(delete_key_result))
                .route(
                    "/key-results/{id}/check-ins",
                    web::get().to(list_checkins),
                )
                .route(
                    "/key-results/{id}/check-ins",
                    web::post().to(create_checkin),
                )
                .route(
                    "/key-results/{id}/override",
                    web::post().to(override_key_result),
                )
                .route("/key-results/{id}/links", web::get().to(list_links))
                .route("/key-results/{id}/links", web::post().to(create_link))
                .route(
                    "/key-results/{id}/links/{link_id}",
                    web::delete().to(delete_link),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/okrs/cycles", owner: "okrs", description: "List cycles (paginated, filter by company_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/okrs/cycles", owner: "okrs", description: "Create a cycle / quarter (draft; ends_at must be after starts_at).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/okrs/cycles/{id}", owner: "okrs", description: "Get one cycle.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/okrs/cycles/{id}/activate", owner: "okrs", description: "Activate a cycle (fails when another cycle of the company is already active).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/okrs/cycles/{id}/close", owner: "okrs", description: "Close a cycle and score every objective (mean key-result progress, 0.0-1.0; emits okrs.cycle.closed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/okrs/objectives", owner: "okrs", description: "List objectives with computed progress and status (paginated, filter by cycle_id, company_id, level, owner_id, parent_objective_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/okrs/objectives", owner: "okrs", description: "Create an objective (company / team / personal level, optional parent objective).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/okrs/objectives/{id}", owner: "okrs", description: "Get one objective with computed progress and status.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/okrs/objectives/{id}", owner: "okrs", description: "Update an objective (not after its cycle closed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/okrs/objectives/{id}", owner: "okrs", description: "Delete an objective with its key results, check-ins, and links.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/okrs/objectives/{id}/alignment", owner: "okrs", description: "Alignment view: parent chain, child objectives, and linked projects / tasks.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/okrs/objectives/{id}/key-results", owner: "okrs", description: "List key results of an objective (with computed progress).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/okrs/objectives/{id}/key-results", owner: "okrs", description: "Add a key result (target value + unit, start value).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/okrs/key-results/{id}", owner: "okrs", description: "Get one key result with computed progress.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/okrs/key-results/{id}", owner: "okrs", description: "Update a key result title / unit / target / start (current value is never hand-typed here).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/okrs/key-results/{id}", owner: "okrs", description: "Delete a key result with its check-ins and links.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/okrs/key-results/{id}/check-ins", owner: "okrs", description: "List check-ins of a key result.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/okrs/key-results/{id}/check-ins", owner: "okrs", description: "Add a check-in (progress note + confidence; records a value snapshot, never sets the value).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/okrs/key-results/{id}/override", owner: "okrs", description: "Manual current-value override (reason required; stored as an override check-in and emitted as the audit-shaped okrs.kr.override event).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/okrs/key-results/{id}/links", owner: "okrs", description: "List project / task links of a key result.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/okrs/key-results/{id}/links", owner: "okrs", description: "Link a project or task to a key result (task links drive the roll-up).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/okrs/key-results/{id}/links/{link_id}", owner: "okrs", description: "Remove a project / task link (roll-up is recomputed).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(OkrsModule::new()));
}
