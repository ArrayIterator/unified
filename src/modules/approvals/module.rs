// Generic Approvals module — data-driven approval workflows for any
// record in any module, without duplicating another module's data.
//
// Routes live under `/api/v1/approvals`:
//
//   Definitions (admin: what gets approved, and by whom):
//     GET/POST /definitions — list definitions / create a definition.
//     GET/PATCH/DELETE /definitions/{id}.
//     POST /definitions/{id}/steps — append a step to a definition.
//     DELETE /definitions/{id}/steps/{step_id} — remove a step.
//   Requests (one approval flow for one record):
//     GET/POST /requests — list requests / create a request.
//     GET /requests/{id} — one request with its steps and decisions.
//     POST /requests/{id}/decide — approve / reject / request-changes.
//     POST /requests/{id}/resubmit — return a changes-requested or
//       expired request to in_progress (new decision round).
//     POST /requests/{id}/cancel — cancel an open request.
//     POST /requests/{id}/reassign — replace the approvers of one
//       pending step (admin; used for escalation handover).
//     GET /requests/{id}/history — append-only decision history.
//     POST /requests/sweep-expiry — expire overdue requests and
//       escalate them (admin; see the expiry note below).
//   Dashboards:
//     GET /my-pending — requests waiting on the caller (as approver
//       or as an active delegate), paginated.
//     GET /my-requests — requests the caller asked for, paginated.
//     GET /overdue — requests past their deadline, paginated.
//     GET /by-record?module=&record_type=&record_id= — requests and
//       decisions for one record.
//   Delegations:
//     GET/POST /delegations — list / create a delegation.
//     DELETE /delegations/{id} — revoke a delegation.
//   Settings (admin-editable configurable defaults):
//     GET/PATCH /settings.
//
//
// HOOK CONTRACT — the core of this module. Other modules never call
// this module directly; everything crosses the hook bus.
//
//   `approval.requested` (emitted BY any module, consumed HERE).
//   Payload:
//     {
//       "module": "invoices",            // owning module id (required)
//       "record_type": "invoice",        // record kind in that module (required)
//       "record_id": "9f...",            // record id as text (required)
//       "requester_id": "user-42",        // who asks (required)
//       "summary": "Invoice #1042 — 1,250,000 IDR",  // (required)
//       "amount_minor": 125000000,       // optional, integer minor units
//       "currency": "IDR",               // optional, ISO 4217 code
//       "company_id": "acme"             // optional tenant scope
//     }
//   The handler finds the active definition for
//   (`module`, `record_type`); when none exists it answers
//   `{"accepted": false, "error": "No active approval definition ..."}`
//   and creates nothing. When one exists it creates the request and
//   answers `{"accepted": true, "request_id": "...",
//   "status": "in_progress"}` via `add_result` and the `result` key.
//   The requesting module keeps the `request_id` and uses it to
//   correlate the `approval.decided` answer.
//
//   `approval.decided` (emitted BY this module, consumed by the
//   requesting module). Payload:
//     {
//       "request_id": "9f...",
//       "decision": "approved" | "rejected" | "changes_requested",
//       "decided_by": "user-7",          // last deciding approver
//       "module": "invoices",
//       "record_type": "invoice",
//       "record_id": "9f...",
//       "requester_id": "user-42"
//     }
//   Emitted exactly once per request, when it reaches a terminal
//   decision. The requesting module applies the outcome to its own
//   record; this module never writes to another module's tables.
//
//   Other events emitted by this module:
//     - `approval.created` — `{"request_id", "module", "record_type",
//       "record_id", "requester_id", "definition_id"}`.
//     - `approval.resubmitted` — `{"request_id", "round"}`.
//     - `approval.cancelled` — `{"request_id", "cancelled_by"}`.
//     - `approval.escalated` — `{"request_id", "step_index",
//       "escalation_count", "action"}`.
//     - `approval.expired` — `{"request_id", "step_index"}` (only
//       when the escalation rule gives up and the request expires).
//     - `notification.requested` — to the current step's approvers
//       when a request is created, and to the escalation target plus
//       the current approvers when a request escalates, using the
//       notifications module's payload shape (`to_user_id`,
//       `title`, `body`). Delivery is the notifications module's job.
//
//   Events this module emits to resolve people (never duplicated
//   user records — approvers are always resolved through other
//   modules):
//     - `user.manager.query` — `{"user_id": "..."}`; the HR / users
//       module answers with `{"manager_id": "..."}` (read from the
//       `manager_id` key). When nobody answers, a `manager`
//       approver slot stays unresolved: the step cannot be decided
//       by that slot and the request lists
//       `"resolution": "unresolved"` for it, so an administrator can
//       reassign the step instead of the request stalling silently.
//
//
// DATA MODEL — everything is data, never baked into code:
//
//   An approval definition names a (`module`, `record_type`) pair and
//   owns ordered steps. A step has a `mode` (`all` — every approver
//   slot must approve; `any` — one approval completes the step) and a
//   list of approver slots. A slot is `{kind, ref}` where kind is
//   `user` (ref is the user id), `role` (ref is the role slug; any
//   caller whose `X-Role` matches satisfies the slot), or `manager`
//   (the requester's manager, resolved through the hook above; the
//   ref is ignored). Steps run in `position` order; a request
//   advances one step at a time.
//
//   A request is polymorphic: (`module`, `record_type`, `record_id`)
//   are plain text. There is deliberately NO foreign key into
//   another module's tables — the owning module owns its records.
//
//   Delegation: an approver delegates to another user for a date
//   range. A delegate deciding records `acting_for` with the original
//   approver's id, so the history always shows who really decided
//   and on whose behalf.
//
//   Decisions are append-only history: `approve`, `reject`, or
//   `request_changes`, each with a comment (required for `reject` and
//   `request_changes`, optional for `approve` unless the settings
//   require it), a timestamp, and the acting identity. Decisions are
//   never edited or deleted — there is no update/delete endpoint for
//   them, and the SQL migration adds a trigger that rejects
//   UPDATE/DELETE on the decisions table.
//
//
// REQUEST LIFECYCLE — the state machine:
//
//   requested -> in_progress -> approved | rejected |
//   changes_requested -> (resubmit) -> in_progress
//   in_progress -> expired (only when escalation gives up; a
//   resubmit from expired is allowed) ; any open state -> cancelled.
//
//   A new request starts `requested` and moves to `in_progress`
//   immediately when its definition has at least one active step; a
//   definition with no active steps leaves the request `requested`
//   until a step is added. A `reject` anywhere ends the request as
//   `rejected`. `request_changes` ends the round as
//   `changes_requested`; a resubmit starts a new decision round
//   (decisions keep their `round` number, so earlier rounds stay
//   visible in history and only the current round counts towards
//   step completion). Completing the last step ends the request as
//   `approved`.
//
//   Expiry: every request carries `expires_at`, computed from its
//   definition's `timeout_hours` (or the settings default when the
//   definition leaves it unset). Expiry NEVER silently auto-approves.
//   The sweep (`POST /requests/sweep-expiry`, or any scheduler job
//   that calls it) finds overdue `in_progress` requests and applies
//   the definition's escalation rule: `notify` (notify the escalation
//   target and the current approvers, extend the deadline by
//   `escalation_extension_hours`), `notify_and_reassign` (same, and
//   the caller is expected to reassign the step through
//   `POST /requests/{id}/reassign` — the escalation record names the
//   target), or `none` (the request expires immediately). After
//   `max_escalations` escalations (settings default 3) the request
//   expires. An expired request can be resubmitted.
//
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory definition / step / request / decision / delegation /
// escalation management in DashMaps (the in-memory store is
// authoritative; the SQL migration mirrors the tables for the
// persistence layer, exactly like the other modules that landed
// before the database layer was wired in); the full hook contract
// above; step completion for `all`/`any` modes; delegation with
// acting-for recording; append-only decisions; expiry sweep with
// escalation and a hard cap; per-request step reassignment;
// dashboards (my pending / my requests / overdue / per-record
// history); admin-editable settings for every configurable default.
//
// What is NOT built here:
// - No persistence beyond the in-memory store: the migration
//   creates the tables, but no Diesel queries run against them yet.
//   A restart loses in-memory state until the persistence layer is
//   wired (same standing gap as the sibling modules).
// - No real user directory: user ids and role slugs are opaque
//   strings. `role` slots are satisfied by the caller's `X-Role`
//   header, and `manager` slots depend on the HR / users module
//   answering `user.manager.query` (see above). Nothing here
//   duplicates user records.
// - The escalation target is notified, not granted decision rights
//   by the escalation itself; handing the step to someone else is
//   the administrator's explicit `reassign` call.
// - `notification.requested` is only emitted: delivery belongs to
//   the notifications module.
//
// Public-access gate: every route requires `X-User-Id` and `X-Role`
// headers and a capability check through the `role.check` hook,
// which fails closed (no answer means not allowed). Reads need
// `approvals.view`; writes need `approvals.manage`; recording a
// decision needs `approvals.decide` (an approver does not need
// `approvals.manage`). There is no token-authenticated public route.
// Like every other module, the API Manager's `ApiAccessRule` gate
// still applies on top (default deny).
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs against,
// exactly like the events and workflows modules.
//
// Security: capability checks on every endpoint; all input validated
// (lengths, UUIDs, enums, amounts); no raw SQL (Diesel only for the
// future persistence layer — this file issues no SQL at all);
// secrets are never stored (there are no secrets in this module);
// output is serialized through serde, never string-built HTML.


