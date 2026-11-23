//! exams - module.

// Online Exams module - question banks, timed exams, server-side
// attempts, auto and manual grading with rubrics, results, and
// analytics.
//
// Routes live under `/api/v1/exams`:
//
//   Question banks:
//     GET/POST /question-banks - list (paginated, filter by category,
//          status, owner_id) / create.
//     GET/PATCH/DELETE /question-banks/{id}.
//     GET/POST /question-banks/{id}/questions - list / add a question.
//     POST /question-banks/{id}/questions/import - import questions
//          from JSON (creates draft questions only).
//     GET  /question-banks/{id}/questions/export - export questions
//          as JSON (correct answers included only for managers).
//   Questions:
//     GET/PATCH/DELETE /questions/{id}.
//     GET  /questions/{id}/versions - version history of a question.
//   Exams:
//     GET/POST /exams - list (paginated, filter by status, category,
//          bank_id, course_id) / create.
//     GET/PATCH/DELETE /exams/{id}.
//     POST /exams/{id}/publish - draft -> published.
//     POST /exams/{id}/archive - published/draft -> archived.
//     POST /exams/{id}/close - published -> closed.
//     POST /exams/{id}/duplicate - copy an exam as a new draft.
//     GET/POST /exams/{id}/questions - the exam's question set /
//          attach a question (by bank question id, pinned version).
//     DELETE /exams/{id}/questions/{question_id} - detach one.
//   Attempts (taking an exam):
//     POST /exams/{id}/attempts - start an attempt (server clock
//          decides the deadline; randomised order is seeded per
//          attempt and the seed is recorded).
//     GET  /attempts - list attempts (own only unless manager or
//          grader; filter by exam_id, user_id, status).
//     GET  /attempts/{id} - one attempt (answers shown without
//          correct answers; correct answers are never returned here).
//     GET  /my/attempts - the caller's attempts.
//     POST /attempts/{id}/heartbeat - server remaining time; an
//          expired attempt is marked expired by the server.
//     POST /attempts/{id}/autosave - persist draft answers.
//     POST /attempts/{id}/submit - submit for grading.
//     POST /attempts/{id}/expire - mark an in-progress attempt
//          expired (manager; the server also expires on heartbeat).
//     POST /attempts/{id}/proctor-signal - record a proctoring
//          signal (tab switch, window blur, navigation away).
//     GET  /attempts/{id}/proctor-signals - signals of an attempt.
//   Rubrics and manual grading:
//     GET/POST /exams/{id}/rubrics - list / create a rubric.
//     GET/PATCH/DELETE /rubrics/{id}.
//     GET  /grading-queue - attempts waiting for manual grading
//          (grader or manager; filter by exam_id).
//     GET  /attempts/{id}/grading - the manual-grading view of one
//          attempt (essay answers plus rubric criteria).
//     POST /attempts/{id}/grade - submit manual grades for the
//          essay answers of an attempt.
//     POST /attempts/{id}/regrade - reopen a graded attempt for
//          grading again (manager; audited).
//     POST /attempts/{id}/override - override the final score
//          (manager; audited, original score kept).
//     POST /grades/{id}/dispute - flag a manual grade as disputed.
//     POST /grades/{id}/second-grade - second grader's grade for a
//          disputed grade (replaces the disputed grade; audited).
//   Results and analytics:
//     GET  /exams/{id}/results - results of an exam (manager or
//          grader; filter by passed, status).
//     GET  /results/{id} - one result (own only unless manager or
//          grader).
//     GET  /my/results - the caller's results.
//     POST /exams/{id}/results/publish - publish draft results.
//     POST /results/{id}/certificate - request a certificate for a
//          passed, published result (events only).
//     GET  /exams/{id}/analytics - pass rate, score distribution,
//          per-question statistics.
//     GET  /exams/{id}/question-stats - per-question statistics
//          (attempted %, correct %, discrimination index).
//   AI (smart module):
//     POST /ai/draft-questions - draft questions with AI help
//          (manager; drafts only, never auto-published).
//     POST /ai/analyse-results - analyse an exam's results with AI
//          help (manager or grader; anonymised statistics only).
//   Settings:
//     GET/PUT /settings - the configurable defaults (manager).
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory question-bank / question / exam / attempt / grade /
// rubric / proctor-signal / result management; question versioning
// (editing a question that already has attempts creates a new
// version and old attempts keep their pinned version); timed exams
// with scheduled open/close windows, seeded randomised question
// and option order, per-question marks, pass marks, and attempt
// limits; server-side attempt timers (start, heartbeat, autosave,
// submit, resume after disconnect - the server clock decides
// expiry, never the client clock); auto-grading for multiple-choice,
// true/false, and short-answer questions (exact match plus an
// optional case-insensitive match); a manual-grading queue for
// essays with rubrics, grader assignment, dispute plus second
// grader, regrade, and audited score overrides; result publication
// (draft -> published); per-question statistics including a
// discrimination index, score distribution, and per-exam pass
// rates; certificate requests and academic grade flow as events
// only. What is NOT built here: durable storage (the DashMap
// stores are authoritative; migration `2026-11-22-000090_exams`
// is the persistence target); camera or video proctoring - tab
// switch / window blur / navigation away are recorded only as
// proctoring SIGNALS in the attempt log, and this module never
// claims camera proctoring exists; payment capture (exams never
// charge directly - the store/billing modules own it); email or
// push delivery (the notifications module owns delivery - this
// module only emits `notification.requested`); certificate
// issuance itself (the e-learning certificate flow owns it -
// this module only emits `exams.certificate.requested`); and
// academic records (grades live in the academic module - this
// module only emits `academic.grade.recorded` when an exam is
// linked to an academic enrollment and component).
//
// AI RULE - READ THIS FIRST. AI-drafted questions are stored as
// DRAFT records only. They are never auto-published: a human must
// review and publish every AI-drafted question. AI analysis of
// results uses anonymised per-question statistics only - raw
// student answers and student identities are never sent to the AI
// helper.
//
// CONFIG-DRIVEN - READ THIS FIRST. Nothing behavioural is
// hard-coded: the allowed question types, difficulty levels, and
// question categories, the default pass mark, the default exam
// duration, the default attempts allowed, the default result
// visibility, the score-distribution bucket count, and the
// proctoring signal types all live in `ExamsSettings`, editable
// by an administrator through `PUT /settings`. The values in
// `ExamsSettings::default` are CONFIGURABLE DEFAULTS only - an
// admin change applies to every validation and default that reads
// them, and no code path consults a baked-in list instead. The
// standard constants that remain hard-coded are the status
// vocabularies (exam / attempt / grade / result statuses), the
// per-question mark bounds, and the field length limits - all
// standard constants, listed in the hard-coded disclosure at the
// end of this file's report.
//
// TENANT SCOPING - READ THIS FIRST (integration round, 2026-11-22).
// Every tenant-data handler in this module resolves the tenant
// FIRST via `tenancy::TenantContext::from_http_request` (see
// `require_tenant` below): no resolved tenant context means the
// request is rejected (401 missing context / 403 invalid tenant
// id), before caller identity, capability checks, store access,
// or any emit. No handler falls back to `public` and no tenant
// id, schema name, or package id is hard-coded - all are resolved
// at request time from the tenant context, per
// `unified_core::tenancy::TENANT_SCOPE_CONTRACT`. This module has
// no entitlement check of its own yet: the entitlements module
// answers `entitlements.check_requested`, but no grant records
// exist for this module and handlers do not reliably carry a
// company id, so a blocking entitlement gate would reject all
// traffic today; wiring it (grant per tenant + company id from
// the tenant context) is OPEN for the integrator. Likewise the
// stores are still in-memory DashMaps with no `DbPool`, so
// `with_tenant_connection` has no call site here yet and the maps
// are not partitioned by tenant - the guard rejects unresolved
// tenants, but full per-tenant isolation lands with the Diesel
// writers (migration `2026-11-22-000090_exams`) and a `tenant_id`
// on every record. Hook listeners (`on_ai_*_grounding`,
// `on_course_deleted`) take no `HttpRequest` and read event
// payloads defensively; they carry no tenant scope until event
// payloads carry `tenant_id`.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the e-learning module. Capability checks
// use the `role.check` hook and fail closed (no answer means not
// allowed). The AI helper resolves the caller's company
// server-side from the verified caller (then the
// `user.company_requested` hook); the `X-Company-Id` header is
// never forwarded or trusted.
//
// Events emitted on the hook bus:
//   - `exams.question.created`, `exams.question.updated`
//   - `exams.questions.import_requested`
//     (`{"bank_id", "imported", "skipped", "format": "json"}` - the
//     import/export module may consume it; the import itself is
//     performed by this module and the event is a record of it)
//   - `exams.exam.created`, `exams.exam.published`,
//     `exams.exam.archived`, `exams.exam.started`, `exams.exam.closed`
//   - `exams.attempt.started`, `exams.attempt.autosaved`,
//     `exams.attempt.submitted`, `exams.attempt.expired`
//   - `exams.attempt.proctor_signal`
//   - `exams.attempt.graded`
//     (`{"attempt_id", "exam_id", "user_id", "score", "max_score",
//     "percentage", "passed", "result_id"}`)
//   - `exams.result.published`
//   - `exams.certificate.requested`
//     (`{"attempt_id", "exam_id", "user_id", "result_id", "score",
//     "passed", "module": "exams"}` - the e-learning certificate
//     flow listens to it; this module never issues certificates)
//   - `academic.grade.recorded`
//     (`{"grade_id", "enrollment_id", "component_id", "score"}` -
//     exactly the payload the academic module records when it
//     creates a grade itself; emitted only when the exam is linked
//     to an academic enrollment and component, on grading
//     completion; `grade_id` is this module's result id and
//     `score` is the percentage score)
//   - `notification.requested` (grading completed, result
//     published, certificate requested, in the notifications
//     module's payload shape).
// Events this module listens to (all read defensively - a missing
// field or an unknown record is skipped, never an error):
//   - `exams.ai.draft_questions_requested`,
//     `exams.ai.analyse_results_requested` - AI grounding hooks:
//     this module answers with grounding context through
//     `ctx.add_result` (question-bank topic counts for drafting;
//     the exam's anonymised per-question statistics for analysis;
//     never raw student answers or student identities).
//   - `elearning.course.deleted` - exams linked to a deleted
//     course keep their link but are marked unlinked in the
//     response (the course itself is never deleted here).

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai::register_ai_capabilities;
use actix_web::web;
use std::sync::Arc;
use super::{constants::*, settings::*, types::*, helpers::*, inputs::*, handlers_banks::*, handlers_questions::*, handlers_exams::*, handlers_attempts::*, handlers_grading::*, handlers_results::*, handlers_ai::*, handlers_settings::*, hooks::*};


