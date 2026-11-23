// Compliance Center module — GDPR / ISO support: data export
// requests, data deletion requests with an approval step, an
// append-only consent log, retention policies, processing-activity
// records (RoPA), compliance checklists with evidence, and a breach
// log.
//
// Routes live under `/api/v1/compliance`:
//
//   Exports:
//     GET/POST /exports — list / request an export.
//     GET  /exports/{id}.
//     POST /exports/{id}/start — mark processing (manager).
//     POST /exports/{id}/complete — attach the storage link with
//          expiry (manager).
//     GET  /exports/{id}/download — the storage link while it is
//          still valid.
//   Deletions:
//     GET/POST /deletions — list / request a deletion.
//     GET  /deletions/{id}.
//     POST /deletions/{id}/approve, /deletions/{id}/reject.
//     POST /deletions/{id}/execute — dispatch per-module execution
//          (manager, only after approval).
//     GET  /deletions/{id}/items.
//     POST /deletions/{id}/items/{item_id}/mark-executed — confirm
//          one item executed (manager; for owning modules without
//          a hook listener).
//   Consents (append-only):
//     GET/POST /consents — list / record consent or withdrawal.
//     GET  /consents/{id}.
//   Retention policies:
//     GET/POST /retention-policies — list / create.
//     GET/PATCH/DELETE /retention-policies/{id}.
//     POST /retention-policies/{id}/review — enqueue the
//          retention-review scheduler job (flags items, never
//          deletes).
//     GET  /retention-reviews — review runs and their flags.
//   Processing-activity records (RoPA):
//     GET/POST /processing-records — list / create.
//     GET/PATCH/DELETE /processing-records/{id}.
//   Checklists:
//     GET/POST /checklists — list / create.
//     GET/PATCH/DELETE /checklists/{id}.
//   Breach log:
//     GET/POST /breaches — list / report a breach.
//     GET/PATCH /breaches/{id}.
//     POST /breaches/{id}/timeline — append a timeline entry.
//     GET  /breaches/{id}/timeline.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory export, deletion, consent, retention-policy,
// processing-record, checklist, and breach-log management; the
// export state machine (requested -> processing -> ready, with a
// storage link and expiry; failed and expired states included);
// the deletion state machine (requested -> approved -> executing
// -> executed, with rejection); per-module deletion items whose
// execution is dispatched to the owning modules through the
// `compliance.deletion.item.execute_requested` event and confirmed
// back through `compliance.deletion.item.executed` (or through the
// manual mark-executed endpoint); an append-only consent log
// (withdrawal is a new row, never an overwrite, and there is no
// update or delete route); retention policies whose enforcement is
// a scheduler job (`compliance.retention.review`) that only flags
// items — nothing in this module ever auto-deletes data; RoPA
// records; checklists with storage references for evidence; and a
// breach log with an append-only timeline and notification status.
// What is NOT built here: the export file itself (assembly belongs
// to the owning modules — this module requests it through
// `compliance.export.requested` and records the storage link the
// assembler returns through the complete endpoint); actual
// deletion of other modules' rows (this module tracks and audits
// only — owning modules delete their own rows when they answer
// the execute event); actual delivery of breach notifications
// (the notifications module owns delivery — this module only
// records notification status and emits
// `notification.requested`); and durable storage (the DashMap
// stores are authoritative; migration
// `2026-11-22-000057_compliance` is the persistence target).
//
// Scheduler note: `compliance.retention.review` is NOT in the
// scheduler's `KNOWN_KINDS` list yet, so today the scheduler
// answers `{"queued": false, "error": "Unknown job kind: ..."}`
// and the review run records `scheduler_status: "declined"`
// instead of pretending it was scheduled. The policy row and the
// review request are still stored; the follow-up is to add
// `"compliance.retention.review"` to `KNOWN_KINDS` in
// `scheduler.rs` and arm the dispatch so the job emits
// `compliance.retention.review_requested` with the policy id.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `compliance.export.requested`, `compliance.export.started`,
//     `compliance.export.completed`, `compliance.export.failed`
//   - `compliance.deletion.requested`,
//     `compliance.deletion.approved`,
//     `compliance.deletion.rejected`,
//     `compliance.deletion.executing`,
//     `compliance.deletion.executed`
//   - `compliance.deletion.item.execute_requested` (per item, for
//     the owning module: `{"deletion_id", "item_id", "module",
//     "subject", "requester_id"}`)
//   - `compliance.deletion.item.executed` (emitted by the owning
//     module to confirm it deleted its rows; this module listens
//     to it but never emits it itself, so confirming an item can
//     never re-enter its own listener)
//   - `compliance.consent.recorded`,
//     `compliance.consent.withdrawn`
//   - `compliance.retention.policy.created`,
//     `compliance.retention.review.created`,
//     `compliance.retention.review.completed`
//   - `compliance.processing_record.created`
//   - `compliance.checklist.created`,
//     `compliance.checklist.updated`
//   - `compliance.breach.reported`, `compliance.breach.updated`
//   - `notification.requested` (breach reported, in the
//     notifications module's payload shape; the caller supplies
//     `notify_user_id` when a specific recipient must be told)
//   - `scheduler.enqueue` with `{"kind":
//     "compliance.retention.review", "payload": {"policy_id",
//     "review_id", ...}}` when a retention review is requested.
// Events this module listens to:
//   - `compliance.deletion.item.executed` — confirms one deletion
//     item as executed; when every item of a request is executed,
//     the request moves to `executed`.
//   - `compliance.retention.review_requested` — the event the
//     scheduler emits when a `compliance.retention.review` job
//     fires; records the review run as completed with the flag
//     counts the job payload carries. Only reviews the scheduler
//     actually queued (`scheduler_status == "queued"`) are
//     completed; a declined or already-completed review is skipped.

