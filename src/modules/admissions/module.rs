// Admissions module — new-student registration (batch 13, builder B3).
//
// Admission periods per academic year and term, online registration
// through the Forms module, selection stages with weighted scoring,
// result announcement, registration fees through Billing, a document
// checklist through the Documents module, an ordered waitlist, and
// enrolment handoff to the academic system. Interconnection happens
// only through the hook bus — no module calls another module directly.
//
// Identity rule: an application stores the applicant user id ONLY.
// Names, emails, and all other profile data live in the Users module
// and are resolved by callers through `GET /api/v1/users/{id}`.
// Programs and levels are ids supplied by callers and owned by the
// academic system: this module stores them, matches quotas against
// them, and never duplicates program records. Document files live in
// the Documents module: this module stores checklist metadata and
// document ids only, never file bytes. Money (registration fees) is
// NUMERIC(19,4) in the migration and `f64` rounded to four decimals
// in memory, the same convention as the HR payroll amounts.
//
// Role-based access (enforced server-side on every handler):
//   - `admissions:read`      — read admission records (applicants see
//                              only their own applications and results).
//   - `admissions:read_all`  — read every application and result.
//   - `admissions:manage`    — create/update/delete periods, quotas,
//                              stages, required documents, decisions,
//                              checklist verification, fee waivers.
//   - `admissions:apply`     — file and edit your own application.
//   - `admissions:score`     — record and correct stage scores.
// Capabilities are checked through the `role.check` hook and fail
// closed: no answer means not allowed. No `admissions:*` capability
// is granted by the default roles; an administrator must grant them.
//
// Status vocabularies are DATA, not code: the allowed values for
// period, application, checklist, waitlist, and result-decision
// statuses live in the settings table (keys `status.period`,
// `status.application`, `status.checklist`, `status.waitlist`,
// `decision.result`) seeded with the defaults listed below. An
// administrator edits them through `PUT /api/v1/admissions/settings`.
// The code only carries the seed defaults and the transition rules
// between them. Other configurable defaults: `defaults.fee_currency`
// (3-letter code, default `USD`), `defaults.score_scale_max`
// (default `100`), `defaults.waitlist_order`
// (`score_desc_submitted_asc` or `submitted_asc`, default the
// former), `defaults.result_visible_to_applicant` (`true`/`false`).
// Weights are per-stage data on the stage definition; quotas are
// per-program/per-level data on the period.
//
// Events emitted:
//   - `admissions.period_created`      {period_id, academic_year, term}
//   - `admissions.period_opened`       {period_id}
//   - `admissions.period_closed`       {period_id}
//   - `admissions.application_created` {application_id, period_id, applicant_user_id, source}
//   - `admissions.application_received` {application_id, period_id, applicant_user_id, form_submission_id}
//   - `admissions.application_submitted` {application_id, period_id}
//   - `admissions.application_withdrawn` {application_id, period_id}
//   - `admissions.score_recorded`      {application_id, period_id, stage_id, score, total_score}
//   - `admissions.decision_made`       {application_id, period_id, decision}
//   - `admissions.results_published`   {period_id, result_count}
//   - `admissions.results_unpublished` {period_id}
//   - `admissions.fee_invoice_requested` {application_id, period_id, amount, currency, reference}
//   - `admissions.fee_paid`            {application_id, period_id, invoice_id}
//   - `admissions.waitlist_promoted`   {application_id, period_id, waitlist_entry_id}
//   - `admissions.document_attached`   {application_id, checklist_item_id, document_id}
//   - `admissions.document_verified`   {application_id, checklist_item_id}
//   - `admissions.enrollment_requested` (see `academic.student.create_requested` below)
//   - `academic.student.create_requested` {application_id, period_id, applicant_user_id, full_name, email, phone, program_id, level, academic_year, term, enrollment_year}
//   - `billing.invoice.requested`      {application_id, period_id, customer_user_id, customer_name, customer_email, amount, currency, description, reference}
//   - `notification.requested`         result publication, waitlist
//                                      promotion, and fee requests
//                                      (the Notifications module listens).
// Events consumed:
//   - `forms.mapping.requested`        target `admissions_application`:
//                                      creates an application from a
//                                      mapped form submission (see the
//                                      contract in `on_forms_mapping_requested`).
//   - `forms.submission.received`      {form_id, submission_id, slug}:
//                                      records the submission id on the
//                                      matching draft application when
//                                      the form is linked to a period.
//   - `document.uploaded`              full document record: when
//                                      `module_id` is `admissions` and
//                                      the slug of the document file
//                                      name's stem equals a pending
//                                      checklist item's `document_key`
//                                      for an application owned by
//                                      `owner_id`, the item is marked
//                                      `submitted`. When the key
//                                      matches items of several
//                                      applications the upload is
//                                      ambiguous and attaches to
//                                      nothing.
//   - `document.deleted`               {id, ...}: clears the link on
//                                      any checklist item pointing at
//                                      the deleted document.
//   - `invoice.paid`                   {invoice, method}: when the
//                                      paid invoice carries
//                                      `order_id` equal to an
//                                      application id, the fee is
//                                      marked paid.
//   - `academic.student.created`       {application_id, student_id}:
//                                      links the created student record
//                                      to the enrolled application.
//
// CONVERGENCE (for the integrator):
//   1. `forms.submission.received` carries no field values (only
//      `form_id`, `submission_id`, `slug`), so the mapping path
//      (`forms.mapping.requested` with target
//      `admissions_application`) is the real registration path: the
//      admin configures the registration form's mapping with that
//      target and a field map. The submission handler only links the
//      submission id to a draft application created by the mapping.
//   2. `billing.invoice.requested` has no listener in the billing
//      module on disk today (the same open contract the appointments
//      module already uses). Billing must set the created invoice's
//      `order_id` to the application id when fulfilling the request,
//      so the `invoice.paid` handler here can match the payment.
//      Until then, fee payment is confirmed manually by recording
//      the invoice id through the fee endpoint with action `mark_paid`.
//   3. The academic module subscribes to
//      `academic.student.create_requested` and answers with
//      `academic.student.created` carrying the same
//      `application_id` (integration round, convergence W6); the
//      handler here links the returned `student_id` onto the
//      application. This module never creates student records
//      itself and never duplicates them. Note the enrol endpoint
//      marks the application `enrolled` before emitting, so the
//      `status == "accepted"` branch in
//      `on_academic_student_created` is a no-op on the normal path
//      — the student id link is what matters there.
//   4. The documents module has no synchronous lookup hook, so a
//      document id supplied to the attach endpoint cannot be verified
//      against the documents store here. Attach records the link and
//      marks the item `submitted`; the `document.uploaded` handler
//      auto-links uploads that follow the naming convention
//      (`module_id` `admissions`, the slug of the document file
//      name's stem equal to the checklist item's `document_key`,
//      matching within one application only). The integrator may add
//      a documents lookup hook later.
//   5. Program and level ids are caller-supplied references owned by
//      the academic system. Quota matching is exact string equality
//      on (`program_id`, `level`); an application whose pair has no
//      quota row is treated as quota-unlimited only when the period
//      has no quota rows at all — otherwise acceptance is refused
//      with a 409 until a quota row exists for the pair.
//   6. `route_version_support` returns `All`, so the API Manager's
//      per-version registry picks every route up. No route here is
//      public: online registration goes through the forms module's
//      public submit routes, gated by the API Manager's
//      `ApiAccessRule` records.
//
// Public-access note: there is no public route in this module. The
// applicant-facing flow is the forms module's public render/submit
// (`GET/POST /api/v1/forms/public/{slug}`), gated by the API Manager.
// Applicants read their own application and result only after
// authenticating with `X-User-Id` / `X-Role` like every other module.

