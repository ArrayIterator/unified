// Field Service / Equipment Maintenance module — equipment
// registry, recurring maintenance plans, work orders with a
// validated status machine, technician dispatch, parts
// consumption requests, time logging, customer sign-off, and
// completion reports.
//
// Routes live under `/api/v1/field-service`:
//
//   Equipment types (admin-editable definitions):
//     GET/POST /equipment-types — list / create a type.
//     GET/PATCH/DELETE /equipment-types/{id}.
//   Equipment:
//     GET/POST /equipment — list / register equipment.
//     GET/PATCH/DELETE /equipment/{id}.
//     POST /equipment/{id}/report-failure — open a corrective work
//          order for a failure and request an incident.
//   Maintenance templates (admin-editable):
//     GET/POST /maintenance-templates.
//     GET/PATCH/DELETE /maintenance-templates/{id}.
//     POST /maintenance-templates/{id}/apply — create a plan from
//          a template for one equipment item.
//   Maintenance plans:
//     GET/POST /plans — list / create a plan.
//     GET  /plans/due — plans due within ?days (default 7).
//     GET  /plans/overdue — overdue plans.
//     POST /plans/check-due — run the due/overdue check now
//          (alerts via `notification.requested`).
//     GET/PATCH/DELETE /plans/{id}.
//     POST /plans/{id}/recompute — recompute the next-due date.
//   Work orders:
//     GET/POST /work-orders — list / create a work order.
//     GET/PATCH/DELETE /work-orders/{id}.
//     POST /work-orders/{id}/status — validated status transition.
//     POST /work-orders/{id}/assign — assign a technician.
//     POST /work-orders/{id}/cancel — cancel the work order.
//     GET/POST /work-orders/{id}/items — checklist items.
//     PATCH/DELETE /work-orders/{id}/items/{item_id}.
//     POST /work-orders/{id}/items/{item_id}/complete.
//     GET/POST /work-orders/{id}/parts — required / used parts.
//     PATCH/DELETE /work-orders/{id}/parts/{part_id}.
//     POST /work-orders/{id}/parts/{part_id}/consume — request
//          stock consumption (never decrements stock directly).
//     POST /work-orders/{id}/time-log — log time through the
//          timetracker module (event request).
//     POST /work-orders/{id}/raise-incident — request an incident
//          for a failure found on the job.
//     POST /work-orders/{id}/sign-off — customer sign-off.
//     GET  /work-orders/{id}/sign-off — the sign-off record.
//     GET  /work-orders/{id}/report — completion report data.
//   Dispatch:
//     GET  /dispatch/board — per-technician ordered job lists.
//     GET  /dispatch/assignments — list dispatch assignments.
//     POST /dispatch/assignments/{id}/release — release a technician.
//     POST /dispatch/assignments/{id}/complete — mark done.
//   Definitions and settings:
//     GET  /definitions — every admin-editable value registry.
//     POST /definitions/{kind} — add a value to a registry.
//     DELETE /definitions/{kind}/{value} — remove a custom value.
//     GET/PATCH /settings — module settings.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory equipment, plan, work-order, checklist, part,
// dispatch-assignment, and sign-off management; the validated
// work-order status machine; interval-based next-due computation
// (days / weeks / calendar months, plus usage-based plans driven
// by the equipment meter reading); overdue detection; a dispatch
// board that orders jobs by priority and due date; customer
// sign-off records. What is NOT built here: asset records
// themselves — equipment only stores an `asset_id` reference and
// never duplicates an asset row (warranty data is a local copy
// refreshed from `assets.updated` events when that payload
// carries a warranty date); user records — technicians are user
// id references and this module cannot verify that a given user
// holds a technician capability (the roles module answers
// `role.check` for roles, not for users, so the check happens
// when the technician acts, through `can_act_on_order`); stock
// decrements — consuming a part only emits
// `assets.stock.adjust_requested` and records the part as
// `consumption_requested`, because the assets module has no
// listener for that event yet (the integrator must add one, or
// stock stays unchanged); time entries — logging time only emits
// `timetracker.time.log_requested` because the timetracker module
// exposes no log-request listener yet (the answer is inspected
// and reported honestly as `handler_answered`); incident
// creation — raising an incident only emits
// `incidents.create_requested` because the incidents module
// listens to no such event yet (again reported as
// `handler_answered`); survey delivery — a follow-up survey is
// only sent when the caller supplies an existing surveys
// distribution id, through `surveys.send_requested`, which the
// surveys module does listen to; without one the follow-up is
// recorded as pending and the event `field_service.survey.requested`
// is emitted for a future listener. The dispatch board ordering
// is a priority / due-date heuristic, documented on the endpoint
// itself — it is not a route optimiser and never claims travel
// times or distances. Actual email / push delivery belongs to
// the notifications module (this module only emits
// `notification.requested`).
//
// There are no public (unauthenticated) routes in this module:
// every route requires the `X-User-Id` / `X-Role` headers and a
// capability check. Field service data is internal and is never
// exposed through the API Manager public-access rules.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `field_service.equipment.created`,
//     `field_service.equipment.updated`,
//     `field_service.equipment.deleted`,
//     `field_service.equipment.failure_reported`
//   - `field_service.equipment_type.created`,
//     `field_service.equipment_type.updated`,
//     `field_service.equipment_type.deleted`
//   - `field_service.template.created`,
//     `field_service.template.updated`,
//     `field_service.template.deleted`
//   - `field_service.plan.created`, `field_service.plan.updated`,
//     `field_service.plan.deleted`,
//     `field_service.plan.overdue`,
//     `field_service.plan.next_due_reset`,
//     `field_service.maintenance.completed`
//   - `field_service.work_order.created`,
//     `field_service.work_order.updated`,
//     `field_service.work_order.status_changed`,
//     `field_service.work_order.assigned`,
//     `field_service.work_order.cancelled`,
//     `field_service.work_order.completed`,
//     `field_service.work_order.verified`
//   - `field_service.work_order.item.created`,
//     `field_service.work_order.item.updated`,
//     `field_service.work_order.item.deleted`,
//     `field_service.work_order.item.completed`
//   - `field_service.work_order.part.created`,
//     `field_service.work_order.part.updated`,
//     `field_service.work_order.part.deleted`,
//     `field_service.work_order.part.consumed`
//   - `field_service.work_order.time_logged`
//   - `field_service.dispatch.assigned`,
//     `field_service.dispatch.released`,
//     `field_service.dispatch.completed`
//   - `field_service.signoff.created`
//   - `field_service.survey.requested`
//   - `assets.stock.adjust_requested` (part consumption — no
//     assets listener yet, see the honesty note)
//   - `assets.maintenance.completed` (preventive completion — no
//     assets listener yet, see the honesty note)
//   - `timetracker.time.log_requested` (time logging — no
//     timetracker listener yet, see the honesty note)
//   - `incidents.create_requested` (failure / incident raising —
//     no incidents listener yet, see the honesty note)
//   - `surveys.send_requested` (post-job satisfaction survey,
//     only with a caller-supplied distribution id; the surveys
//     module listens to this event)
//   - `notification.requested` (dispatch assignment, overdue and
//     due-soon plans) with the notifications module's payload
//     shape: `{"kind", "user_id", "title", "body",
//     "module": "field_service", "link", "priority", "channels"}`.
// Events this module listens to:
//   - `assets.updated` — refreshes the local warranty copy of
//     equipment rows that reference the asset, when the payload
//     carries `asset_id` and `warranty_end`.
//   - `field_service.plans.check_requested` — the event the
//     scheduler emits when a `field_service.plans.check` job
//     fires; runs the due/overdue check and emits
//     `notification.requested` for every plan inside the alert
//     window. The scheduler job kind itself is not registered
//     yet, so today the check only runs on demand or through
//     this hook.

