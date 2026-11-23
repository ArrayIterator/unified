// Academic System module (batch 13, builder B1).
//
// Students, lecturers, study programs, academic subjects, class
// schedules and rooms, course registration with advisor approval,
// attendance, grades and GPA, grade appeals, official transcripts,
// academic calendar, thesis / final-project tracking, and academic
// standing. Interconnection happens only through the hook bus — no
// module calls another module directly.
//
// Identity rule: a student or lecturer record stores the linked user
// id ONLY. Names, emails, and all other profile data live in the
// Users module and are resolved by callers through the Users API.
// This module never duplicates user records.
//
// E-learning rule: the E-Learning module owns course content
// (lessons, quizzes, attempts, certificates). An academic subject
// only REFERENCES an e-learning course id (`elearning_course_id`);
// it never stores lesson or quiz data. The reference is verified
// through the `elearning.course.requested` hook query (see the
// CONVERGENCE section below).
//
// Privacy rule (enforced server-side on every handler, never just
// filtered in a UI): a student sees ONLY their own academic data. A
// lecturer sees only the classes they teach and the students
// enrolled in them. Anything else requires `academic:read_all`.
// Records a caller may not read are answered with 404, so ids
// cannot be probed.
//
// Printing rule: transcripts are never printed from here. Issuing
// or printing a transcript only emits `print.job.requested` (and
// `academic.transcript.print_requested`) for the printing module;
// the transcript payload travels inside the event.
//
// Role-based access (enforced server-side on every handler):
//   - `academic:read`       — read academic records (own records
//                             for students; own classes for
//                             lecturers).
//   - `academic:read_all`   — read every student's academic data.
//   - `academic:manage`     — create/update/delete programs,
//                             subjects, schedules, rooms, semesters,
//                             calendar entries, definitions, and
//                             settings; approve or reject
//                             registrations; issue transcripts.
//   - `academic:advise`     — advisor approval of registrations and
//                             thesis advising for assigned students.
//   - `academic:grade`      — record grades for subjects the caller
//                             teaches.
//   - `academic:transcript` — issue official transcripts.
// Capabilities are checked through the `role.check` hook and fail
// closed: no answer means not allowed. The admin role holds `"*":
// ["*"]`, so it passes every check. No `academic:*` capability is
// granted by the default roles in the Roles module; an administrator
// must grant them.
//
// Modularity rule: no business list, rule, or status vocabulary is
// hard-coded. Status vocabularies, grade scales, standing rules,
// and state-machine transitions live in the admin-editable
// `academic_definitions` table (kind + key + JSON config) and are
// exposed through the definitions endpoints. The constants in this
// file marked CONFIGURABLE DEFAULT are only fallbacks used when no
// definition exists yet for that kind; an administrator can replace
// them at runtime through the API without touching code. Credit
// limits are per program (`max_credits_per_semester`) with a
// configurable default from `academic_settings`. Grade component
// weights are configurable per subject. GPA thresholds live in
// standing-rule definitions.
//
// Events emitted:
//   - `academic.student.created`        {student_id, user_id, program_id, application_id}
//                                       (`application_id` is the
//                                       admissions application the
//                                       student was created from, or
//                                       null for a directly created
//                                       student; admissions links back
//                                       on this key.)
//   - `academic.lecturer.created`       {lecturer_id, user_id}
//   - `academic.program.created`        {program_id, code}
//   - `academic.subject.created`        {subject_id, code, program_id}
//   - `academic.subject.updated`        {subject_id}
//   - `academic.schedule.created`       {schedule_id, subject_id, room_id, semester_id}
//   - `academic.enrollment.requested`   {enrollment_id, student_id, subject_id, semester_id, credits}
//   - `academic.enrollment.approved`    {enrollment_id, student_id, subject_id, semester_id}
//   - `academic.enrollment.rejected`    {enrollment_id, student_id, subject_id, reason}
//   - `academic.enrollment.dropped`     {enrollment_id, student_id, subject_id}
//   - `academic.enrollment.completed`   {enrollment_id, student_id, subject_id}
//   - `academic.attendance.recorded`    {attendance_id, student_id, schedule_id, date, status, source}
//   - `academic.grade.recorded`         {grade_id, enrollment_id, component_id, score}
//   - `academic.grade.appeal_filed`     {appeal_id, enrollment_id, student_id}
//   - `academic.grade.appeal_resolved`  {appeal_id, enrollment_id, decision}
//   - `academic.transcript.issued`      {transcript_id, student_id, gpa, total_credits}
//   - `academic.transcript.print_requested` {transcript_id, student_id}
//   - `print.job.requested`             {source: "academic", document_type: "transcript", transcript_id, student_id, title}
//   - `academic.thesis.created`         {thesis_id, student_id, advisor_id}
//   - `academic.thesis.defense_scheduled` {thesis_id, student_id, defense_date}
//   - `academic.thesis.status_changed`  {thesis_id, student_id, from_status, to_status}
//   - `academic.standing.changed`       {student_id, semester_id, standing, gpa}
//   - `notification.requested`          registration decisions, appeal
//                                       outcomes, defense scheduling,
//                                       and standing warnings (the
//                                       Notifications module listens).
// Events consumed:
//   - `timetracker.time.logged`         the event the TimeTracker
//                                       module in this tree actually
//                                       emits (payload: `{entry_id,
//                                       user_id, started_at, ended_at,
//                                       duration_secs, hours, billable,
//                                       source, ...}`).
//   - `timetracker.entry_logged`        the event name the brief
//                                       documents (payload: `{user_id,
//                                       date, minutes, source}`).
//                                       Both shapes are accepted; a
//                                       matching class session on that
//                                       date records attendance with
//                                       source "timetracker".
//   - `elearning.course.created` / `elearning.course.updated` /
//     `elearning.course.deleted`        refresh the verified flag of
//                                       linked subjects.
//   - `elearning.enrollment.completed`  `{enrollment_id, course_id,
//                                       user_id}` — marks a linked
//                                       academic enrollment completed.
//   - `academic.student.create_requested` `{application_id,
//                                       period_id, applicant_user_id
//                                       (or `user_id`), full_name,
//                                       email, phone, program_id,
//                                       level, academic_year, term,
//                                       enrollment_year?}` — sent by
//                                       admissions when an accepted
//                                       application is enrolled; this
//                                       module creates the student
//                                       record and answers with
//                                       `academic.student.created`
//                                       carrying the same
//                                       `application_id`.
//   - `academic.student.lookup`         `{student_ref}` (also
//                                       accepts `student_id` /
//                                       `user_id`) — hook query
//                                       answered with one result
//                                       `{exists, student_id?,
//                                       user_id?, program_id?}`; the
//                                       payload key `exists` is also
//                                       set for callers that read the
//                                       payload instead of results.
//   - `calendar.items_requested`        `{tenant_id, company_id,
//                                       user_id, start, end,
//                                       modules?}` (RFC 3339 UTC
//                                       bounds) — answered with the
//                                       academic calendar events,
//                                       class-schedule occurrences,
//                                       and thesis defenses in the
//                                       window, in the shared calendar
//                                       item shape
//                                       (`{id, module: "academic",
//                                       title, start, end?, all_day,
//                                       label?, deep_link?,
//                                       visibility: "shared",
//                                       owner_user_id?}`); the global
//                                       calendar aggregates, this
//                                       module never renders a
//                                       calendar view of its own.
// Hook queries emitted (answers read, never subscribed):
//   - `elearning.course.requested`      `{course_id}` — expects
//                                       results shaped `{exists,
//                                       course_id, title, status}`.
//   - `role.check`                      capability checks (fail
//                                       closed).
//
// CONVERGENCE (for the integrator):
//   1. No `elearning.course.requested` handler exists in this tree
//      yet (the E-Learning module only subscribes to order and
//      anchor events). A subject link is therefore stored with
//      `elearning_course_verified: false` when nothing answers, and
//      the link is never rejected on that basis. The integrator
//      should add the answering handler to the E-Learning module
//      (or to the API layer) with the payload above.
//   2. No printing module exists in this tree yet, so no handler
//      consumes `print.job.requested` today. The event is the
//      contract: `{source: "academic", document_type:
//      "transcript", transcript_id, student_id, title, issued_at}`.
//      The printing module should answer it and, when it prints,
//      may emit `print.job.completed` with `{transcript_id}`.
//   3. `src/modules/mod.rs` must gain `pub mod academic;` and an
//      `academic::register(registry);` line in `register_all` — this
//      file cannot do that itself (mod.rs is owned by the
//      integrator). Until then the module compiles but is not wired
//      into the binary.
//   4. The audit module only records events listed in its
//      `AUDITED_EVENTS`; the `academic.*` names above must be added
//      there for academic actions to appear in the audit trail.
//   5. Seeded definition rows are CONFIGURABLE DEFAULTS: they are
//      inserted by the migration and used only when no admin
//      definition exists for that kind. Deleting a seeded row and
//      restarting does not resurrect it in memory, but the
//      migration seed remains the database default for fresh
//      installs.