//! Compliance Center module — GDPR / ISO support: data export — Module.
//!
//! Split from `src/modules/compliance.rs`; behaviour unchanged.

use super::breaches::*;
use super::checklists::*;
use super::consents::*;
use super::constants::*;
use super::deletion_listeners::*;
use super::deletions::*;
use super::exports::*;
use super::helpers::*;
use super::records::*;
use super::requests::*;
use super::retention::*;
use super::state::*;
use super::types::*;
use super::ai_capabilities::AI_CAPABILITIES;

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

pub struct ComplianceModule {
    pub(super) state: ComplianceState,
}

impl ComplianceModule {
    pub fn new() -> Self {
        Self {
            state: ComplianceState::default(),
        }
    }
}

impl Default for ComplianceModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ComplianceModule {
    fn id(&self) -> &'static str {
        "compliance"
    }

    fn name(&self) -> &'static str {
        "Compliance Center"
    }

    fn description(&self) -> &'static str {
        "GDPR / ISO compliance: data export requests with expiring storage links, deletion requests with a manager approval step and per-module audit items executed by the owning modules, an append-only consent log, retention policies enforced by a flag-only scheduler job, processing-activity records (RoPA), checklists with storage-backed evidence, and a breach log with an append-only timeline."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "scheduler", "storage"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "scheduler", "storage"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("compliance", AI_CAPABILITIES);
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
            DELETION_ITEM_EXECUTED,
            "compliance",
            Arc::new(move |ctx: &mut HookContext| {
                on_deletion_item_executed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            RETENTION_REVIEW_REQUESTED,
            "compliance",
            Arc::new(move |ctx: &mut HookContext| {
                on_retention_review_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/compliance")
                .app_data(state)
                .route("/exports", web::get().to(list_exports))
                .route("/exports", web::post().to(create_export))
                .route("/exports/{id}", web::get().to(get_export))
                .route("/exports/{id}/start", web::post().to(start_export))
                .route("/exports/{id}/complete", web::post().to(complete_export))
                .route("/exports/{id}/fail", web::post().to(fail_export))
                .route("/exports/{id}/download", web::get().to(download_export))
                .route("/deletions", web::get().to(list_deletions))
                .route("/deletions", web::post().to(create_deletion))
                .route("/deletions/{id}", web::get().to(get_deletion))
                .route("/deletions/{id}/approve", web::post().to(approve_deletion))
                .route("/deletions/{id}/reject", web::post().to(reject_deletion))
                .route("/deletions/{id}/execute", web::post().to(execute_deletion))
                .route("/deletions/{id}/items", web::get().to(list_deletion_items))
                .route(
                    "/deletions/{id}/items/{item_id}/mark-executed",
                    web::post().to(mark_item_executed),
                )
                .route("/consents", web::get().to(list_consents))
                .route("/consents", web::post().to(record_consent))
                .route("/consents/{id}", web::get().to(get_consent))
                .route("/retention-policies", web::get().to(list_policies))
                .route("/retention-policies", web::post().to(create_policy))
                .route("/retention-policies/{id}", web::get().to(get_policy))
                .route("/retention-policies/{id}", web::patch().to(update_policy))
                .route("/retention-policies/{id}", web::delete().to(delete_policy))
                .route(
                    "/retention-policies/{id}/review",
                    web::post().to(request_policy_review),
                )
                .route("/retention-reviews", web::get().to(list_retention_reviews))
                .route("/processing-records", web::get().to(list_processing_records))
                .route(
                    "/processing-records",
                    web::post().to(create_processing_record),
                )
                .route(
                    "/processing-records/{id}",
                    web::get().to(get_processing_record),
                )
                .route(
                    "/processing-records/{id}",
                    web::patch().to(update_processing_record),
                )
                .route(
                    "/processing-records/{id}",
                    web::delete().to(delete_processing_record),
                )
                .route("/checklists", web::get().to(list_checklists))
                .route("/checklists", web::post().to(create_checklist))
                .route("/checklists/{id}", web::get().to(get_checklist))
                .route("/checklists/{id}", web::patch().to(update_checklist))
                .route("/checklists/{id}", web::delete().to(delete_checklist))
                .route("/breaches", web::get().to(list_breaches))
                .route("/breaches", web::post().to(create_breach))
                .route("/breaches/{id}", web::get().to(get_breach))
                .route("/breaches/{id}", web::patch().to(update_breach))
                .route(
                    "/breaches/{id}/timeline",
                    web::post().to(add_breach_timeline),
                )
                .route(
                    "/breaches/{id}/timeline",
                    web::get().to(list_breach_timeline),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/compliance/exports", owner: "compliance", description: "List export requests (a requester sees only their own unless they hold the manage capability).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/compliance/exports", owner: "compliance", description: "Request a data export (one open request per requester).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/compliance/exports/{id}", owner: "compliance", description: "Get one export request (own only unless manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/compliance/exports/{id}/start", owner: "compliance", description: "Mark an export as processing (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/compliance/exports/{id}/complete", owner: "compliance", description: "Attach the storage link with expiry to a processing export (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/compliance/exports/{id}/fail", owner: "compliance", description: "Mark an export as failed with a reason (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/compliance/exports/{id}/download", owner: "compliance", description: "Return the storage link of a ready export while it is still valid (own only unless manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/compliance/deletions", owner: "compliance", description: "List deletion requests with their per-module items (own only unless manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/compliance/deletions", owner: "compliance", description: "Request a deletion with per-module items (one open request per requester).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/compliance/deletions/{id}", owner: "compliance", description: "Get one deletion request with its items (own only unless manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/compliance/deletions/{id}/approve", owner: "compliance", description: "Approve a deletion request (manager, never the requester themselves).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/compliance/deletions/{id}/reject", owner: "compliance", description: "Reject a deletion request (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/compliance/deletions/{id}/execute", owner: "compliance", description: "Dispatch execution to the owning modules (manager, only after approval).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/compliance/deletions/{id}/items", owner: "compliance", description: "List the per-module items of a deletion request.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/compliance/deletions/{id}/items/{item_id}/mark-executed", owner: "compliance", description: "Confirm one deletion item executed with an audit detail (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/compliance/consents", owner: "compliance", description: "List consent records (own history only unless manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/compliance/consents", owner: "compliance", description: "Record consent or a withdrawal (append-only; withdrawal is a new row).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/compliance/consents/{id}", owner: "compliance", description: "Get one consent record (own only unless manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/compliance/retention-policies", owner: "compliance", description: "List retention policies (paginated, filter by data_category).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/compliance/retention-policies", owner: "compliance", description: "Create a retention policy (one per data category, manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/compliance/retention-policies/{id}", owner: "compliance", description: "Get one retention policy.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/compliance/retention-policies/{id}", owner: "compliance", description: "Update a retention policy (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/compliance/retention-policies/{id}", owner: "compliance", description: "Delete a retention policy (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/compliance/retention-policies/{id}/review", owner: "compliance", description: "Enqueue a retention review job (flags items, never deletes; manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/compliance/retention-reviews", owner: "compliance", description: "List retention review runs and their flag counts.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/compliance/processing-records", owner: "compliance", description: "List processing-activity records (RoPA).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/compliance/processing-records", owner: "compliance", description: "Create a processing-activity record (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/compliance/processing-records/{id}", owner: "compliance", description: "Get one processing-activity record.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/compliance/processing-records/{id}", owner: "compliance", description: "Update a processing-activity record (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/compliance/processing-records/{id}", owner: "compliance", description: "Delete a processing-activity record (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/compliance/checklists", owner: "compliance", description: "List compliance checklists (paginated, filter by status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/compliance/checklists", owner: "compliance", description: "Create a checklist item with optional storage-backed evidence (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/compliance/checklists/{id}", owner: "compliance", description: "Get one checklist item.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/compliance/checklists/{id}", owner: "compliance", description: "Update a checklist item (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/compliance/checklists/{id}", owner: "compliance", description: "Delete a checklist item (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/compliance/breaches", owner: "compliance", description: "List breach log entries (paginated, filter by status and notification_status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/compliance/breaches", owner: "compliance", description: "Report a breach (manager; optionally notifies one user).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/compliance/breaches/{id}", owner: "compliance", description: "Get one breach with its timeline.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/compliance/breaches/{id}", owner: "compliance", description: "Update a breach status or notification status (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/compliance/breaches/{id}/timeline", owner: "compliance", description: "Append a timeline entry to a breach (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/compliance/breaches/{id}/timeline", owner: "compliance", description: "List the timeline entries of a breach.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ComplianceModule::new()));
}