// field_service module — Module definition and registration.
//
// Split from the former single-file module `src/modules/field_service.rs`.
// The module root (`field_service.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{ASSETS_UPDATED, PLANS_CHECK_REQUESTED};
use super::types::Equipment;
use super::state::FieldServiceState;
use super::helpers::{on_assets_updated, on_plans_check_requested};
use super::handlers_equipment_types::{create_equipment_type, delete_equipment_type, get_equipment_type, list_equipment_types, update_equipment_type};
use super::handlers_equipment::{create_equipment, delete_equipment, get_equipment, list_equipment, report_failure, update_equipment};
use super::handlers_templates::{apply_template, create_template, delete_template, get_template, list_templates, update_template};
use super::handlers_plans::{check_due_plans, create_plan, delete_plan, get_plan, list_due_plans, list_overdue_plans, list_plans, recompute_plan, update_plan};
use super::handlers_work_orders::{assign_work_order, cancel_work_order, change_work_order_status, create_work_order, delete_work_order, get_work_order, list_work_orders, update_work_order};
use super::handlers_items::{add_work_order_item, complete_work_order_item, delete_work_order_item, list_work_order_items, update_work_order_item};
use super::handlers_parts::{add_work_order_part, consume_work_order_part, delete_work_order_part, list_work_order_parts, update_work_order_part};
use super::handlers_time_signoff::{get_work_order_signoff, log_work_order_time, raise_incident, sign_off_work_order, work_order_report};
use super::handlers_dispatch::{complete_dispatch_assignment, dispatch_board, list_dispatch_assignments, release_dispatch_assignment};
use super::handlers_definitions::{add_definition, delete_definition, get_settings, list_definitions, update_settings};
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