// admissions module — Module implementation.
//
// Split from the former single-file module `src/modules/admissions.rs`.
// The module root (`admissions.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::state::AdmissionsModule;
use super::hooks::{on_academic_student_created, on_document_deleted, on_document_uploaded, on_forms_mapping_requested, on_forms_submission_received, on_invoice_paid};
use super::handlers_periods::{close_period, create_period, delete_period, get_period, list_periods, open_period, update_period};
use super::handlers_quotas::{create_quota, delete_quota, list_quotas, update_quota};
use super::handlers_stages::{create_stage, delete_stage, list_stages, update_stage};
use super::handlers_documents::{create_required_document, delete_required_document, list_required_documents, update_required_document};
use super::handlers_summary::period_summary;
use super::handlers_applications::{create_application, delete_application, get_application, list_applications, submit_application, update_application, withdraw_application};
use super::handlers_scores::{create_score, delete_score, get_score_total, list_scores, update_score};
use super::handlers_checklist::{attach_checklist_document, get_checklist, reject_checklist_item, verify_checklist_item};
use super::handlers_fees::{decide_application, enroll_application, get_application_result, request_fee_invoice};
use super::handlers_waitlist::{decline_waitlist_entry, list_waitlist, promote_waitlist};
use super::handlers_results::{list_results, publish_results, unpublish_results};
use super::handlers_settings::{list_settings, update_setting};
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module implementation
// ---------------------------------------------------------------------------