//! Academic - module struct, route table and registration.
//!
//! Part of the `academic` module split; see `src/modules/academic.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::HookBus;
use unified_core::hooks::HookContext;
use unified_core::hooks::HookHandler;
use unified_core::json;
use unified_core::modules::Module;
use unified_core::modules::ModuleRegistry;
use unified_core::modules::RouteInfo;
use crate::modules::ai;
use actix_web::web;
use serde_json::json;
use std::sync::Arc;

use super::ai_capabilities::AI_CAPABILITIES;
use super::ai::academic_progress_grounding;
use super::ai::ai_capability;
use super::handlers_assessment::create_grade;
use super::handlers_assessment::create_grade_appeal;
use super::handlers_assessment::delete_grade;
use super::handlers_assessment::get_final_grade;
use super::handlers_assessment::get_grade_appeal;
use super::handlers_assessment::get_transcript;
use super::handlers_assessment::issue_transcript;
use super::handlers_assessment::list_grade_appeals;
use super::handlers_assessment::list_grades_for_enrollment;
use super::handlers_assessment::list_transcripts;
use super::handlers_assessment::print_transcript;
use super::handlers_assessment::resolve_grade_appeal;
use super::handlers_assessment::update_grade;
use super::handlers_catalog::create_lecturer;
use super::handlers_catalog::create_program;
use super::handlers_catalog::create_student;
use super::handlers_catalog::create_subject;
use super::handlers_catalog::delete_lecturer;
use super::handlers_catalog::delete_program;
use super::handlers_catalog::delete_student;
use super::handlers_catalog::delete_subject;
use super::handlers_catalog::get_lecturer;
use super::handlers_catalog::get_program;
use super::handlers_catalog::get_student;
use super::handlers_catalog::get_student_gpa;
use super::handlers_catalog::get_student_summary;
use super::handlers_catalog::get_subject;
use super::handlers_catalog::link_elearning_course;
use super::handlers_catalog::list_lecturer_classes;
use super::handlers_catalog::list_lecturers;
use super::handlers_catalog::list_programs;
use super::handlers_catalog::list_students;
use super::handlers_catalog::list_subjects;
use super::handlers_catalog::update_lecturer;
use super::handlers_catalog::update_program;
use super::handlers_catalog::update_student;
use super::handlers_catalog::update_subject;
use super::handlers_enrollment::approve_enrollment;
use super::handlers_enrollment::create_attendance;
use super::handlers_enrollment::create_enrollment;
use super::handlers_enrollment::drop_enrollment;
use super::handlers_enrollment::get_attendance;
use super::handlers_enrollment::get_enrollment;
use super::handlers_enrollment::list_attendance;
use super::handlers_enrollment::list_enrollments;
use super::handlers_enrollment::list_schedule_attendance;
use super::handlers_enrollment::list_student_enrollments;
use super::handlers_enrollment::reject_enrollment;
use super::handlers_enrollment::update_attendance;
use super::handlers_scheduling::activate_semester;
use super::handlers_scheduling::create_calendar_event;
use super::handlers_scheduling::create_grade_component;
use super::handlers_scheduling::create_room;
use super::handlers_scheduling::create_schedule;
use super::handlers_scheduling::create_semester;
use super::handlers_scheduling::delete_calendar_event;
use super::handlers_scheduling::delete_grade_component;
use super::handlers_scheduling::delete_room;
use super::handlers_scheduling::delete_schedule;
use super::handlers_scheduling::delete_semester;
use super::handlers_scheduling::get_calendar_event;
use super::handlers_scheduling::get_room;
use super::handlers_scheduling::get_schedule;
use super::handlers_scheduling::get_semester;
use super::handlers_scheduling::list_calendar_events;
use super::handlers_scheduling::list_grade_components;
use super::handlers_scheduling::list_rooms;
use super::handlers_scheduling::list_schedules;
use super::handlers_scheduling::list_semesters;
use super::handlers_scheduling::update_calendar_event;
use super::handlers_scheduling::update_grade_component;
use super::handlers_scheduling::update_room;
use super::handlers_scheduling::update_schedule;
use super::handlers_scheduling::update_semester;
use super::handlers_thesis::create_definition;
use super::handlers_thesis::create_thesis;
use super::handlers_thesis::delete_definition;
use super::handlers_thesis::delete_thesis;
use super::handlers_thesis::get_definition;
use super::handlers_thesis::get_thesis;
use super::handlers_thesis::list_definitions;
use super::handlers_thesis::list_settings;
use super::handlers_thesis::list_standing;
use super::handlers_thesis::list_student_standing;
use super::handlers_thesis::list_theses;
use super::handlers_thesis::recompute_standing;
use super::handlers_thesis::schedule_defense;
use super::handlers_thesis::update_definition;
use super::handlers_thesis::update_setting;
use super::handlers_thesis::update_thesis;
use super::hooks::on_academic_student_lookup;
use super::hooks::on_admissions_student_create_requested;
use super::hooks::on_calendar_items_requested;
use super::hooks::on_elearning_course_event;
use super::hooks::on_elearning_enrollment_completed;
use super::hooks::on_timetracker_entry;
use super::state::AcademicState;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct AcademicModule {
    state: Arc<AcademicState>,
}

