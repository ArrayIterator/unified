// Curriculum module — competency-based school curricula.
//
// Routes live under `/api/v1/curriculum`:
//
//   Frameworks (the curriculum registry):
//     GET/POST /frameworks — list frameworks / create a framework.
//     GET/PATCH/DELETE /frameworks/{id}.
//     POST /frameworks/{id}/set-default — make a framework the default.
//   School levels (data, admin-editable):
//     GET/POST /levels, GET/PATCH/DELETE /levels/{id}.
//   Subjects per framework and level:
//     GET/POST /subjects, GET/PATCH/DELETE /subjects/{id}.
//   Competency standards (hierarchical, versioned per curriculum year):
//     GET/POST /core-competencies, GET/PATCH/DELETE /core-competencies/{id}.
//     GET/POST /basic-competencies, GET/PATCH/DELETE /basic-competencies/{id}.
//   Curriculum structure (subjects per level and year, hours allocation):
//     GET/POST /structure, GET/PATCH/DELETE /structure/{id}.
//   Assessment domains (configurable registry, attitudes split into
//   sub-domains as data rows, not hard-coded enums):
//     GET/POST /assessment-domains, GET/PATCH/DELETE /assessment-domains/{id}.
//   Grade descriptors (descriptor text per grade band, stored as data):
//     GET/POST /grade-descriptors, GET/PATCH/DELETE /grade-descriptors/{id}.
//   Lesson plans:
//     GET/POST /lesson-plans, GET/PATCH/DELETE /lesson-plans/{id},
//     POST /lesson-plans/{id}/publish, POST /lesson-plans/{id}/archive.
//   Report cards:
//     GET/POST /report-cards, GET/PATCH/DELETE /report-cards/{id}.
//     GET/POST /report-cards/{id}/entries,
//     PATCH/DELETE /report-cards/entries/{entry_id}.
//     POST /report-cards/{id}/issue — mark a report card issued.
//     POST /report-cards/{id}/print-request — emit a print event for
//       the printing module (this module never calls it directly).
//   Remedial and enrichment interventions:
//     GET/POST /interventions, GET/PATCH/DELETE /interventions/{id},
//     POST /interventions/{id}/complete, POST /interventions/{id}/cancel.
//   Settings (configurable defaults and vocabularies):
//     GET/PATCH /settings.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory stores (DashMap) for every record above, backed by the
// migration tables in `migrations/2026-11-22-000087_curriculum`
// (the tables are the durable shape; the DashMap store is still
// authoritative at runtime, exactly like the other modules built in
// this project — no Diesel queries are issued from this file yet).
// What is NOT built here: no PDF rendering and no direct call into
// any printing module — printing is requested only through events
// (see below). Student, course, and grade records are NOT duplicated
// here: a student is referenced by an opaque `student_ref` string
// and a grade by an opaque `academic_grade_id` string, exactly like
// `webinars.host_id` / `store_orders.user_id` elsewhere.
//
// MODULARITY — READ THIS FIRST. Nothing business-specific is
// hard-coded in this file except standard constants (score range
// 0..=100, UUIDs, HTTP status codes). Every list, rule, and status
// vocabulary is data or admin-editable settings:
//   - curriculum frameworks live in a registry table; the seeded
//     framework is only the default row (see `DEFAULT_FRAMEWORK_CODE`);
//   - school levels, subjects, competencies, structures, assessment
//     domains, and grade descriptors are all admin-editable rows;
//   - the assessment domain list (knowledge, skills, attitudes, and
//     the spiritual / social sub-domains of attitudes) is a registry
//     table, seeded in the migration — new domains or sub-domains are
//     added as rows, never as code;
//   - status vocabularies (lesson-plan, report-card, intervention)
//     and grade bands live in `CurriculumSettings` and are editable
//     through `PATCH /settings`.
// Remaining hard-coded spots (disclosed honestly): the score range
// 0..=100, the seeded default values themselves (they are defaults,
// clearly marked, and editable through the settings endpoint), and
// the event-name strings below (event names are the integration
// contract; they are constants by design).
//
// CURRICULUM FRAMEWORK EXTENSION POINT. A framework is a row in
// `curriculum_frameworks` plus the rows that reference it (levels,
// subjects, competencies, structures, descriptors). Adding another
// national curriculum framework later — for example a newer national
// curriculum — needs NO code change: create a framework row through
// `POST /frameworks`, then add its levels, subjects, competencies,
// and structures through the same endpoints, and set it as the
// default with `POST /frameworks/{id}/set-default` when it should
// become the default. Only the "curriculum 2013" framework is seeded
// here; no second framework is built in this round.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and the
// `X-Role` header (required for capability checks), exactly like the
// e-learning module.
//
// Events emitted on the hook bus:
//   - `curriculum.framework.created`, `curriculum.framework.updated`,
//     `curriculum.framework.deleted`,
//     `curriculum.framework.default_changed`
//   - `curriculum.level.created`, `curriculum.level.updated`,
//     `curriculum.level.deleted`
//   - `curriculum.subject.created`, `curriculum.subject.updated`,
//     `curriculum.subject.deleted`
//   - `curriculum.core_competency.created`,
//     `curriculum.core_competency.updated`,
//     `curriculum.core_competency.deleted`
//   - `curriculum.basic_competency.created`,
//     `curriculum.basic_competency.updated`,
//     `curriculum.basic_competency.deleted`
//   - `curriculum.structure.created`, `curriculum.structure.updated`,
//     `curriculum.structure.deleted`
//   - `curriculum.assessment_domain.created`,
//     `curriculum.assessment_domain.updated`,
//     `curriculum.assessment_domain.deleted`
//   - `curriculum.grade_descriptor.created`,
//     `curriculum.grade_descriptor.updated`,
//     `curriculum.grade_descriptor.deleted`
//   - `curriculum.lesson_plan.created`,
//     `curriculum.lesson_plan.updated`,
//     `curriculum.lesson_plan.deleted`,
//     `curriculum.lesson_plan.published`,
//     `curriculum.lesson_plan.archived`
//   - `curriculum.report_card.created`,
//     `curriculum.report_card.updated`,
//     `curriculum.report_card.deleted`,
//     `curriculum.report_card.issued`
//   - `curriculum.report_card.entry.added`,
//     `curriculum.report_card.entry.updated`,
//     `curriculum.report_card.entry.removed`
//   - `curriculum.report_card.print_requested` — the print request
//     for the printing module, carrying `{"report_card_id",
//     "student_ref", "academic_year", "semester", "paper_size",
//     "document_type": "report_card"}`. The same payload is also
//     emitted as `print.job.requested` (generic print-job event) so
//     the printing module can subscribe to either name; this module
//     never calls a printing module directly.
//   - `curriculum.intervention.created`,
//     `curriculum.intervention.updated`,
//     `curriculum.intervention.deleted`,
//     `curriculum.intervention.completed`,
//     `curriculum.intervention.cancelled`
//   - `curriculum.grade_component_mapping.created`,
//     `curriculum.grade_component_mapping.deleted` —
//     admin-managed rows linking one academic grade component to
//     one basic competency (see the grade-mapping note below).
//   - `curriculum.settings.updated`
// Events this module listens to (canonical academic shapes —
// the academic module is the owner of students, enrollments, and
// grades; this module only caches references, never duplicates the
// records):
//   - `academic.student.created` with `{"student_id", "user_id",
//     "program_id", "application_id"}` — records the student
//     reference (`student_ref` IS the academic `student_id`) in a
//     local lookup cache. The legacy `{"student_ref", "level_id"}`
//     shape is still accepted.
//   - `academic.enrollment.approved` with `{"enrollment_id",
//     "student_id", "subject_id", "semester_id"}` — records the
//     enrollment-to-student link the grade handler needs to resolve
//     a grade's student.
//   - `academic.grade.recorded` with `{"grade_id",
//     "enrollment_id", "component_id", "score"}` — the grade is
//     resolved to a student through the cached enrollment link and
//     to a basic competency through the admin-managed
//     grade-component mapping table (see
//     `ComponentCompetencyMapping`); a grade with no enrollment
//     link or no component mapping is still recorded, marked
//     `unmapped` with a reason — never dropped silently, never
//     given a fabricated mapping. The legacy
//     `{"academic_grade_id", "student_ref", "basic_competency_id",
//     "score"}` shape is still accepted.
// Hook queries this module emits (same convergence point):
//   - `academic.student.lookup` with `{"student_ref"}` and expects
//     one result `{"exists": bool, ...}`. When no handler answers
//     (academic module absent) the reference is accepted; when a
//     handler answers `exists: false` the request is rejected. This
//     keeps the module usable standalone while still validating
//     against the academic module when it is present.
//
// GRADE-COMPONENT MAPPING (the grade → competency bridge). An
// academic grade names a grade component (`component_id`), not a
// curriculum competency. The link between the two is DATA, not
// code: administrators create rows through
// `POST /grade-component-mappings` (`academic_component_id` →
// `basic_competency_id`). There is no automatic or guessed mapping:
// a component with no row is unmapped, and the grade is recorded
// that way (see `on_academic_grade_recorded`).