use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::TenantContext as TenancyContext;
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_definitions::*;
use super::handlers_requests::*;
use super::handlers_dashboards::*;
use super::hooks::*;
use super::ai_capabilities::{AI_CAPABILITIES};

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct ApprovalsModule {
    state: ApprovalsState,
}

impl ApprovalsModule {
    pub fn new() -> Self {
        Self {
            state: ApprovalsState::default(),
        }
    }
}

impl Default for ApprovalsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ApprovalsModule {
    fn id(&self) -> &'static str {
        "approvals"
    }

    fn name(&self) -> &'static str {
        "Approvals"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }

    fn description(&self) -> &'static str {
        "Data-driven approval workflows for any record in any module: sequential and parallel steps, user / role / manager approvers, delegation, append-only decisions, expiry with escalation, and a hook contract (approval.requested / approval.decided)."
    }

    fn requires(&self) -> &[&'static str] {
        &["roles", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["roles", "notifications"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("approvals", AI_CAPABILITIES);
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
            APPROVAL_REQUESTED,
            "approvals",
            Arc::new(move |ctx: &mut HookContext| {
                on_approval_requested(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            "approvals.sweep_expiry_requested",
            "approvals",
            Arc::new(move |ctx: &mut HookContext| {
                on_sweep_expiry_requested(Arc::clone(&state), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/approvals")
                .app_data(state)
                // Literal segments first: they must win over `/{id}`.
                .route("/definitions", web::get().to(list_definitions))
                .route("/definitions", web::post().to(create_definition))
                .route("/definitions/{id}", web::get().to(get_definition))
                .route("/definitions/{id}", web::patch().to(update_definition))
                .route("/definitions/{id}", web::delete().to(delete_definition))
                .route("/definitions/{id}/steps", web::post().to(add_definition_step))
                .route(
                    "/definitions/{id}/steps/{step_id}",
                    web::delete().to(delete_definition_step),
                )
                .route("/requests", web::get().to(list_requests))
                .route("/requests", web::post().to(create_request))
                .route("/requests/sweep-expiry", web::post().to(sweep_expiry))
                .route("/requests/{id}", web::get().to(get_request))
                .route("/requests/{id}/decide", web::post().to(decide_request))
                .route("/requests/{id}/resubmit", web::post().to(resubmit_request))
                .route("/requests/{id}/cancel", web::post().to(cancel_request))
                .route("/requests/{id}/reassign", web::post().to(reassign_request_step))
                .route("/requests/{id}/history", web::get().to(request_history))
                .route("/my-pending", web::get().to(my_pending))
                .route("/my-requests", web::get().to(my_requests))
                .route("/overdue", web::get().to(overdue_requests))
                .route("/by-record", web::get().to(by_record))
                .route("/delegations", web::get().to(list_delegations))
                .route("/delegations", web::post().to(create_delegation))
                .route("/delegations/{id}", web::delete().to(delete_delegation))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/approvals/definitions", owner: "approvals", description: "List approval definitions (paginated, filter by module, record_type, is_active, company_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/approvals/definitions", owner: "approvals", description: "Create an approval definition with optional steps (one active definition per module / record_type).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/approvals/definitions/{id}", owner: "approvals", description: "Get one definition with its steps.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/approvals/definitions/{id}", owner: "approvals", description: "Update a definition (name, activity, timeout, escalation rule).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/approvals/definitions/{id}", owner: "approvals", description: "Delete a definition with its steps (refused while open requests exist).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/approvals/definitions/{id}/steps", owner: "approvals", description: "Append a step (mode all/any, user / role / manager approvers) to a definition.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/approvals/definitions/{id}/steps/{step_id}", owner: "approvals", description: "Remove a step (refused when open requests have reached it).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/approvals/requests", owner: "approvals", description: "List approval requests (paginated, filter by module, record_type, record_id, requester_id, status, definition_id, company_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/approvals/requests", owner: "approvals", description: "Create an approval request for a record under the active definition.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/approvals/requests/{id}", owner: "approvals", description: "Get one request with its steps (live status) and decision history.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/approvals/requests/{id}/decide", owner: "approvals", description: "Decide the current step: approve / reject / request_changes (comment required for reject / request_changes).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/approvals/requests/{id}/resubmit", owner: "approvals", description: "Resubmit a changes_requested or expired request (new decision round).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/approvals/requests/{id}/cancel", owner: "approvals", description: "Cancel an open request.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/approvals/requests/{id}/reassign", owner: "approvals", description: "Replace the approvers of one pending step (admin; escalation handover).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/approvals/requests/{id}/history", owner: "approvals", description: "Append-only decision history plus escalations for one request (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/approvals/requests/sweep-expiry", owner: "approvals", description: "Expire overdue requests and escalate them (never auto-approves; gives up after the escalation cap).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/approvals/my-pending", owner: "approvals", description: "Requests waiting on the caller as approver or active delegate (paginated, soonest deadline first).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/approvals/my-requests", owner: "approvals", description: "Requests the caller asked for (paginated, filter by status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/approvals/overdue", owner: "approvals", description: "In-progress requests past their deadline (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/approvals/by-record", owner: "approvals", description: "Requests and decisions for one record (?module=&record_type=&record_id=, paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/approvals/delegations", owner: "approvals", description: "List delegations (paginated, filter by delegator_id, delegate_id, is_active).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/approvals/delegations", owner: "approvals", description: "Create a temporary delegation (delegate decides as acting-for the original approver).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/approvals/delegations/{id}", owner: "approvals", description: "Revoke a delegation (soft revoke; past acting-for decisions keep their meaning).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/approvals/settings", owner: "approvals", description: "Read the admin-editable approval defaults (timeouts, escalation cap, limits, self-approval).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/approvals/settings", owner: "approvals", description: "Update the admin-editable approval defaults.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ApprovalsModule::new()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_dead_status_query_listener_is_registered() {
        // `approval.status.query` had a listener but no emitter
        // anywhere in the tree; it was removed. The request flow
        // itself stays reachable over HTTP, and the live
        // `approval.requested` listener must still register.
        let bus = HookBus::new();
        let module = ApprovalsModule::new();
        module.register_hooks(&bus);
        assert!(bus.has(APPROVAL_REQUESTED));
        assert!(!bus.has("approval.status.query"));
    }
}
