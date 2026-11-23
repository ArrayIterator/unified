//! Recruitment - Module definition: routes, hooks, and registration.

// Recruitment / ATS module -- job postings, applicants with a
// validated hiring pipeline, interviews, per-interviewer
// evaluations, offers, and the hire handoff to HR onboarding.
//
// Routes live under `/api/v1/recruitment`:
//
//   Jobs:
//     GET/POST /jobs -- list jobs / create a job.
//     GET/PATCH/DELETE /jobs/{id}.
//     POST /jobs/{id}/publish, /jobs/{id}/close.
//     GET  /jobs/{id}/applicants -- applicants of a job.
//     POST /jobs/{id}/applicants -- add an applicant to a job.
//   Applicants:
//     GET  /applicants/{id} -- one applicant, with the aggregated
//          evaluation score.
//     PATCH/DELETE /applicants/{id}.
//     POST /applicants/{id}/move -- validated stage transition
//          (audited in the stage history).
//     GET  /applicants/{id}/history -- stage history.
//   Interviews:
//     GET/POST /applicants/{id}/interviews.
//     PATCH /interviews/{id}.
//     POST /interviews/{id}/outcome -- record the outcome.
//   Evaluations:
//     GET/POST /applicants/{id}/evaluations -- per-interviewer
//          scorecards; the read side aggregates the score.
//   Offers:
//     GET/POST /applicants/{id}/offers.
//     GET  /offers/{id}.
//     PATCH /offers/{id}.
//     POST /offers/{id}/send, /accept, /decline, /expire.
//   Hiring:
//     POST /applicants/{id}/hire -- hire; emits the HR onboarding
//          handoff event (no HR record is created here).
//   Public (no auth; see the public-access note below):
//     GET  /public/jobs -- published jobs only.
//     GET  /public/jobs/{id} -- one published job.
//     POST /public/jobs/{id}/apply -- apply for a published job
//          (consent flag required in the body).
//
// HONESTY -- READ THIS FIRST. What is real in this module:
// in-memory job, applicant, stage-history, interview, evaluation,
// and offer management; the pipeline state machine
// (`applied` -> `screening` -> `interview` -> `offer` ->
// `hired`, with `rejected` reachable from any open stage and
// terminal afterwards) with every transition written to the
// stage history; per-interviewer scorecards aggregated on read
// (average score plus per-criterion averages -- no score is ever
// invented for an interviewer who did not submit one); offer
// lifecycle (`draft` -> `sent` -> `accepted` / `declined` /
// `expired`); interview scheduling through `scheduler.enqueue`
// with a `recruitment.interview.reminder_requested` listener
// that notifies the interviewers. What is NOT built here: HR
// records -- hiring emits `hr.onboarding.started` for the HR
// module and creates nothing on the HR side; resume bytes -- the
// resume lives in the storage module and only its target + key
// are stored here as metadata; actual email / push delivery
// (the notifications module owns delivery -- this module only
// emits `notification.requested`); and any check that a
// `department_id` names a real department -- the Departments
// module owns departments and this module stores the id as a
// plain reference, exactly like the events module stores
// `company_id`. Consent: an applicant with
// `consent_contact: false` cannot be contacted -- interview
// scheduling and offer sending both refuse with 409 until
// consent is recorded with a timestamp.
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`, and denied outright when a rule exists
// with `allowed: false`). There is no reusable per-request
// "is this endpoint public?" helper to call from a handler -- the
// check lives in the API Manager middleware / `resolve_public`
// path -- so the public routes below rely on that gate: an
// administrator must create the matching `ApiAccessRule` rows for
// `GET /api/v1/recruitment/public/*` and for
// `POST /api/v1/recruitment/public/jobs/{id}/apply`. The routes
// are registered in `route_info()` with
// `route_version_support()` defaulting to `All`, so the API
// Manager's per-version registry picks them up. Do NOT treat the
// absence of a capability check on a public route as an open
// door: without the rule the API Manager denies the call. Public
// routes expose published jobs only, and never expose applicant
// data, evaluations, or offers.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `recruitment.job.created`, `recruitment.job.updated`,
//     `recruitment.job.deleted`, `recruitment.job.published`,
//     `recruitment.job.closed`
//   - `recruitment.applicant.created`, `recruitment.applicant.updated`,
//     `recruitment.applicant.deleted`, `recruitment.applicant.moved`
//   - `recruitment.interview.scheduled`,
//     `recruitment.interview.updated`,
//     `recruitment.interview.outcome_recorded`
//   - `recruitment.evaluation.submitted`
//   - `recruitment.offer.created`, `recruitment.offer.updated`,
//     `recruitment.offer.sent`, `recruitment.offer.accepted`,
//     `recruitment.offer.declined`, `recruitment.offer.expired`
//   - `recruitment.hired`
//   - `hr.onboarding.started` (hire handoff for the HR module:
//     `{"applicant_id", "job_id", "user_id", "name", "email",
//     "start_date", "salary", "currency", "department_id"}` --
//     the HR module owns the onboarding record)
//   - `notification.requested` (interview reminders to the
//     interviewers) with the notifications module's payload
//     shape: `{"kind", "user_id", "title", "body", "module":
//     "recruitment", "link", "priority", "channels"}`.
//   - `scheduler.enqueue` with `{"kind":
//     "recruitment.interview.reminder", "payload":
//     {"interview_id", "applicant_id", ...}}` when an interview is
//     scheduled.
// Events this module listens to:
//   - `recruitment.interview.reminder_requested` -- the event the
//     scheduler emits when a `recruitment.interview.reminder` job
//     fires, following its naming rule (`backup.run` ->
//     `backup.run_requested`); notifies the interviewers through
//     `notification.requested`.
//
// Scheduler note: `recruitment.interview.reminder` is NOT in the
// scheduler's `KNOWN_KINDS` list yet, so today the scheduler
// answers `{"queued": false, "error": "Unknown job kind: ..."}` and
// the interview row records `scheduler_status: "declined"` instead
// of pretending it was scheduled. The interview is still stored
// and stays usable; the follow-up is to add
// `"recruitment.interview.reminder"` to `KNOWN_KINDS` in
// `scheduler.rs` and arm the dispatch so the job emits
// `recruitment.interview.reminder_requested` with the interview
// payload (see the integrator snippet in the module report).

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
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
use super::{constants::*, types::*, state::*, helpers::*, handlers_jobs::*, handlers_applicants::*, handlers_interviews::*, handlers_evaluations::*, handlers_offers::*};
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

/// The Recruitment / ATS module.
pub struct RecruitmentModule {
    pub(crate) state: RecruitmentState,
}

impl RecruitmentModule {
    pub fn new() -> Self {
        Self {
            state: RecruitmentState::default(),
        }
    }
}

impl Default for RecruitmentModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for RecruitmentModule {
    fn id(&self) -> &'static str {
        "recruitment"
    }

    fn name(&self) -> &'static str {
        "Recruitment / ATS"
    }

    fn description(&self) -> &'static str {
        "Job postings with a public listing, applicants with a validated hiring pipeline, interviews, per-interviewer evaluations, offers, and the hire handoff to HR onboarding."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "departments", "notifications", "scheduler", "storage"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec![
            "users",
            "roles",
            "departments",
            "notifications",
            "scheduler",
            "storage",
        ]
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

        ai::register_ai_capabilities("recruitment", AI_CAPABILITIES);
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
            INTERVIEW_REMINDER_REQUESTED,
            "recruitment",
            Arc::new(move |ctx: &mut HookContext| {
                on_interview_reminder_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/recruitment")
                .app_data(state)
                .route("/public/jobs", web::get().to(list_public_jobs))
                .route("/public/jobs/{id}", web::get().to(get_public_job))
                .route("/public/jobs/{id}/apply", web::post().to(apply_public))
                .route("/jobs", web::get().to(list_jobs))
                .route("/jobs", web::post().to(create_job))
                .route("/jobs/{id}", web::get().to(get_job))
                .route("/jobs/{id}", web::patch().to(update_job))
                .route("/jobs/{id}", web::delete().to(delete_job))
                .route("/jobs/{id}/publish", web::post().to(publish_job))
                .route("/jobs/{id}/close", web::post().to(close_job))
                .route("/jobs/{id}/applicants", web::get().to(list_job_applicants))
                .route("/jobs/{id}/applicants", web::post().to(create_applicant))
                .route("/applicants/{id}", web::get().to(get_applicant))
                .route("/applicants/{id}", web::patch().to(update_applicant))
                .route("/applicants/{id}", web::delete().to(delete_applicant))
                .route("/applicants/{id}/move", web::post().to(move_applicant))
                .route("/applicants/{id}/history", web::get().to(list_stage_history))
                .route("/applicants/{id}/interviews", web::get().to(list_interviews))
                .route("/applicants/{id}/interviews", web::post().to(create_interview))
                .route("/applicants/{id}/evaluations", web::get().to(list_evaluations))
                .route(
                    "/applicants/{id}/evaluations",
                    web::post().to(create_evaluation),
                )
                .route("/applicants/{id}/offers", web::get().to(list_offers))
                .route("/applicants/{id}/offers", web::post().to(create_offer))
                .route("/applicants/{id}/hire", web::post().to(hire_applicant))
                .route("/interviews/{id}", web::patch().to(update_interview))
                .route("/interviews/{id}/outcome", web::post().to(record_outcome))
                .route("/offers/{id}", web::get().to(get_offer))
                .route("/offers/{id}", web::patch().to(update_offer))
                .route("/offers/{id}/send", web::post().to(send_offer))
                .route("/offers/{id}/accept", web::post().to(accept_offer))
                .route("/offers/{id}/decline", web::post().to(decline_offer))
                .route("/offers/{id}/expire", web::post().to(expire_offer)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/recruitment/jobs", owner: "recruitment", description: "List jobs (paginated, filter by status, type, department_id, location).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/recruitment/jobs", owner: "recruitment", description: "Create a job posting (draft).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/recruitment/jobs/{id}", owner: "recruitment", description: "Get one job.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/recruitment/jobs/{id}", owner: "recruitment", description: "Update a job (draft or published only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/recruitment/jobs/{id}", owner: "recruitment", description: "Delete a draft job with no applicants.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/recruitment/jobs/{id}/publish", owner: "recruitment", description: "Publish a draft job (emits recruitment.job.published).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/recruitment/jobs/{id}/close", owner: "recruitment", description: "Close a published job (emits recruitment.job.closed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/recruitment/jobs/{id}/applicants", owner: "recruitment", description: "List applicants of a job (paginated, filter by stage, source).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/recruitment/jobs/{id}/applicants", owner: "recruitment", description: "Add an applicant to a job (consent flag recorded with a timestamp).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/recruitment/applicants/{id}", owner: "recruitment", description: "Get one applicant with the aggregated evaluation score.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/recruitment/applicants/{id}", owner: "recruitment", description: "Update an applicant (consent changes re-stamp the consent time).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/recruitment/applicants/{id}", owner: "recruitment", description: "Delete an applicant (not after hiring).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/recruitment/applicants/{id}/move", owner: "recruitment", description: "Move an applicant to the next pipeline stage (validated, audited).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/recruitment/applicants/{id}/history", owner: "recruitment", description: "Stage history of an applicant (append-only, paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/recruitment/applicants/{id}/interviews", owner: "recruitment", description: "List interviews of an applicant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/recruitment/applicants/{id}/interviews", owner: "recruitment", description: "Schedule an interview (consent required; reminder via scheduler.enqueue).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/recruitment/interviews/{id}", owner: "recruitment", description: "Update an interview (scheduled interviews only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/recruitment/interviews/{id}/outcome", owner: "recruitment", description: "Record an interview outcome (passed/failed/no_show).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/recruitment/applicants/{id}/evaluations", owner: "recruitment", description: "List evaluations of an applicant with the aggregated score.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/recruitment/applicants/{id}/evaluations", owner: "recruitment", description: "Submit a per-interviewer scorecard (one per interviewer per interview).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/recruitment/applicants/{id}/offers", owner: "recruitment", description: "List offers of an applicant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/recruitment/applicants/{id}/offers", owner: "recruitment", description: "Create an offer for an applicant (draft).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/recruitment/offers/{id}", owner: "recruitment", description: "Get one offer.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/recruitment/offers/{id}", owner: "recruitment", description: "Update a draft offer.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/recruitment/offers/{id}/send", owner: "recruitment", description: "Send an offer (consent required).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/recruitment/offers/{id}/accept", owner: "recruitment", description: "Accept a sent offer (emits recruitment.offer.accepted).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/recruitment/offers/{id}/decline", owner: "recruitment", description: "Decline a sent offer.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/recruitment/offers/{id}/expire", owner: "recruitment", description: "Expire a sent offer.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/recruitment/applicants/{id}/hire", owner: "recruitment", description: "Hire an applicant (accepted offer required; emits recruitment.hired and hr.onboarding.started).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/recruitment/public/jobs", owner: "recruitment", description: "PUBLIC: list published jobs (no auth; gated by the API Manager ApiAccessRule for this path).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/recruitment/public/jobs/{id}", owner: "recruitment", description: "PUBLIC: one published job (no auth; gated by the API Manager ApiAccessRule).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/recruitment/public/jobs/{id}/apply", owner: "recruitment", description: "PUBLIC: apply for a published job (no auth; gated by the API Manager ApiAccessRule; consent required).", auth: unified_core::auth::RouteAuth::Public},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(RecruitmentModule::new()));
}