/// The Field Service / Equipment Maintenance module.
pub struct FieldServiceModule {
    pub(crate) state: FieldServiceState,
}

impl FieldServiceModule {
    pub fn new() -> Self {
        Self {
            state: FieldServiceState::seeded(),
        }
    }
}

impl Default for FieldServiceModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for FieldServiceModule {
    fn id(&self) -> &'static str {
        "field_service"
    }

    fn name(&self) -> &'static str {
        "Field Service & Equipment Maintenance"
    }

    fn description(&self) -> &'static str {
        "Field service and equipment maintenance: equipment registry linked to assets, recurring maintenance plans with next-due computation and overdue detection, work orders with a validated status machine, checklist items and required parts, technician dispatch board, time logging through the timetracker module, parts consumption requests to assets, customer sign-off with completion reports, and post-job satisfaction follow-up through the surveys module."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["assets", "projects", "incidents", "timetracker"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["assets", "projects", "incidents", "timetracker"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["actix-web", "dashmap", "serde", "chrono", "uuid"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("field_service", AI_CAPABILITIES);
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
            ASSETS_UPDATED,
            "field_service",
            Arc::new(move |ctx: &mut HookContext| on_assets_updated(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            PLANS_CHECK_REQUESTED,
            "field_service",
            Arc::new(move |ctx: &mut HookContext| on_plans_check_requested(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/field-service")
                .app_data(state)
                // Literal segments first: they must win over `/{id}`.
                .route("/plans/due", web::get().to(list_due_plans))
                .route("/plans/overdue", web::get().to(list_overdue_plans))
                .route("/plans/check-due", web::post().to(check_due_plans))
                .route("/dispatch/board", web::get().to(dispatch_board))
                .route("/equipment-types", web::get().to(list_equipment_types))
                .route("/equipment-types", web::post().to(create_equipment_type))
                .route("/equipment-types/{id}", web::get().to(get_equipment_type))
                .route("/equipment-types/{id}", web::patch().to(update_equipment_type))
                .route("/equipment-types/{id}", web::delete().to(delete_equipment_type))
                .route("/equipment", web::get().to(list_equipment))
                .route("/equipment", web::post().to(create_equipment))
                .route("/equipment/{id}", web::get().to(get_equipment))
                .route("/equipment/{id}", web::patch().to(update_equipment))
                .route("/equipment/{id}", web::delete().to(delete_equipment))
                .route("/equipment/{id}/report-failure", web::post().to(report_failure))
                .route("/maintenance-templates", web::get().to(list_templates))
                .route("/maintenance-templates", web::post().to(create_template))
                .route("/maintenance-templates/{id}", web::get().to(get_template))
                .route("/maintenance-templates/{id}", web::patch().to(update_template))
                .route("/maintenance-templates/{id}", web::delete().to(delete_template))
                .route("/maintenance-templates/{id}/apply", web::post().to(apply_template))
                .route("/plans", web::get().to(list_plans))
                .route("/plans", web::post().to(create_plan))
                .route("/plans/{id}", web::get().to(get_plan))
                .route("/plans/{id}", web::patch().to(update_plan))
                .route("/plans/{id}", web::delete().to(delete_plan))
                .route("/plans/{id}/recompute", web::post().to(recompute_plan))
                .route("/work-orders", web::get().to(list_work_orders))
                .route("/work-orders", web::post().to(create_work_order))
                .route("/work-orders/{id}", web::get().to(get_work_order))
                .route("/work-orders/{id}", web::patch().to(update_work_order))
                .route("/work-orders/{id}", web::delete().to(delete_work_order))
                .route("/work-orders/{id}/status", web::post().to(change_work_order_status))
                .route("/work-orders/{id}/assign", web::post().to(assign_work_order))
                .route("/work-orders/{id}/cancel", web::post().to(cancel_work_order))
                .route("/work-orders/{id}/items", web::get().to(list_work_order_items))
                .route("/work-orders/{id}/items", web::post().to(add_work_order_item))
                .route("/work-orders/{id}/items/{item_id}", web::patch().to(update_work_order_item))
                .route("/work-orders/{id}/items/{item_id}", web::delete().to(delete_work_order_item))
                .route("/work-orders/{id}/items/{item_id}/complete", web::post().to(complete_work_order_item))
                .route("/work-orders/{id}/parts", web::get().to(list_work_order_parts))
                .route("/work-orders/{id}/parts", web::post().to(add_work_order_part))
                .route("/work-orders/{id}/parts/{part_id}", web::patch().to(update_work_order_part))
                .route("/work-orders/{id}/parts/{part_id}", web::delete().to(delete_work_order_part))
                .route("/work-orders/{id}/parts/{part_id}/consume", web::post().to(consume_work_order_part))
                .route("/work-orders/{id}/time-log", web::post().to(log_work_order_time))
                .route("/work-orders/{id}/raise-incident", web::post().to(raise_incident))
                .route("/work-orders/{id}/sign-off", web::post().to(sign_off_work_order))
                .route("/work-orders/{id}/sign-off", web::get().to(get_work_order_signoff))
                .route("/work-orders/{id}/report", web::get().to(work_order_report))
                .route("/dispatch/assignments", web::get().to(list_dispatch_assignments))
                .route("/dispatch/assignments/{id}/release", web::post().to(release_dispatch_assignment))
                .route("/dispatch/assignments/{id}/complete", web::post().to(complete_dispatch_assignment))
                .route("/definitions", web::get().to(list_definitions))
                .route("/definitions/{kind}", web::post().to(add_definition))
                .route("/definitions/{kind}/{value}", web::delete().to(delete_definition))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/field-service/equipment-types", owner: "field_service", description: "List equipment types (paginated, filter by slug, search).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/equipment-types", owner: "field_service", description: "Create an equipment type (admin-editable definition).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/equipment-types/{id}", owner: "field_service", description: "Get one equipment type.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/field-service/equipment-types/{id}", owner: "field_service", description: "Update an equipment type.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/field-service/equipment-types/{id}", owner: "field_service", description: "Delete an equipment type with no equipment using it.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/equipment", owner: "field_service", description: "List equipment (paginated, filter by type, status, location, asset_id, search).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/equipment", owner: "field_service", description: "Register equipment (asset_id is a reference to the assets module).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/equipment/{id}", owner: "field_service", description: "Get one equipment item.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/field-service/equipment/{id}", owner: "field_service", description: "Update equipment (retired equipment cannot be edited).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/field-service/equipment/{id}", owner: "field_service", description: "Delete equipment with no active plans or open work orders.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/equipment/{id}/report-failure", owner: "field_service", description: "Report a failure: opens a corrective work order and requests an incident.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/maintenance-templates", owner: "field_service", description: "List maintenance templates (paginated, filter by equipment_type_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/maintenance-templates", owner: "field_service", description: "Create a maintenance template (admin-editable).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/maintenance-templates/{id}", owner: "field_service", description: "Get one maintenance template.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/field-service/maintenance-templates/{id}", owner: "field_service", description: "Update a maintenance template.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/field-service/maintenance-templates/{id}", owner: "field_service", description: "Delete a maintenance template with no plans using it.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/maintenance-templates/{id}/apply", owner: "field_service", description: "Create a maintenance plan from a template for one equipment item.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/plans", owner: "field_service", description: "List maintenance plans (paginated, filter by equipment_id, is_active, overdue).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/plans", owner: "field_service", description: "Create a maintenance plan (next-due date is computed from the interval).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/plans/due", owner: "field_service", description: "Plans due within ?days (default 7), including overdue ones.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/plans/overdue", owner: "field_service", description: "Overdue maintenance plans.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/plans/check-due", owner: "field_service", description: "Run the due/overdue check now and notify plan owners.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/plans/{id}", owner: "field_service", description: "Get one maintenance plan.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/field-service/plans/{id}", owner: "field_service", description: "Update a maintenance plan (next-due date is recomputed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/field-service/plans/{id}", owner: "field_service", description: "Delete a maintenance plan with no open work orders.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/plans/{id}/recompute", owner: "field_service", description: "Recompute a plan's next-due date from its interval and last service.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/work-orders", owner: "field_service", description: "List work orders (paginated, filter by equipment_id, plan_id, project_id, status, type, priority, assignee_id, search).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders", owner: "field_service", description: "Create a work order (checklist and required parts can be seeded inline).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/work-orders/{id}", owner: "field_service", description: "Get one work order with its checklist items and parts.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/field-service/work-orders/{id}", owner: "field_service", description: "Update a work order (closed/cancelled orders cannot be edited).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/field-service/work-orders/{id}", owner: "field_service", description: "Delete an open or cancelled work order.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/status", owner: "field_service", description: "Validated status transition (open -> assigned -> in_progress -> on_hold -> completed -> verified -> closed, with cancel).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/assign", owner: "field_service", description: "Assign a technician (creates a dispatch assignment and notifies them).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/cancel", owner: "field_service", description: "Cancel a work order (releases the active dispatch assignment).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/work-orders/{id}/items", owner: "field_service", description: "Checklist items of a work order (paginated, ordered by position).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/items", owner: "field_service", description: "Add a checklist item.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/field-service/work-orders/{id}/items/{item_id}", owner: "field_service", description: "Update a checklist item.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/field-service/work-orders/{id}/items/{item_id}", owner: "field_service", description: "Delete a checklist item.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/items/{item_id}/complete", owner: "field_service", description: "Mark a checklist item done.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/work-orders/{id}/parts", owner: "field_service", description: "Parts of a work order (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/parts", owner: "field_service", description: "Add a required part to a work order.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/field-service/work-orders/{id}/parts/{part_id}", owner: "field_service", description: "Update a part line.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/field-service/work-orders/{id}/parts/{part_id}", owner: "field_service", description: "Delete a part line that has not been consumed.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/parts/{part_id}/consume", owner: "field_service", description: "Request stock consumption for a part (emits assets.stock.adjust_requested; stock is never decremented here).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/time-log", owner: "field_service", description: "Log time on a work order through the timetracker module (event request).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/raise-incident", owner: "field_service", description: "Request an incident for a failure found on the job (event request).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/sign-off", owner: "field_service", description: "Customer sign-off (completed -> verified; optional satisfaction survey follow-up).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/work-orders/{id}/sign-off", owner: "field_service", description: "Get the sign-off record of a work order.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/work-orders/{id}/report", owner: "field_service", description: "Completion report data: work order, checklist, parts, dispatch, and sign-off.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/dispatch/board", owner: "field_service", description: "Dispatch board: per-technician ordered job lists with availability (priority/due-date heuristic, not a route optimiser).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/dispatch/assignments", owner: "field_service", description: "List dispatch assignments (paginated, filter by technician_id, work_order_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/dispatch/assignments/{id}/release", owner: "field_service", description: "Release a technician from a dispatch assignment.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/dispatch/assignments/{id}/complete", owner: "field_service", description: "Mark a dispatch assignment completed.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/definitions", owner: "field_service", description: "Every admin-editable value registry (work order types, priorities, equipment statuses, interval units).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/field-service/definitions/{kind}", owner: "field_service", description: "Add a value to a registry (interval_unit needs days_per_unit).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/field-service/definitions/{kind}/{value}", owner: "field_service", description: "Remove a custom registry value that nothing uses.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/field-service/settings", owner: "field_service", description: "Get the module settings.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/field-service/settings", owner: "field_service", description: "Update the module settings.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(FieldServiceModule::new()));
}