impl AcademicModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(AcademicState::new()),
        }
    }
}

impl Default for AcademicModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for AcademicModule {
    fn id(&self) -> &'static str {
        "academic"
    }

    fn name(&self) -> &'static str {
        "Academic System"
    }

    fn description(&self) -> &'static str {
        "Academic system: students, lecturers, study programs, subjects linked to e-learning courses, class schedules and rooms, course registration with advisor approval, attendance, grades and GPA, grade appeals, official transcripts, academic calendar, thesis tracking, and academic standing."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "elearning", "timetracker", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "elearning", "timetracker", "notifications"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self.state.hooks.write().expect("hooks lock poisoned") = ctx.hooks.clone();
        ai::register_ai_capabilities("academic", AI_CAPABILITIES);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Both TimeTracker event names (see the module header): the
        // documented `timetracker.entry_logged` and the
        // `timetracker.time.logged` the TimeTracker module in this
        // tree actually emits. One handler accepts both payloads.
        for event in ["timetracker.entry_logged", "timetracker.time.logged"] {
            let state = self.state.clone();
            let handler: HookHandler =
                Arc::new(move |ctx: &mut HookContext| on_timetracker_entry(state.clone(), ctx));
            bus.register(event, "academic", handler);
        }
        // E-learning course lifecycle: keep subject link flags fresh.
        for (event, verified) in [
            ("elearning.course.created", true),
            ("elearning.course.updated", true),
            ("elearning.course.deleted", false),
        ] {
            let state = self.state.clone();
            let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
                on_elearning_course_event(state.clone(), ctx, verified)
            });
            bus.register(event, "academic", handler);
        }
        let state = self.state.clone();
        bus.register(
            "elearning.enrollment.completed",
            "academic",
            Arc::new(move |ctx: &mut HookContext| {
                on_elearning_enrollment_completed(state.clone(), ctx)
            }),
        );
        // Admissions handoff: an accepted application is enrolled
        // by creating the student record here and answering with
        // `academic.student.created` carrying the application id.
        let state = self.state.clone();
        bus.register(
            "academic.student.create_requested",
            "academic",
            Arc::new(move |ctx: &mut HookContext| {
                on_admissions_student_create_requested(state.clone(), ctx)
            }),
        );
        // Student lookup hook query (curriculum asks for it): one
        // result `{exists, student_id?, user_id?, program_id?}`.
        let state = self.state.clone();
        bus.register(
            "academic.student.lookup",
            "academic",
            Arc::new(move |ctx: &mut HookContext| on_academic_student_lookup(state.clone(), ctx)),
        );
        // Global calendar aggregation: contribute the academic
        // calendar events, class-schedule occurrences, and thesis
        // defenses in the requested window.
        let state = self.state.clone();
        bus.register(
            "calendar.items_requested",
            "academic",
            Arc::new(move |ctx: &mut HookContext| on_calendar_items_requested(state.clone(), ctx)),
        );
        // Ground the `summarize_progress` AI capability with the
        // anonymised progress aggregates (see
        // `academic_progress_grounding`). Tenant scoping: this store
        // is in-memory with no tenant partition yet; when batch 17
        // persistence lands, this handler must only ground students
        // of the calling tenant, keyed by the caller company id in
        // the payload — the endpoint already enforces the module's
        // own read scoping for the named student.
        let state = self.state.clone();
        bus.register(
            "academic.ai.summarize_progress_requested",
            "academic",
            Arc::new(move |ctx: &mut HookContext| {
                let student_id = ctx
                    .get("input")
                    .and_then(|input| input.get("student_id"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if student_id.trim().is_empty() {
                    ctx.add_result(json!({}));
                    return;
                }
                ctx.add_result(academic_progress_grounding(&state, &student_id));
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/academic")
                .route("/programs", web::get().to(list_programs))
                .route("/programs", web::post().to(create_program))
                .route("/programs/{id}", web::get().to(get_program))
                .route("/programs/{id}", web::put().to(update_program))
                .route("/programs/{id}", web::delete().to(delete_program))
                .route("/students", web::get().to(list_students))
                .route("/students", web::post().to(create_student))
                .route("/students/{id}", web::get().to(get_student))
                .route("/students/{id}", web::put().to(update_student))
                .route("/students/{id}", web::delete().to(delete_student))
                .route("/students/{id}/summary", web::get().to(get_student_summary))
                .route("/students/{id}/gpa", web::get().to(get_student_gpa))
                .route(
                    "/students/{id}/enrollments",
                    web::get().to(list_student_enrollments),
                )
                .route(
                    "/students/{id}/transcripts",
                    web::get().to(list_transcripts),
                )
                .route(
                    "/students/{id}/transcripts",
                    web::post().to(issue_transcript),
                )
                .route(
                    "/students/{id}/standing",
                    web::get().to(list_student_standing),
                )
                .route("/lecturers", web::get().to(list_lecturers))
                .route("/lecturers", web::post().to(create_lecturer))
                .route("/lecturers/{id}", web::get().to(get_lecturer))
                .route("/lecturers/{id}", web::put().to(update_lecturer))
                .route("/lecturers/{id}", web::delete().to(delete_lecturer))
                .route(
                    "/lecturers/{id}/classes",
                    web::get().to(list_lecturer_classes),
                )
                .route("/subjects", web::get().to(list_subjects))
                .route("/subjects", web::post().to(create_subject))
                .route("/subjects/{id}", web::get().to(get_subject))
                .route("/subjects/{id}", web::put().to(update_subject))
                .route("/subjects/{id}", web::delete().to(delete_subject))
                .route(
                    "/subjects/{id}/link-elearning",
                    web::post().to(link_elearning_course),
                )
                .route(
                    "/subjects/{id}/components",
                    web::get().to(list_grade_components),
                )
                .route(
                    "/subjects/{id}/components",
                    web::post().to(create_grade_component),
                )
                .route(
                    "/grade-components/{id}",
                    web::put().to(update_grade_component),
                )
                .route(
                    "/grade-components/{id}",
                    web::delete().to(delete_grade_component),
                )
                .route("/rooms", web::get().to(list_rooms))
                .route("/rooms", web::post().to(create_room))
                .route("/rooms/{id}", web::get().to(get_room))
                .route("/rooms/{id}", web::put().to(update_room))
                .route("/rooms/{id}", web::delete().to(delete_room))
                .route("/semesters", web::get().to(list_semesters))
                .route("/semesters", web::post().to(create_semester))
                .route("/semesters/{id}", web::get().to(get_semester))
                .route("/semesters/{id}", web::put().to(update_semester))
                .route("/semesters/{id}", web::delete().to(delete_semester))
                .route(
                    "/semesters/{id}/activate",
                    web::post().to(activate_semester),
                )
                .route("/calendar", web::get().to(list_calendar_events))
                .route("/calendar", web::post().to(create_calendar_event))
                .route("/calendar/{id}", web::get().to(get_calendar_event))
                .route("/calendar/{id}", web::put().to(update_calendar_event))
                .route("/calendar/{id}", web::delete().to(delete_calendar_event))
                .route("/schedules", web::get().to(list_schedules))
                .route("/schedules", web::post().to(create_schedule))
                .route("/schedules/{id}", web::get().to(get_schedule))
                .route("/schedules/{id}", web::put().to(update_schedule))
                .route("/schedules/{id}", web::delete().to(delete_schedule))
                .route(
                    "/schedules/{id}/attendance",
                    web::get().to(list_schedule_attendance),
                )
                .route("/enrollments", web::get().to(list_enrollments))
                .route("/enrollments", web::post().to(create_enrollment))
                .route("/enrollments/{id}", web::get().to(get_enrollment))
                .route(
                    "/enrollments/{id}/approve",
                    web::post().to(approve_enrollment),
                )
                .route(
                    "/enrollments/{id}/reject",
                    web::post().to(reject_enrollment),
                )
                .route("/enrollments/{id}/drop", web::post().to(drop_enrollment))
                .route("/attendance", web::get().to(list_attendance))
                .route("/attendance", web::post().to(create_attendance))
                .route("/attendance/{id}", web::get().to(get_attendance))
                .route("/attendance/{id}", web::put().to(update_attendance))
                .route(
                    "/enrollments/{id}/grades",
                    web::get().to(list_grades_for_enrollment),
                )
                .route("/enrollments/{id}/grades", web::post().to(create_grade))
                .route(
                    "/enrollments/{id}/final-grade",
                    web::get().to(get_final_grade),
                )
                .route("/grades/{id}", web::put().to(update_grade))
                .route("/grades/{id}", web::delete().to(delete_grade))
                .route("/grade-appeals", web::get().to(list_grade_appeals))
                .route("/grade-appeals", web::post().to(create_grade_appeal))
                .route("/grade-appeals/{id}", web::get().to(get_grade_appeal))
                .route(
                    "/grade-appeals/{id}/resolve",
                    web::post().to(resolve_grade_appeal),
                )
                .route("/transcripts/{id}", web::get().to(get_transcript))
                .route("/transcripts/{id}/print", web::post().to(print_transcript))
                .route("/theses", web::get().to(list_theses))
                .route("/theses", web::post().to(create_thesis))
                .route("/theses/{id}", web::get().to(get_thesis))
                .route("/theses/{id}", web::put().to(update_thesis))
                .route("/theses/{id}", web::delete().to(delete_thesis))
                .route("/theses/{id}/defense", web::post().to(schedule_defense))
                .route("/standing", web::get().to(list_standing))
                .route("/standing/recompute", web::post().to(recompute_standing))
                .route("/definitions", web::get().to(list_definitions))
                .route("/definitions", web::post().to(create_definition))
                .route("/definitions/{id}", web::get().to(get_definition))
                .route("/definitions/{id}", web::put().to(update_definition))
                .route("/definitions/{id}", web::delete().to(delete_definition))
                .route("/settings", web::get().to(list_settings))
                .route("/settings/{key}", web::put().to(update_setting))
                .route("/ai/{capability}", web::post().to(ai_capability)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/programs",
                owner: "academic",
                description: "List study programs.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/programs",
                owner: "academic",
                description: "Create a study program (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/programs/{id}",
                owner: "academic",
                description: "Get one study program.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/academic/programs/{id}",
                owner: "academic",
                description: "Update a study program (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/academic/programs/{id}",
                owner: "academic",
                description: "Delete a study program (academic:manage; refused with students or subjects).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/students",
                owner: "academic",
                description: "List students (staff see all; a student sees only their own record; advisors see their advisees).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/students",
                owner: "academic",
                description: "Create a student record linked to a user account (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/students/{id}",
                owner: "academic",
                description: "Get one student (self, advisor, or academic:read_all).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/academic/students/{id}",
                owner: "academic",
                description: "Update a student (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/academic/students/{id}",
                owner: "academic",
                description: "Delete a student (academic:manage; refused with enrollments or a thesis).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/students/{id}/summary",
                owner: "academic",
                description: "A student's academic summary: GPA, credits, standing, enrollment and attendance counts (self, advisor, or academic:read_all).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/students/{id}/gpa",
                owner: "academic",
                description: "A student's GPA, credits, and standing (self, advisor, or academic:read_all).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/students/{id}/enrollments",
                owner: "academic",
                description: "List one student's enrollments (self, advisor, or academic:read_all).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/students/{id}/transcripts",
                owner: "academic",
                description: "List one student's issued transcripts (self, advisor, or academic:read_all).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/students/{id}/transcripts",
                owner: "academic",
                description: "Issue an official transcript (academic:transcript).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/students/{id}/standing",
                owner: "academic",
                description: "List one student's standing records (self, advisor, or academic:read_all).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/lecturers",
                owner: "academic",
                description: "List lecturers (a lecturer sees only their own record).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/lecturers",
                owner: "academic",
                description: "Create a lecturer record linked to a user account (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/lecturers/{id}",
                owner: "academic",
                description: "Get one lecturer (self, manager, or academic:read_all).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/academic/lecturers/{id}",
                owner: "academic",
                description: "Update a lecturer (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/academic/lecturers/{id}",
                owner: "academic",
                description: "Delete a lecturer (academic:manage; refused with active schedules or advisees).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/lecturers/{id}/classes",
                owner: "academic",
                description: "List the class schedules one lecturer teaches (self, manager, or academic:read_all).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/subjects",
                owner: "academic",
                description: "List academic subjects.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/subjects",
                owner: "academic",
                description: "Create an academic subject; an e-learning course id may be linked and is verified through the e-learning hook (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/subjects/{id}",
                owner: "academic",
                description: "Get one subject.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/academic/subjects/{id}",
                owner: "academic",
                description: "Update a subject (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/academic/subjects/{id}",
                owner: "academic",
                description: "Delete a subject (academic:manage; refused with enrollments or active schedules).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/subjects/{id}/link-elearning",
                owner: "academic",
                description: "Link (or re-verify) an e-learning course to a subject through the e-learning hook (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/subjects/{id}/components",
                owner: "academic",
                description: "List a subject's grade components with their weights.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/subjects/{id}/components",
                owner: "academic",
                description: "Add a grade component with a configurable weight (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/academic/grade-components/{id}",
                owner: "academic",
                description: "Update a grade component (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/academic/grade-components/{id}",
                owner: "academic",
                description: "Delete a grade component (academic:manage; refused while grades reference it).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/rooms",
                owner: "academic",
                description: "List rooms.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/rooms",
                owner: "academic",
                description: "Create a room (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/rooms/{id}",
                owner: "academic",
                description: "Get one room.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/academic/rooms/{id}",
                owner: "academic",
                description: "Update a room (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/academic/rooms/{id}",
                owner: "academic",
                description: "Delete a room (academic:manage; refused with active schedules).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/semesters",
                owner: "academic",
                description: "List semesters.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/semesters",
                owner: "academic",
                description: "Create a semester (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/semesters/{id}",
                owner: "academic",
                description: "Get one semester.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/academic/semesters/{id}",
                owner: "academic",
                description: "Update a semester (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/academic/semesters/{id}",
                owner: "academic",
                description: "Delete a semester (academic:manage; refused with enrollments or active schedules).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/semesters/{id}/activate",
                owner: "academic",
                description: "Mark a semester current (academic:manage; the previous current semester returns to active).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/calendar",
                owner: "academic",
                description: "List academic calendar events (holidays, exam periods, registration windows).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/calendar",
                owner: "academic",
                description: "Create a calendar event (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/calendar/{id}",
                owner: "academic",
                description: "Get one calendar event.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/academic/calendar/{id}",
                owner: "academic",
                description: "Update a calendar event (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/academic/calendar/{id}",
                owner: "academic",
                description: "Delete a calendar event (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/schedules",
                owner: "academic",
                description: "List class schedules (a lecturer sees only their own classes).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/schedules",
                owner: "academic",
                description: "Create a class schedule; room and lecturer conflicts are rejected (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/schedules/{id}",
                owner: "academic",
                description: "Get one schedule (its lecturer, manager, or academic:read_all).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/academic/schedules/{id}",
                owner: "academic",
                description: "Update a schedule; conflicts are re-checked (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/academic/schedules/{id}",
                owner: "academic",
                description: "Delete a schedule (academic:manage; refused with attendance records).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/schedules/{id}/attendance",
                owner: "academic",
                description: "List attendance for one class session (its lecturer, manager, or academic:read_all).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/enrollments",
                owner: "academic",
                description: "List enrollments (a student sees only their own; a lecturer sees their subjects' enrollments).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/enrollments",
                owner: "academic",
                description: "Register for a course (self or staff; program credit cap enforced).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/enrollments/{id}",
                owner: "academic",
                description: "Get one enrollment (self, advisor, subject lecturer, or academic:read_all).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/enrollments/{id}/approve",
                owner: "academic",
                description: "Approve a registration (the student's advisor with academic:advise, or academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/enrollments/{id}/reject",
                owner: "academic",
                description: "Reject a registration with a reason (the student's advisor with academic:advise, or academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/enrollments/{id}/drop",
                owner: "academic",
                description: "Drop a registration (self or staff; refused once completed).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/attendance",
                owner: "academic",
                description: "List attendance records (a student sees only their own).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/attendance",
                owner: "academic",
                description: "Record attendance manually (academic:manage; the student must be enrolled in the class).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/attendance/{id}",
                owner: "academic",
                description: "Get one attendance record (self, advisor, or academic:read_all).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/academic/attendance/{id}",
                owner: "academic",
                description: "Update an attendance record (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/enrollments/{id}/grades",
                owner: "academic",
                description: "List the grades of one enrollment (self, advisor, subject lecturer, or academic:read_all).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/enrollments/{id}/grades",
                owner: "academic",
                description: "Record a grade for one component (the subject's lecturer with academic:grade, or academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/enrollments/{id}/final-grade",
                owner: "academic",
                description: "The computed final grade of one enrollment: weight-normalized score, letter, and points (self, advisor, subject lecturer, or academic:read_all).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/academic/grades/{id}",
                owner: "academic",
                description: "Update a grade (the subject's lecturer with academic:grade, or academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/academic/grades/{id}",
                owner: "academic",
                description: "Delete a grade (the subject's lecturer with academic:grade, or academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/grade-appeals",
                owner: "academic",
                description: "List grade appeals (a student sees only their own).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/grade-appeals",
                owner: "academic",
                description: "File a grade appeal (self or staff).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/grade-appeals/{id}",
                owner: "academic",
                description: "Get one grade appeal (self, advisor, or academic:read_all).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/grade-appeals/{id}/resolve",
                owner: "academic",
                description: "Resolve a grade appeal; an upheld appeal with a corrected score rewrites the grade (the subject's lecturer with academic:grade, or academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/transcripts/{id}",
                owner: "academic",
                description: "Get one issued transcript (self, advisor, or academic:read_all).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/transcripts/{id}/print",
                owner: "academic",
                description: "Request printing of a transcript: emits print.job.requested for the printing module (academic:transcript).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/theses",
                owner: "academic",
                description: "List theses (a student sees only their own; an advisor sees their students').",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/theses",
                owner: "academic",
                description: "Create a thesis record (self or staff).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/theses/{id}",
                owner: "academic",
                description: "Get one thesis (self, advisor, co-advisor, or academic:read_all).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/academic/theses/{id}",
                owner: "academic",
                description: "Update a thesis; status changes follow the thesis transition definitions (self, advisor, or academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/academic/theses/{id}",
                owner: "academic",
                description: "Delete a thesis (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/theses/{id}/defense",
                owner: "academic",
                description: "Schedule a defense or record its result (advisor or academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/standing",
                owner: "academic",
                description: "List academic standing records (a student sees only their own).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/standing/recompute",
                owner: "academic",
                description: "Recompute standing from GPA for one student or all students in a semester (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/definitions",
                owner: "academic",
                description: "List admin-editable academic definitions (grade scales, standing rules, status vocabularies, transitions).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/definitions",
                owner: "academic",
                description: "Create an academic definition (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/definitions/{id}",
                owner: "academic",
                description: "Get one definition.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/academic/definitions/{id}",
                owner: "academic",
                description: "Update a definition (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/academic/definitions/{id}",
                owner: "academic",
                description: "Delete a definition (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/academic/settings",
                owner: "academic",
                description: "List academic settings (configurable defaults).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/academic/settings/{key}",
                owner: "academic",
                description: "Set one academic setting (academic:manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/academic/ai/{capability}",
                owner: "academic",
                description: "Run one registered academic AI capability (summarize_progress; requires ai:use plus academic:read and the module's read scoping for the student).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AcademicModule::new()));
}