//! Curriculum module — competency-based school curricula. — Module.
//!
//! Split from `src/modules/curriculum.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::assessment_domains::*;
use super::basic_competencies::*;
use super::constants::*;
use super::core_competencies::*;
use super::frameworks::*;
use super::grade_component_mappings::*;
use super::grade_descriptors::*;
use super::helpers::*;
use super::hooks::*;
use super::inputs::*;
use super::interventions::*;
use super::lesson_plans::*;
use super::levels::*;
use super::report_card_entries::*;
use super::report_cards::*;
use super::settings::*;
use super::state::*;
use super::structures::*;
use super::subjects::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai;
use actix_web::web;
use serde_json::Value;
use std::sync::Arc;

pub struct CurriculumModule {
    pub(super) state: CurriculumState,
}

impl CurriculumModule {
    pub fn new() -> Self {
        Self {
            state: CurriculumState::default(),
        }
    }
}

impl Default for CurriculumModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CurriculumModule {
    fn id(&self) -> &'static str {
        "curriculum"
    }

    fn name(&self) -> &'static str {
        "Curriculum"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }

    fn description(&self) -> &'static str {
        "Competency-based school curricula: framework registry (seeded with the 2013 national curriculum; new frameworks are data rows, no code change), school levels, subjects, hierarchical core and basic competencies versioned per curriculum year, curriculum structure with hours allocation, configurable three-domain assessment (knowledge, skills, attitudes with spiritual and social sub-domains as registry rows), grade descriptors per band, lesson plans, per-semester report cards with print events, and remedial / enrichment interventions. Students, courses, and grades stay owned by the academic module and are referenced only by opaque ids through events."
    }

    /// Curriculum is sold as a product: a customer buys a module
    /// entitlement for it (see `crate::modules::licensing`).
    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles"]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        ai::register_ai_capabilities("curriculum", AI_CAPABILITIES);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "academic.student.created",
            "curriculum",
            Arc::new(move |ctx: &mut HookContext| {
                on_academic_student(state.clone(), ctx)
            }),
        );
        // Canonical enrollment event: the enrollment-to-student
        // link the grade handler resolves through. The academic
        // module emits `academic.enrollment.approved` / `.dropped`
        // and never `academic.student.enrolled`, so no listener for
        // that name is registered here.
        let state = self.state.clone();
        bus.register(
            "academic.enrollment.approved",
            "curriculum",
            Arc::new(move |ctx: &mut HookContext| {
                on_academic_enrollment_approved(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "academic.grade.recorded",
            "curriculum",
            Arc::new(move |ctx: &mut HookContext| {
                on_academic_grade_recorded(state.clone(), ctx)
            }),
        );
        // Ground the `draft_lesson_plan` AI capability with the
        // named competencies, subject, and framework. Tenant
        // scoping: this store is in-memory with no tenant partition
        // yet; when batch 17 persistence lands, this handler must
        // only ground competencies of the calling tenant, keyed by
        // the caller company id in the payload.
        let state = self.state.clone();
        bus.register(
            "curriculum.ai.draft_lesson_plan_requested",
            "curriculum",
            Arc::new(move |ctx: &mut HookContext| {
                let input = ctx.get("input").cloned().unwrap_or(Value::Null);
                let core_id = input
                    .get("core_competency_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                let basic_id = input
                    .get("basic_competency_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.add_result(lesson_plan_grounding(&state, core_id, basic_id));
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/curriculum")
                .app_data(state)
                .route("/frameworks", web::get().to(list_frameworks))
                .route("/frameworks", web::post().to(create_framework))
                .route("/frameworks/{id}", web::get().to(get_framework))
                .route("/frameworks/{id}", web::patch().to(update_framework))
                .route("/frameworks/{id}", web::delete().to(delete_framework))
                .route("/frameworks/{id}/set-default", web::post().to(set_default_framework))
                .route("/levels", web::get().to(list_levels))
                .route("/levels", web::post().to(create_level))
                .route("/levels/{id}", web::get().to(get_level))
                .route("/levels/{id}", web::patch().to(update_level))
                .route("/levels/{id}", web::delete().to(delete_level))
                .route("/subjects", web::get().to(list_subjects))
                .route("/subjects", web::post().to(create_subject))
                .route("/subjects/{id}", web::get().to(get_subject))
                .route("/subjects/{id}", web::patch().to(update_subject))
                .route("/subjects/{id}", web::delete().to(delete_subject))
                .route("/core-competencies", web::get().to(list_core_competencies))
                .route("/core-competencies", web::post().to(create_core_competency))
                .route("/core-competencies/{id}", web::get().to(get_core_competency))
                .route("/core-competencies/{id}", web::patch().to(update_core_competency))
                .route("/core-competencies/{id}", web::delete().to(delete_core_competency))
                .route("/basic-competencies", web::get().to(list_basic_competencies))
                .route("/basic-competencies", web::post().to(create_basic_competency))
                .route("/basic-competencies/{id}", web::get().to(get_basic_competency))
                .route("/basic-competencies/{id}", web::patch().to(update_basic_competency))
                .route("/basic-competencies/{id}", web::delete().to(delete_basic_competency))
                .route("/structure", web::get().to(list_structures))
                .route("/structure", web::post().to(create_structure))
                .route("/structure/{id}", web::get().to(get_structure))
                .route("/structure/{id}", web::patch().to(update_structure))
                .route("/structure/{id}", web::delete().to(delete_structure))
                .route("/assessment-domains", web::get().to(list_assessment_domains))
                .route("/assessment-domains", web::post().to(create_assessment_domain))
                .route("/assessment-domains/{id}", web::get().to(get_assessment_domain))
                .route("/assessment-domains/{id}", web::patch().to(update_assessment_domain))
                .route("/assessment-domains/{id}", web::delete().to(delete_assessment_domain))
                .route("/grade-descriptors", web::get().to(list_grade_descriptors))
                .route("/grade-descriptors", web::post().to(create_grade_descriptor))
                .route("/grade-descriptors/{id}", web::get().to(get_grade_descriptor))
                .route("/grade-descriptors/{id}", web::patch().to(update_grade_descriptor))
                .route("/grade-descriptors/{id}", web::delete().to(delete_grade_descriptor))
                .route("/grade-component-mappings", web::get().to(list_component_mappings))
                .route("/grade-component-mappings", web::post().to(create_component_mapping))
                .route("/grade-component-mappings/{component_id}", web::delete().to(delete_component_mapping))
                .route("/lesson-plans", web::get().to(list_lesson_plans))
                .route("/lesson-plans", web::post().to(create_lesson_plan))
                .route("/lesson-plans/{id}", web::get().to(get_lesson_plan))
                .route("/lesson-plans/{id}", web::patch().to(update_lesson_plan))
                .route("/lesson-plans/{id}", web::delete().to(delete_lesson_plan))
                .route("/lesson-plans/{id}/publish", web::post().to(publish_lesson_plan))
                .route("/lesson-plans/{id}/archive", web::post().to(archive_lesson_plan))
                .route("/report-cards", web::get().to(list_report_cards))
                .route("/report-cards", web::post().to(create_report_card))
                .route("/report-cards/{id}", web::get().to(get_report_card))
                .route("/report-cards/{id}", web::patch().to(update_report_card))
                .route("/report-cards/{id}", web::delete().to(delete_report_card))
                .route("/report-cards/{id}/issue", web::post().to(issue_report_card))
                .route("/report-cards/{id}/print-request", web::post().to(print_report_card))
                .route("/report-cards/{id}/entries", web::get().to(list_report_card_entries))
                .route("/report-cards/{id}/entries", web::post().to(add_report_card_entry))
                .route("/report-cards/entries/{entry_id}", web::patch().to(update_report_card_entry))
                .route("/report-cards/entries/{entry_id}", web::delete().to(delete_report_card_entry))
                .route("/interventions", web::get().to(list_interventions))
                .route("/interventions", web::post().to(create_intervention))
                .route("/interventions/{id}", web::get().to(get_intervention))
                .route("/interventions/{id}", web::patch().to(update_intervention))
                .route("/interventions/{id}", web::delete().to(delete_intervention))
                .route("/interventions/{id}/complete", web::post().to(complete_intervention))
                .route("/interventions/{id}/cancel", web::post().to(cancel_intervention))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings))
                .route("/ai/{capability}", web::post().to(ai_capability)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/curriculum/frameworks", owner: "curriculum", description: "List curriculum frameworks (paginated, filter by code, country, is_active, is_built_in).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/frameworks", owner: "curriculum", description: "Create a curriculum framework (new frameworks are data rows; no code change).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/frameworks/{id}", owner: "curriculum", description: "Get one curriculum framework.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/frameworks/{id}", owner: "curriculum", description: "Update a curriculum framework.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/frameworks/{id}", owner: "curriculum", description: "Delete a curriculum framework (built-in and referenced frameworks are refused).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/frameworks/{id}/set-default", owner: "curriculum", description: "Make a framework the default framework.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/levels", owner: "curriculum", description: "List school levels (paginated, filter by framework_id, code).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/levels", owner: "curriculum", description: "Create a school level.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/levels/{id}", owner: "curriculum", description: "Get one school level.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/levels/{id}", owner: "curriculum", description: "Update a school level.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/levels/{id}", owner: "curriculum", description: "Delete a school level (referenced levels are refused).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/subjects", owner: "curriculum", description: "List subjects (paginated, filter by framework_id, level_id, code).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/subjects", owner: "curriculum", description: "Create a subject for a framework and level.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/subjects/{id}", owner: "curriculum", description: "Get one subject.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/subjects/{id}", owner: "curriculum", description: "Update a subject.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/subjects/{id}", owner: "curriculum", description: "Delete a subject (referenced subjects are refused).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/core-competencies", owner: "curriculum", description: "List core competencies (paginated, filter by framework_id, subject_id, level_id, code, grade_year, version).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/core-competencies", owner: "curriculum", description: "Create a core competency (versioned per curriculum year).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/core-competencies/{id}", owner: "curriculum", description: "Get one core competency.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/core-competencies/{id}", owner: "curriculum", description: "Update a core competency.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/core-competencies/{id}", owner: "curriculum", description: "Delete a core competency (referenced competencies are refused).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/basic-competencies", owner: "curriculum", description: "List basic competencies (paginated, filter by framework_id, core_competency_id, code, grade_year, version).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/basic-competencies", owner: "curriculum", description: "Create a basic competency under a core competency.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/basic-competencies/{id}", owner: "curriculum", description: "Get one basic competency.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/basic-competencies/{id}", owner: "curriculum", description: "Update a basic competency.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/basic-competencies/{id}", owner: "curriculum", description: "Delete a basic competency (referenced competencies are refused).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/structure", owner: "curriculum", description: "List curriculum structure rows (paginated, filter by framework_id, level_id, grade_year, subject_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/structure", owner: "curriculum", description: "Add a subject to a level and grade year with weekly hours.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/structure/{id}", owner: "curriculum", description: "Get one curriculum structure row.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/structure/{id}", owner: "curriculum", description: "Update hours or grade year of a structure row.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/structure/{id}", owner: "curriculum", description: "Remove a subject from a level and grade year.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/assessment-domains", owner: "curriculum", description: "List assessment domains (paginated, filter by code, parent_domain_id, is_active).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/assessment-domains", owner: "curriculum", description: "Create an assessment domain or sub-domain.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/assessment-domains/{id}", owner: "curriculum", description: "Get one assessment domain.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/assessment-domains/{id}", owner: "curriculum", description: "Update an assessment domain.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/assessment-domains/{id}", owner: "curriculum", description: "Delete an assessment domain (referenced domains are refused).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/grade-descriptors", owner: "curriculum", description: "List grade descriptors (paginated, filter by framework_id, domain_id, grade_label).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/grade-descriptors", owner: "curriculum", description: "Create a grade descriptor for a score band.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/grade-descriptors/{id}", owner: "curriculum", description: "Get one grade descriptor.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/grade-descriptors/{id}", owner: "curriculum", description: "Update a grade descriptor.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/grade-descriptors/{id}", owner: "curriculum", description: "Delete a grade descriptor.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/grade-component-mappings", owner: "curriculum", description: "List grade-component mappings (academic grade component id to basic competency id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/grade-component-mappings", owner: "curriculum", description: "Create a grade-component mapping (manager or teacher; the basic competency must exist).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/grade-component-mappings/{component_id}", owner: "curriculum", description: "Delete a grade-component mapping (manager or teacher; later grades for the component record as unmapped).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/lesson-plans", owner: "curriculum", description: "List lesson plans (paginated, filter by framework_id, subject_id, competency ids, grade_year, semester, teacher_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/lesson-plans", owner: "curriculum", description: "Create a lesson plan tied to competencies.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/lesson-plans/{id}", owner: "curriculum", description: "Get one lesson plan.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/lesson-plans/{id}", owner: "curriculum", description: "Update a lesson plan (manager or the plan teacher).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/lesson-plans/{id}", owner: "curriculum", description: "Delete a lesson plan (manager or the plan teacher).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/lesson-plans/{id}/publish", owner: "curriculum", description: "Publish a lesson plan.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/lesson-plans/{id}/archive", owner: "curriculum", description: "Archive a lesson plan.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/report-cards", owner: "curriculum", description: "List report cards (paginated, filter by student_ref, framework_id, level_id, grade_year, semester, academic_year, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/report-cards", owner: "curriculum", description: "Create a per-semester report card for a student.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/report-cards/{id}", owner: "curriculum", description: "Get one report card with its entries.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/report-cards/{id}", owner: "curriculum", description: "Update a report card (issued cards are locked).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/report-cards/{id}", owner: "curriculum", description: "Delete a draft report card and its entries (issued cards are refused).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/report-cards/{id}/issue", owner: "curriculum", description: "Issue a report card (locks its entries).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/report-cards/{id}/print-request", owner: "curriculum", description: "Request printing of a report card via print events (no direct printing-module call).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/report-cards/{id}/entries", owner: "curriculum", description: "List entries of a report card (paginated, filter by subject_id, domain_id, grade_label).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/report-cards/{id}/entries", owner: "curriculum", description: "Add a subject/domain entry to a report card (grade label and descriptor derived from data when not supplied).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/report-cards/entries/{entry_id}", owner: "curriculum", description: "Update a report-card entry.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/report-cards/entries/{entry_id}", owner: "curriculum", description: "Remove a report-card entry.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/interventions", owner: "curriculum", description: "List remedial and enrichment interventions (paginated, filter by student_ref, basic_competency_id, kind, status, teacher_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/interventions", owner: "curriculum", description: "Create a remedial or enrichment intervention for a student and competency.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/interventions/{id}", owner: "curriculum", description: "Get one intervention.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/interventions/{id}", owner: "curriculum", description: "Update an intervention.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/interventions/{id}", owner: "curriculum", description: "Delete an intervention.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/interventions/{id}/complete", owner: "curriculum", description: "Mark an intervention completed (sets completed_at).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/interventions/{id}/cancel", owner: "curriculum", description: "Cancel an intervention.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/curriculum/settings", owner: "curriculum", description: "Get curriculum settings (configurable defaults and vocabularies).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/settings", owner: "curriculum", description: "Update curriculum settings (defaults and vocabularies).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/curriculum/ai/{capability}", owner: "curriculum", description: "Run one registered curriculum AI capability (draft_lesson_plan; requires ai:use plus curriculum:view).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CurriculumModule::new()));
}