pub struct ExamsModule {
    pub(crate) state: ExamsStateRef,
}


impl ExamsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(ExamsState::default()),
        }
    }
}


impl Default for ExamsModule {
    fn default() -> Self {
        Self::new()
    }
}



// ---------------------------------------------------------------------------
// Module implementation
// ---------------------------------------------------------------------------
impl Module for ExamsModule {
    fn id(&self) -> &'static str {
        "exams"
    }

    fn name(&self) -> &'static str {
        "Online Exams"
    }

    fn description(&self) -> &'static str {
        "Online exams: question banks with versioned questions, timed exams with scheduled windows and seeded randomisation, server-side attempt timers with autosave and resume, auto-grading for objective questions, manual grading queues with rubrics, disputes and second graders, audited regrades and score overrides, result publication, per-question statistics and score distribution, certificate requests and academic grade flow as events only, and AI-assisted question drafting and result analysis. All lists, defaults, and limits are admin-configurable settings, never hard-coded."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["elearning", "academic", "notifications", "billing", "storage", "roles"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["elearning", "academic", "notifications", "billing", "storage", "roles"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        register_ai_capabilities("exams", AI_CAPABILITIES);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVT_AI_DRAFT_GROUNDING,
            "exams",
            Arc::new(move |ctx: &mut HookContext| {
                on_ai_draft_grounding(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVT_AI_ANALYSE_GROUNDING,
            "exams",
            Arc::new(move |ctx: &mut HookContext| {
                on_ai_analyse_grounding(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVT_COURSE_DELETED,
            "exams",
            Arc::new(move |ctx: &mut HookContext| {
                on_course_deleted(state.clone(), ctx)
            }),
        );
        // Paid certification exams: both order events fire for one
        // payment; activation is idempotent, so the same handler
        // answers both (same convention as the e-learning module).
        let state = self.state.clone();
        bus.register(
            ORDER_PAID,
            "exams",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_paid(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STORE_ORDER_COMPLETED,
            "exams",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_paid(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/exams")
                .app_data(state)
                .route("/question-banks", web::get().to(list_banks))
                .route("/question-banks", web::post().to(create_bank))
                .route("/question-banks/{id}", web::get().to(get_bank))
                .route("/question-banks/{id}", web::patch().to(update_bank))
                .route("/question-banks/{id}", web::delete().to(delete_bank))
                .route("/question-banks/{id}/questions", web::get().to(list_bank_questions))
                .route("/question-banks/{id}/questions", web::post().to(create_question))
                .route("/question-banks/{id}/questions/import", web::post().to(import_questions))
                .route("/question-banks/{id}/questions/export", web::get().to(export_questions))
                .route("/questions/{id}", web::get().to(get_question))
                .route("/questions/{id}", web::patch().to(update_question))
                .route("/questions/{id}", web::delete().to(delete_question))
                .route("/questions/{id}/versions", web::get().to(list_question_versions))
                .route("/exams", web::get().to(list_exams))
                .route("/exams", web::post().to(create_exam))
                .route("/exams/{id}", web::get().to(get_exam))
                .route("/exams/{id}", web::patch().to(update_exam))
                .route("/exams/{id}", web::delete().to(delete_exam))
                .route("/exams/{id}/publish", web::post().to(publish_exam))
                .route("/exams/{id}/archive", web::post().to(archive_exam))
                .route("/exams/{id}/close", web::post().to(close_exam))
                .route("/exams/{id}/duplicate", web::post().to(duplicate_exam))
                .route("/exams/{id}/questions", web::get().to(list_exam_questions))
                .route("/exams/{id}/questions", web::post().to(attach_exam_question))
                .route("/exams/{id}/questions/{question_id}", web::delete().to(detach_exam_question))
                .route("/exams/{id}/attempts", web::post().to(start_attempt))
                .route("/attempts", web::get().to(list_attempts))
                .route("/attempts/{id}", web::get().to(get_attempt))
                .route("/my/attempts", web::get().to(list_my_attempts))
                .route("/attempts/{id}/heartbeat", web::post().to(heartbeat_attempt))
                .route("/attempts/{id}/autosave", web::post().to(autosave_attempt))
                .route("/attempts/{id}/submit", web::post().to(submit_attempt))
                .route("/attempts/{id}/expire", web::post().to(expire_attempt))
                .route("/attempts/{id}/proctor-signal", web::post().to(record_proctor_signal))
                .route("/attempts/{id}/proctor-signals", web::get().to(list_proctor_signals))
                .route("/exams/{id}/rubrics", web::get().to(list_rubrics))
                .route("/exams/{id}/rubrics", web::post().to(create_rubric))
                .route("/rubrics/{id}", web::get().to(get_rubric))
                .route("/rubrics/{id}", web::patch().to(update_rubric))
                .route("/rubrics/{id}", web::delete().to(delete_rubric))
                .route("/grading-queue", web::get().to(list_grading_queue))
                .route("/attempts/{id}/grading", web::get().to(get_attempt_grading))
                .route("/attempts/{id}/grade", web::post().to(grade_attempt))
                .route("/attempts/{id}/regrade", web::post().to(regrade_attempt))
                .route("/attempts/{id}/override", web::post().to(override_attempt_score))
                .route("/grades/{id}/dispute", web::post().to(dispute_grade))
                .route("/grades/{id}/second-grade", web::post().to(second_grade))
                .route("/exams/{id}/results", web::get().to(list_exam_results))
                .route("/results/{id}", web::get().to(get_result))
                .route("/my/results", web::get().to(list_my_results))
                .route("/exams/{id}/results/publish", web::post().to(publish_exam_results))
                .route("/results/{id}/certificate", web::post().to(request_result_certificate))
                .route("/exams/{id}/analytics", web::get().to(get_exam_analytics))
                .route("/exams/{id}/question-stats", web::get().to(get_question_stats))
                .route("/ai/draft-questions", web::post().to(ai_draft_questions))
                .route("/ai/analyse-results", web::post().to(ai_analyse_results))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/exams/question-banks", owner: "exams", description: "List question banks (paginated, filter by category, owner_id, company_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/question-banks", owner: "exams", description: "Create a question bank (manager; category validated against settings).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/question-banks/{id}", owner: "exams", description: "Get one question bank.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/exams/question-banks/{id}", owner: "exams", description: "Update a question bank (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/exams/question-banks/{id}", owner: "exams", description: "Delete an empty question bank (manager; blocked while it holds questions).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/question-banks/{id}/questions", owner: "exams", description: "List questions of a bank (paginated, filter by type, difficulty, category, status; correct answers hidden from non-managers).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/question-banks/{id}/questions", owner: "exams", description: "Add a question to a bank (manager; type, category, difficulty validated against settings).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/question-banks/{id}/questions/import", owner: "exams", description: "Import questions from JSON (manager; drafts only; per-item errors reported).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/question-banks/{id}/questions/export", owner: "exams", description: "Export questions of a bank as JSON (correct answers included only for managers).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/questions/{id}", owner: "exams", description: "Get one question (correct answers hidden from non-managers).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/exams/questions/{id}", owner: "exams", description: "Update a question (manager; editing a used question creates a new version, old attempts keep their version).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/exams/questions/{id}", owner: "exams", description: "Delete a question (manager; blocked once attempts exist or it is attached to an exam).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/questions/{id}/versions", owner: "exams", description: "List the version history of a question (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/exams", owner: "exams", description: "List exams (paginated, filter by status, category, course_id, bank_id; learners see published or closed only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/exams", owner: "exams", description: "Create an exam in draft (manager; duration, pass mark, attempts allowed, settings).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/exams/{id}", owner: "exams", description: "Get one exam (drafts only for managers).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/exams/exams/{id}", owner: "exams", description: "Update an exam (manager; closed or archived exams cannot be edited).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/exams/exams/{id}", owner: "exams", description: "Delete an exam with no attempts (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/exams/{id}/publish", owner: "exams", description: "Publish a draft exam (manager; needs at least one question).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/exams/{id}/archive", owner: "exams", description: "Archive a published or draft exam (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/exams/{id}/close", owner: "exams", description: "Close a published exam (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/exams/{id}/duplicate", owner: "exams", description: "Duplicate an exam as a new draft with its question set (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/exams/{id}/questions", owner: "exams", description: "List the question set of an exam (paginated, ordered by position).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/exams/{id}/questions", owner: "exams", description: "Attach a question to an exam with a pinned version (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/exams/exams/{id}/questions/{question_id}", owner: "exams", description: "Detach a question from an exam (manager; blocked once attempts exist).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/exams/{id}/attempts", owner: "exams", description: "Start an attempt (taker; resumes an in-progress attempt with remaining server time; seeded randomised order).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/attempts", owner: "exams", description: "List attempts (own only unless manager or grader; filter by exam_id, user_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/attempts/{id}", owner: "exams", description: "Get one attempt with server remaining time (own only unless manager or grader).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/my/attempts", owner: "exams", description: "The caller's attempts (paginated, filter by exam_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/attempts/{id}/heartbeat", owner: "exams", description: "Heartbeat (taker; server remaining time; the server expires overdue attempts).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/attempts/{id}/autosave", owner: "exams", description: "Autosave draft answers (taker; answers validated against the attempt's question set).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/attempts/{id}/submit", owner: "exams", description: "Submit an attempt for grading (taker; auto-grades objective answers, queues essays for manual grading).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/attempts/{id}/expire", owner: "exams", description: "Mark an in-progress attempt expired (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/attempts/{id}/proctor-signal", owner: "exams", description: "Record a proctoring signal such as a tab switch (taker; recorded only, never auto-fails an attempt).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/attempts/{id}/proctor-signals", owner: "exams", description: "List the proctoring signals of an attempt (own only unless manager or grader).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/exams/{id}/rubrics", owner: "exams", description: "List the rubrics of an exam (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/exams/{id}/rubrics", owner: "exams", description: "Create a rubric with criteria (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/rubrics/{id}", owner: "exams", description: "Get one rubric.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/exams/rubrics/{id}", owner: "exams", description: "Update a rubric (manager; blocked once it has graded answers).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/exams/rubrics/{id}", owner: "exams", description: "Delete a rubric (manager; blocked while referenced by manual grades).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/grading-queue", owner: "exams", description: "List attempts waiting for manual grading (grader; filter by exam_id, attempt_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/attempts/{id}/grading", owner: "exams", description: "The manual-grading view of one attempt (grader; essay answers plus rubrics).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/attempts/{id}/grade", owner: "exams", description: "Submit manual grades for an attempt's essays (grader; rubric criterion scores must sum to the awarded marks).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/attempts/{id}/regrade", owner: "exams", description: "Reopen a graded attempt for grading again (manager; audited).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/attempts/{id}/override", owner: "exams", description: "Override the final score of a graded attempt (manager; audited, original score kept in the event).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/grades/{id}/dispute", owner: "exams", description: "Flag a manual grade as disputed (grader).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/grades/{id}/second-grade", owner: "exams", description: "Second grader's grade for a disputed grade (grader; must differ from the first grader).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/exams/{id}/results", owner: "exams", description: "List results of an exam (manager or grader; filter by passed, status, user_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/results/{id}", owner: "exams", description: "Get one result (own only unless manager or grader).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/my/results", owner: "exams", description: "The caller's results (paginated, filter by exam_id, status, passed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/exams/{id}/results/publish", owner: "exams", description: "Publish the draft results of an exam (manager; notifies students).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/results/{id}/certificate", owner: "exams", description: "Request a certificate for a passed, published result (events only; the e-learning certificate flow issues it).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/exams/{id}/analytics", owner: "exams", description: "Exam analytics (manager or grader; pass rate, mean, median, score distribution, question statistics, proctor signal count).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/exams/{id}/question-stats", owner: "exams", description: "Per-question statistics (manager or grader; attempted %, correct %, discrimination index).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/ai/draft-questions", owner: "exams", description: "Draft questions with AI help (manager; drafts only, never auto-published).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/exams/ai/analyse-results", owner: "exams", description: "Analyse an exam's anonymised result statistics with AI help (manager or grader).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/exams/settings", owner: "exams", description: "Get the configurable exams settings (question types, difficulties, categories, defaults).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/exams/settings", owner: "exams", description: "Update the configurable exams settings (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}


pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ExamsModule::new()));
}