impl Module for AdmissionsModule {
    fn id(&self) -> &'static str {
        "admissions"
    }

    fn name(&self) -> &'static str {
        "Admissions"
    }

    fn description(&self) -> &'static str {
        "New-student registration: admission periods with per-program quotas, online registration through the Forms module, admin-defined selection stages with weighted scoring, result announcement with per-applicant visibility, registration fees through Billing, a document checklist through the Documents module, an ordered waitlist with promotion, and enrolment handoff to the academic system."
    }

    /// Admissions is sold as a product: a customer buys a module
    /// entitlement for it (see `crate::modules::licensing`).
    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn requires(&self) -> &[&'static str] {
        &["forms", "documents", "billing", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["forms", "documents", "billing", "notifications"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("admissions", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Arc::clone(&ctx.hooks);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "forms.mapping.requested",
            "admissions",
            Arc::new(move |hook_ctx: &mut HookContext| {
                on_forms_mapping_requested(state.clone(), hook_ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "forms.submission.received",
            "admissions",
            Arc::new(move |hook_ctx: &mut HookContext| {
                on_forms_submission_received(state.clone(), hook_ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "document.uploaded",
            "admissions",
            Arc::new(move |hook_ctx: &mut HookContext| {
                on_document_uploaded(state.clone(), hook_ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "document.deleted",
            "admissions",
            Arc::new(move |hook_ctx: &mut HookContext| {
                on_document_deleted(state.clone(), hook_ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "invoice.paid",
            "admissions",
            Arc::new(move |hook_ctx: &mut HookContext| {
                on_invoice_paid(state.clone(), hook_ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "academic.student.created",
            "admissions",
            Arc::new(move |hook_ctx: &mut HookContext| {
                on_academic_student_created(state.clone(), hook_ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/admissions")
                // Literal segments first: they must win over `/{id}`.
                .route("/settings", web::get().to(list_settings))
                .route("/settings", web::put().to(update_setting))
                .route("/periods", web::get().to(list_periods))
                .route("/periods", web::post().to(create_period))
                .route("/periods/{id}", web::get().to(get_period))
                .route("/periods/{id}", web::patch().to(update_period))
                .route("/periods/{id}", web::delete().to(delete_period))
                .route("/periods/{id}/open", web::post().to(open_period))
                .route("/periods/{id}/close", web::post().to(close_period))
                .route("/periods/{id}/summary", web::get().to(period_summary))
                .route("/periods/{id}/quotas", web::get().to(list_quotas))
                .route("/periods/{id}/quotas", web::post().to(create_quota))
                .route(
                    "/periods/{id}/quotas/{quota_id}",
                    web::patch().to(update_quota),
                )
                .route(
                    "/periods/{id}/quotas/{quota_id}",
                    web::delete().to(delete_quota),
                )
                .route("/periods/{id}/stages", web::get().to(list_stages))
                .route("/periods/{id}/stages", web::post().to(create_stage))
                .route(
                    "/periods/{id}/stages/{stage_id}",
                    web::patch().to(update_stage),
                )
                .route(
                    "/periods/{id}/stages/{stage_id}",
                    web::delete().to(delete_stage),
                )
                .route(
                    "/periods/{id}/required-documents",
                    web::get().to(list_required_documents),
                )
                .route(
                    "/periods/{id}/required-documents",
                    web::post().to(create_required_document),
                )
                .route(
                    "/periods/{id}/required-documents/{document_id}",
                    web::patch().to(update_required_document),
                )
                .route(
                    "/periods/{id}/required-documents/{document_id}",
                    web::delete().to(delete_required_document),
                )
                .route("/periods/{id}/waitlist", web::get().to(list_waitlist))
                .route(
                    "/periods/{id}/waitlist/promote",
                    web::post().to(promote_waitlist),
                )
                .route("/periods/{id}/results", web::get().to(list_results))
                .route(
                    "/periods/{id}/results/publish",
                    web::post().to(publish_results),
                )
                .route(
                    "/periods/{id}/results/unpublish",
                    web::post().to(unpublish_results),
                )
                .route("/applications", web::get().to(list_applications))
                .route("/applications", web::post().to(create_application))
                .route("/applications/{id}", web::get().to(get_application))
                .route("/applications/{id}", web::patch().to(update_application))
                .route("/applications/{id}", web::delete().to(delete_application))
                .route(
                    "/applications/{id}/submit",
                    web::post().to(submit_application),
                )
                .route(
                    "/applications/{id}/withdraw",
                    web::post().to(withdraw_application),
                )
                .route("/applications/{id}/scores", web::get().to(list_scores))
                .route("/applications/{id}/scores", web::post().to(create_score))
                .route(
                    "/applications/{id}/scores/total",
                    web::get().to(get_score_total),
                )
                .route(
                    "/applications/{id}/scores/{score_id}",
                    web::patch().to(update_score),
                )
                .route(
                    "/applications/{id}/scores/{score_id}",
                    web::delete().to(delete_score),
                )
                .route(
                    "/applications/{id}/checklist",
                    web::get().to(get_checklist),
                )
                .route(
                    "/applications/{id}/checklist/{item_id}/attach",
                    web::post().to(attach_checklist_document),
                )
                .route(
                    "/applications/{id}/checklist/{item_id}/verify",
                    web::post().to(verify_checklist_item),
                )
                .route(
                    "/applications/{id}/checklist/{item_id}/reject",
                    web::post().to(reject_checklist_item),
                )
                .route(
                    "/applications/{id}/request-fee-invoice",
                    web::post().to(request_fee_invoice),
                )
                .route(
                    "/applications/{id}/result",
                    web::get().to(get_application_result),
                )
                .route(
                    "/applications/{id}/decide",
                    web::post().to(decide_application),
                )
                .route(
                    "/applications/{id}/enroll",
                    web::post().to(enroll_application),
                )
                .route(
                    "/waitlist/{entry_id}/decline",
                    web::post().to(decline_waitlist_entry),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/admissions/settings", owner: "admissions", description: "List admin-editable admissions settings (vocabularies and defaults).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/admissions/settings", owner: "admissions", description: "Update one admissions setting (admissions:manage; vocabularies validated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/admissions/periods", owner: "admissions", description: "List admission periods (paginated, filter by status, academic_year, term).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods", owner: "admissions", description: "Create an admission period (draft; admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/admissions/periods/{id}", owner: "admissions", description: "Get one admission period.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/admissions/periods/{id}", owner: "admissions", description: "Update a draft or closed period (archive only from closed; admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/admissions/periods/{id}", owner: "admissions", description: "Delete a draft period with no applications (admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods/{id}/open", owner: "admissions", description: "Open a draft period for submissions (admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods/{id}/close", owner: "admissions", description: "Close an open period (admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/admissions/periods/{id}/summary", owner: "admissions", description: "Period summary: counts by status, quota usage, waitlist, results, checklist completion.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/admissions/periods/{id}/quotas", owner: "admissions", description: "List per-program, per-level seat quotas of a period (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods/{id}/quotas", owner: "admissions", description: "Create a seat quota for one program and level (admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/admissions/periods/{id}/quotas/{quota_id}", owner: "admissions", description: "Update a quota's seats (never below already-accepted; admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/admissions/periods/{id}/quotas/{quota_id}", owner: "admissions", description: "Delete a quota with no accepted applications (admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/admissions/periods/{id}/stages", owner: "admissions", description: "List selection stage definitions of a period (paginated, definition order).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods/{id}/stages", owner: "admissions", description: "Create a selection stage (key, weight, pass score, required; admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/admissions/periods/{id}/stages/{stage_id}", owner: "admissions", description: "Update a selection stage; totals are recomputed (admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/admissions/periods/{id}/stages/{stage_id}", owner: "admissions", description: "Delete a stage with no recorded scores (admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/admissions/periods/{id}/required-documents", owner: "admissions", description: "List required documents of a period (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods/{id}/required-documents", owner: "admissions", description: "Define a required document for a period (admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/admissions/periods/{id}/required-documents/{document_id}", owner: "admissions", description: "Update a required document (admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/admissions/periods/{id}/required-documents/{document_id}", owner: "admissions", description: "Delete a required document with no submitted or verified checklist items (admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/admissions/periods/{id}/waitlist", owner: "admissions", description: "List the waiting waitlist of a period in promotion order (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods/{id}/waitlist/promote", owner: "admissions", description: "Promote the top (or a named) waiting entry to accepted when a seat is free (admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/admissions/periods/{id}/results", owner: "admissions", description: "List results of a period (applicants see only published ones).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods/{id}/results/publish", owner: "admissions", description: "Publish all results of a closed period and notify applicants (admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods/{id}/results/unpublish", owner: "admissions", description: "Unpublish all results of a period (admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/admissions/applications", owner: "admissions", description: "List applications (applicants see only their own; paginated, filter by period_id, status, program_id, level).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications", owner: "admissions", description: "Create an application (self with admissions:apply, or for another user with admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/admissions/applications/{id}", owner: "admissions", description: "Get one application (owner or admissions:read_all).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/admissions/applications/{id}", owner: "admissions", description: "Update a draft application (owner or admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/admissions/applications/{id}", owner: "admissions", description: "Delete a draft, withdrawn, or rejected application (admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/submit", owner: "admissions", description: "Submit a draft application while the period is open (owner or admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/withdraw", owner: "admissions", description: "Withdraw an application; a freed seat auto-promotes the top waitlisted applicant (owner or admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/admissions/applications/{id}/scores", owner: "admissions", description: "List stage scores of an application (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/scores", owner: "admissions", description: "Record a stage score; the weighted total is recomputed (admissions:score).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/admissions/applications/{id}/scores/total", owner: "admissions", description: "Weighted total score with per-stage breakdown and required-stage pass state.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/admissions/applications/{id}/scores/{score_id}", owner: "admissions", description: "Correct a stage score; the weighted total is recomputed (admissions:score).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/admissions/applications/{id}/scores/{score_id}", owner: "admissions", description: "Delete a stage score; the weighted total is recomputed (admissions:score).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/admissions/applications/{id}/checklist", owner: "admissions", description: "List the document checklist of an application (paginated, filter by status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/checklist/{item_id}/attach", owner: "admissions", description: "Attach a documents-module document to a checklist item (owner or admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/checklist/{item_id}/verify", owner: "admissions", description: "Verify a submitted checklist item (admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/checklist/{item_id}/reject", owner: "admissions", description: "Reject a submitted checklist item with a reason (admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/request-fee-invoice", owner: "admissions", description: "Request, waive, or manually mark paid the registration fee invoice (admissions:manage; emits billing.invoice.requested).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/admissions/applications/{id}/result", owner: "admissions", description: "Get one application's result (owner only once published; staff always).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/decide", owner: "admissions", description: "Decide an application (accepted requires passing stages and a free seat; waitlisted joins the waitlist; admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/enroll", owner: "admissions", description: "Enrol an accepted application (fee paid or waived, documents verified; emits academic.student.create_requested; admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/admissions/waitlist/{entry_id}/decline", owner: "admissions", description: "Decline a waitlist entry (owner or admissions:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AdmissionsModule::new()));
}
