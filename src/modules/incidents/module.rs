//! incidents - module.

// Incident & On-Call module - incidents with a validated status
// machine and an append-only timeline, on-call rotations and
// schedules with overrides, escalation policies with paging, and
// postmortems whose action items become tasks.
//
// Routes live under `/api/v1/incidents`:
//
//   Incidents:
//     GET/POST / - list incidents / open an incident.
//     GET/PATCH /{id}.
//     POST /{id}/acknowledge - acknowledge (starts the clock).
//     POST /{id}/status - validated status transition.
//     POST /{id}/page - page the on-call responder.
//     POST /{id}/escalate - escalate to the next policy step.
//     GET  /{id}/timeline - append-only timeline entries.
//     POST /{id}/timeline - add a note entry.
//     POST /{id}/responders - add a responder.
//     DELETE /{id}/responders/{user_id} - remove a responder.
//   Postmortems:
//     GET  /{id}/postmortem.
//     POST /{id}/postmortem - create (resolved / closed only).
//     POST /{id}/postmortem/action-items - add an action item.
//     PATCH /{id}/postmortem/action-items/{item_id} - update one.
//   On-call rotations:
//     GET/POST /rotations - list rotations / create a rotation.
//     GET/PATCH/DELETE /rotations/{id}.
//     POST /rotations/{id}/members - add a rotation member.
//     DELETE /rotations/{id}/members/{user_id} - remove a member.
//     GET  /rotations/{id}/on-call - who is on call now.
//     GET/POST /rotations/{id}/schedules - list / create a schedule
//       entry (kind `regular` or `override`).
//     POST /rotations/{id}/overrides - create an override entry.
//   Escalation policies:
//     GET/POST /escalation-policies.
//     GET/PATCH/DELETE /escalation-policies/{id}.
//     POST /escalations/run - process due escalations (called by
//       the scheduler or an operator; pages the next responder).
//   Service Monitor linkage:
//     GET/POST /auto-open - list / set per-service auto-open rules.
//     POST /auto-open/{service_id} - set one service's rule.
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory incident, timeline, rotation, schedule, escalation
// policy, page-attempt, and postmortem management; the validated
// status machine (`open` -> `acknowledged` -> `investigating` ->
// `mitigating` -> `resolved` -> `closed`, with `resolved` ->
// `investigating` reopen and `investigating` -> `acknowledged`
// step-back); an append-only timeline (entries are never updated
// or deleted); on-call rotation order with handover times and
// schedule entries where `override` entries take precedence over
// `regular` ones; escalation policies as ordered steps with a
// per-incident escalation state; paging that emits
// `notification.requested` with high priority and records the
// page attempt with acknowledgement tracking; postmortems linked
// to resolved or closed incidents. What is NOT built here: actual
// SMS / phone-call delivery (the notifications module owns
// delivery - this module only emits `notification.requested`);
// automatic escalation on a timer inside this module (escalation
// only moves when `POST /escalations/run` is called - the
// scheduler must call it, or an operator must; the run endpoint
// is the honest boundary, exactly like the events module's
// reminder scheduler note); and task creation itself - a
// postmortem action item emits `tasks.create.requested` with a
// full task payload for the tasks module, but no listener in the
// tasks module consumes that event yet, so today the action item
// is stored here with status `open` and the request event is the
// integration point (see the integrator note in the module
// report).
//
// Public-access gate: this module has no public routes. Every
// route below requires the caller headers and the `incidents.view`
// or `incidents.manage` capability.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `incidents.created`, `incidents.updated`,
//     `incidents.acknowledged`, `incidents.status_changed`,
//     `incidents.resolved`, `incidents.closed`
//   - `incidents.paged`, `incidents.page.acknowledged`,
//     `incidents.escalated`
//   - `incidents.postmortem.created`,
//     `incidents.postmortem.action_item.created`
//   - `incidents.timeline.added`
//   - `tasks.create.requested` (postmortem action items - see the
//     honesty note above; no tasks listener consumes it yet)
//   - `notification.requested` (paging, escalation, new incidents)
//     with the notifications module's payload shape: `{"kind",
//     "user_id", "title", "body", "module": "incidents", "link",
//     "priority", "channels"}`.
// Events this module listens to:
//   - `cluster.health.changed` (Service Monitor) - a `down`
//     health for a service with an enabled auto-open rule opens
//     an incident (deduplicated: no second open auto-incident for
//     the same service); a recovery appends a timeline note to
//     open auto-incidents for that service and resolves them when
//     the rule asks for it. Auto-open is off by default for every
//     service.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;
use super::{constants::*, types::*, inputs::*, state::*, helpers::*, handlers_incidents::*, handlers_rotations::*, handlers_policies::*, handlers_postmortems::*, handlers_auto_open::*};
use super::ai_capabilities::AI_CAPABILITIES;



// Module
pub struct IncidentsModule {
    pub(crate) state: IncidentsState,
}


impl IncidentsModule {
    pub fn new() -> Self {
        Self {
            state: IncidentsState::default(),
        }
    }
}


impl Default for IncidentsModule {
    fn default() -> Self {
        Self::new()
    }
}


impl Module for IncidentsModule {
    fn id(&self) -> &'static str {
        "incidents"
    }

    fn name(&self) -> &'static str {
        "Incident & On-Call"
    }

    fn description(&self) -> &'static str {
        "Incidents with a validated status machine and append-only timeline, on-call rotations and schedules with overrides, escalation policies with paging, postmortems whose action items become tasks, and auto-open from Service Monitor alerts (opt-in per service)."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "service_monitor", "tasks"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "service_monitor", "tasks"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("infrastructure")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("incidents", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = Arc::new(self.state.clone());
        bus.register(
            SERVICE_HEALTH_CHANGED,
            "incidents",
            Arc::new(move |ctx: &mut HookContext| {
                on_service_health_changed(Arc::clone(&state), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/incidents")
                .app_data(state)
                .route("", web::get().to(list_incidents))
                .route("", web::post().to(create_incident))
                .route("/rotations", web::get().to(list_rotations))
                .route("/rotations", web::post().to(create_rotation))
                .route("/rotations/{id}", web::get().to(get_rotation))
                .route("/rotations/{id}", web::patch().to(update_rotation))
                .route("/rotations/{id}", web::delete().to(delete_rotation))
                .route("/rotations/{id}/members", web::post().to(add_rotation_member))
                .route(
                    "/rotations/{id}/members/{user_id}",
                    web::delete().to(remove_rotation_member),
                )
                .route("/rotations/{id}/on-call", web::get().to(get_on_call))
                .route("/rotations/{id}/handover", web::post().to(handover_rotation))
                .route("/rotations/{id}/schedules", web::get().to(list_schedules))
                .route("/rotations/{id}/schedules", web::post().to(create_schedule))
                .route("/rotations/{id}/overrides", web::post().to(create_override))
                .route("/escalation-policies", web::get().to(list_policies))
                .route("/escalation-policies", web::post().to(create_policy))
                .route("/escalation-policies/{id}", web::get().to(get_policy))
                .route("/escalation-policies/{id}", web::patch().to(update_policy))
                .route("/escalation-policies/{id}", web::delete().to(delete_policy))
                .route("/escalations/run", web::post().to(run_escalations))
                .route("/auto-open", web::get().to(list_auto_open))
                .route("/auto-open/{service_id}", web::post().to(set_auto_open))
                .route("/{id}", web::get().to(get_incident))
                .route("/{id}", web::patch().to(update_incident))
                .route("/{id}/acknowledge", web::post().to(acknowledge_incident))
                .route("/{id}/status", web::post().to(change_incident_status))
                .route("/{id}/page", web::post().to(page_incident))
                .route("/{id}/escalate", web::post().to(escalate_incident))
                .route("/{id}/timeline", web::get().to(list_timeline))
                .route("/{id}/timeline", web::post().to(add_timeline_note))
                .route("/{id}/responders", web::post().to(add_responder))
                .route("/{id}/responders/{user_id}", web::delete().to(remove_responder))
                .route("/{id}/postmortem", web::get().to(get_postmortem))
                .route("/{id}/postmortem", web::post().to(create_postmortem))
                .route("/{id}/postmortem/action-items", web::post().to(add_action_item))
                .route(
                    "/{id}/postmortem/action-items/{item_id}",
                    web::patch().to(update_action_item),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/incidents", owner: "incidents", description: "List incidents (paginated, filter by status, severity, responder_id, service_id, source).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/incidents", owner: "incidents", description: "Open an incident (manual source; responders are notified).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/incidents/{id}", owner: "incidents", description: "Get one incident.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/incidents/{id}", owner: "incidents", description: "Update an incident (not when closed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/incidents/{id}/acknowledge", owner: "incidents", description: "Acknowledge an incident (open -> acknowledged; stops escalation; closes open page attempts).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/incidents/{id}/status", owner: "incidents", description: "Validated status transition (open -> acknowledged -> investigating -> mitigating -> resolved -> closed, with reopen).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/incidents/{id}/page", owner: "incidents", description: "Page the on-call responder (high-priority notification.requested; attempt recorded).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/incidents/{id}/escalate", owner: "incidents", description: "Escalate to the next policy step (pages the step target).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/incidents/{id}/timeline", owner: "incidents", description: "List the append-only timeline (filter by kind, actor_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/incidents/{id}/timeline", owner: "incidents", description: "Add a note entry to the timeline.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/incidents/{id}/responders", owner: "incidents", description: "Add a responder to an incident.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/incidents/{id}/responders/{user_id}", owner: "incidents", description: "Remove a responder from an incident.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/incidents/{id}/postmortem", owner: "incidents", description: "Get the postmortem of an incident.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/incidents/{id}/postmortem", owner: "incidents", description: "Create a postmortem (resolved or closed incidents only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/incidents/{id}/postmortem/action-items", owner: "incidents", description: "Add a postmortem action item (emits tasks.create.requested for the tasks module).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/incidents/{id}/postmortem/action-items/{item_id}", owner: "incidents", description: "Update a postmortem action item (title, assignee, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/incidents/rotations", owner: "incidents", description: "List on-call rotations.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/incidents/rotations", owner: "incidents", description: "Create an on-call rotation (ordered members, handover interval and time).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/incidents/rotations/{id}", owner: "incidents", description: "Get one rotation.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/incidents/rotations/{id}", owner: "incidents", description: "Update a rotation (name, handover interval/time, timezone).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/incidents/rotations/{id}", owner: "incidents", description: "Delete a rotation not used by incidents.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/incidents/rotations/{id}/members", owner: "incidents", description: "Add a member to a rotation (appended at the end of the order).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/incidents/rotations/{id}/members/{user_id}", owner: "incidents", description: "Remove a member from a rotation.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/incidents/rotations/{id}/on-call", owner: "incidents", description: "Who is on call now (override entries win over regular ones).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/incidents/rotations/{id}/handover", owner: "incidents", description: "Hand over to the next rotation member (advances current_index).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/incidents/rotations/{id}/schedules", owner: "incidents", description: "List schedule entries of a rotation (filter by kind, member_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/incidents/rotations/{id}/schedules", owner: "incidents", description: "Create a schedule entry (kind regular or override).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/incidents/rotations/{id}/overrides", owner: "incidents", description: "Create an override schedule entry.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/incidents/escalation-policies", owner: "incidents", description: "List escalation policies.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/incidents/escalation-policies", owner: "incidents", description: "Create an escalation policy (ordered steps with strictly increasing after_minutes).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/incidents/escalation-policies/{id}", owner: "incidents", description: "Get one escalation policy.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/incidents/escalation-policies/{id}", owner: "incidents", description: "Update an escalation policy.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/incidents/escalation-policies/{id}", owner: "incidents", description: "Delete an escalation policy not used by incidents.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/incidents/escalations/run", owner: "incidents", description: "Process due escalations (called by the scheduler or an operator; pages the next responder).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/incidents/auto-open", owner: "incidents", description: "List per-service auto-open rules (off by default).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/incidents/auto-open/{service_id}", owner: "incidents", description: "Set one service's auto-open rule (enabled, severity, auto-resolve on recovery).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}


pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(IncidentsModule::new()));
}
