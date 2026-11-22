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
//     a grade's student. The `academic.student.enrolled` name is
//     still subscribed to (another module may emit it), but the
//     academic module never does.
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

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, Pagination, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_CODE_LEN: usize = 64;
const MAX_NAME_LEN: usize = 200;
const MAX_TITLE_LEN: usize = 300;
const MAX_TEXT_LEN: usize = 5000;
const MAX_REF_LEN: usize = 128;

/// Scores are standard 0..=100 values. This is a standard constant,
/// not a business rule.
const MIN_SCORE: f64 = 0.0;
const MAX_SCORE: f64 = 100.0;

/// Capabilities checked through the `role.check` hook (fail closed).
const PERM_CURRICULUM_VIEW: &str = "curriculum:view";
const PERM_CURRICULUM_MANAGE: &str = "curriculum:manage";
const PERM_CURRICULUM_TEACH: &str = "curriculum:teach";

/// Seeded default framework code. CONFIGURABLE DEFAULT: the admin
/// can change it with `POST /frameworks/{id}/set-default`; it only
/// picks which seeded framework row new records default to.
const DEFAULT_FRAMEWORK_CODE: &str = "curriculum-2013";
/// Seeded default paper size for print requests. CONFIGURABLE
/// DEFAULT: editable through `PATCH /settings` (`paper_sizes` list
/// plus `default_paper_size`).
const DEFAULT_PAPER_SIZE: &str = "a4";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One curriculum framework (the registry). A newer national
/// curriculum is added as another row here — no code change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurriculumFramework {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub description: String,
    pub country: String,
    pub is_active: bool,
    pub is_built_in: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One school level (elementary, junior-high, senior-high, ...).
/// Levels are data rows, not an enum: a school adds its own levels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchoolLevel {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub code: String,
    pub name: String,
    pub grade_count: u32,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One subject taught under a framework at a level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subject {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub level_id: Uuid,
    pub code: String,
    pub name: String,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A core competency: the top of the competency hierarchy
/// (subject -> core competency -> basic competency).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreCompetency {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub subject_id: Uuid,
    pub level_id: Uuid,
    pub code: String,
    pub title: String,
    pub description: String,
    pub grade_year: u32,
    /// Curriculum year this version of the competency belongs to
    /// (for example 2013). Versioning is per curriculum year.
    pub version: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A basic competency: a child of one core competency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasicCompetency {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub core_competency_id: Uuid,
    pub code: String,
    pub title: String,
    pub description: String,
    pub grade_year: u32,
    pub version: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Curriculum structure: which subjects a level and grade year
/// teaches, and how many hours per week each subject gets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurriculumStructure {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub level_id: Uuid,
    pub grade_year: u32,
    pub subject_id: Uuid,
    pub hours_per_week: f64,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One assessment domain from the configurable domain registry.
/// Top-level domains are knowledge, skills, and attitudes; the
/// attitudes domain has sub-domains (spiritual and social) stored
/// as rows whose `parent_domain_id` points at the attitudes row.
/// No domain name is hard-coded in logic anywhere in this file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssessmentDomain {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub parent_domain_id: Option<Uuid>,
    pub description: String,
    pub sort_order: i32,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Descriptor text for one grade band, optionally scoped to one
/// assessment domain. Descriptors are data, not code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradeDescriptor {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub domain_id: Option<Uuid>,
    pub min_score: f64,
    pub max_score: f64,
    pub grade_label: String,
    pub descriptor: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One structured lesson plan, tied to competencies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LessonPlan {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub subject_id: Uuid,
    pub core_competency_id: Option<Uuid>,
    pub basic_competency_id: Option<Uuid>,
    pub title: String,
    pub objectives: Vec<String>,
    pub materials: Vec<String>,
    pub activities: Vec<String>,
    pub assessment_notes: String,
    pub time_minutes: u32,
    pub grade_year: u32,
    pub semester: u32,
    pub teacher_id: String,
    /// Status comes from the settings vocabulary
    /// (`lesson_plan_statuses`), never from a hard-coded enum.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One per-semester report card for one student.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportCard {
    pub id: Uuid,
    pub student_ref: String,
    pub framework_id: Uuid,
    pub level_id: Uuid,
    pub grade_year: u32,
    pub semester: u32,
    pub academic_year: String,
    /// Status comes from the settings vocabulary
    /// (`report_card_statuses`), never from a hard-coded enum.
    pub status: String,
    pub issued_at: Option<DateTime<Utc>>,
    pub issued_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One subject/domain line on a report card. `score` is the numeric
/// result; `academic_grade_id` links (never copies) the grade record
/// owned by the academic module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportCardEntry {
    pub id: Uuid,
    pub report_card_id: Uuid,
    pub subject_id: Uuid,
    pub domain_id: Uuid,
    pub score: Option<f64>,
    pub grade_label: Option<String>,
    pub descriptor: Option<String>,
    pub notes: String,
    pub academic_grade_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One remedial or enrichment intervention for one student and one
/// basic competency. `kind` and `status` come from the settings
/// vocabularies (`intervention_kinds`, `intervention_statuses`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Intervention {
    pub id: Uuid,
    pub student_ref: String,
    pub basic_competency_id: Uuid,
    pub kind: String,
    pub status: String,
    pub teacher_id: String,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A student reference seen through academic-module events. This is
/// a lookup cache only — students are owned by the academic module
/// and are never duplicated here. `student_ref` IS the academic
/// `student_id` (the canonical identity key); `program_id` is the
/// academic program the student belongs to, when the event carried
/// it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownStudent {
    pub student_ref: String,
    pub level_id: Option<String>,
    #[serde(default)]
    pub program_id: Option<String>,
    pub source_event: String,
    pub seen_at: DateTime<Utc>,
}

/// An enrollment link seen through `academic.enrollment.approved`.
/// This is the bridge a grade needs: a grade event carries only an
/// `enrollment_id`, and this cache resolves it to the student (and
/// subject) without duplicating enrollment records here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownEnrollment {
    pub enrollment_id: String,
    pub student_ref: String,
    pub subject_id: Option<String>,
    pub semester_id: Option<String>,
    pub seen_at: DateTime<Utc>,
}

/// One admin-managed grade-component mapping: the bridge between an
/// academic grade component and a curriculum basic competency. The
/// mapping is DATA (created through the API), never guessed: a
/// component with no row here is unmapped, and grades for it are
/// recorded as unmapped rather than linked to a competency they do
/// not belong to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentCompetencyMapping {
    pub academic_component_id: String,
    pub basic_competency_id: String,
    pub created_at: DateTime<Utc>,
}

/// A grade reference seen through academic-module events. The grade
/// record itself stays in the academic module; only the link context
/// the event carried is kept here, so report-card entries can link
/// to it. `mapping_status` is `"mapped"` when the grade resolved to
/// a basic competency and `"unmapped"` otherwise; `mapping_reason`
/// explains an unmapped grade (unknown enrollment, unmapped
/// component, or a mapping pointing at a deleted competency).
/// Unmapped grades are kept, never dropped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownGrade {
    pub academic_grade_id: String,
    pub student_ref: String,
    pub basic_competency_id: Option<String>,
    #[serde(default)]
    pub enrollment_id: Option<String>,
    #[serde(default)]
    pub component_id: Option<String>,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default = "default_mapping_status")]
    pub mapping_status: String,
    #[serde(default)]
    pub mapping_reason: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

fn default_mapping_status() -> String {
    "unmapped".to_string()
}

/// One configurable grade band (min/max/label). Bands are settings
/// data; the grade-descriptor rows carry the descriptive text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradeBand {
    pub min_score: f64,
    pub max_score: f64,
    pub label: String,
}

/// Admin-editable settings. Every list here is a configurable
/// default / vocabulary — change it through `PATCH /settings` and
/// the whole module follows, with no code change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurriculumSettings {
    pub default_framework_code: String,
    pub default_paper_size: String,
    pub paper_sizes: Vec<String>,
    pub semesters: Vec<u32>,
    pub intervention_kinds: Vec<String>,
    pub lesson_plan_statuses: Vec<String>,
    pub report_card_statuses: Vec<String>,
    pub intervention_statuses: Vec<String>,
    pub default_grade_bands: Vec<GradeBand>,
}

impl Default for CurriculumSettings {
    fn default() -> Self {
        Self {
            // CONFIGURABLE DEFAULTS (seeded values only).
            default_framework_code: DEFAULT_FRAMEWORK_CODE.to_string(),
            default_paper_size: DEFAULT_PAPER_SIZE.to_string(),
            paper_sizes: vec![
                "a4".to_string(),
                "letter".to_string(),
                "legal".to_string(),
            ],
            semesters: vec![1, 2],
            intervention_kinds: vec!["remedial".to_string(), "enrichment".to_string()],
            lesson_plan_statuses: vec![
                "draft".to_string(),
                "published".to_string(),
                "archived".to_string(),
            ],
            report_card_statuses: vec![
                "draft".to_string(),
                "issued".to_string(),
                "archived".to_string(),
            ],
            intervention_statuses: vec![
                "scheduled".to_string(),
                "in_progress".to_string(),
                "completed".to_string(),
                "cancelled".to_string(),
            ],
            default_grade_bands: vec![
                GradeBand { min_score: 90.0, max_score: 100.0, label: "A".to_string() },
                GradeBand { min_score: 80.0, max_score: 89.99, label: "B".to_string() },
                GradeBand { min_score: 70.0, max_score: 79.99, label: "C".to_string() },
                GradeBand { min_score: 60.0, max_score: 69.99, label: "D".to_string() },
                GradeBand { min_score: 0.0, max_score: 59.99, label: "E".to_string() },
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// Input shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct FrameworkInput {
    code: Option<String>,
    name: Option<String>,
    description: Option<String>,
    country: Option<String>,
    is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct LevelInput {
    framework_id: Option<String>,
    code: Option<String>,
    name: Option<String>,
    grade_count: Option<u32>,
    sort_order: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct SubjectInput {
    framework_id: Option<String>,
    level_id: Option<String>,
    code: Option<String>,
    name: Option<String>,
    sort_order: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct CoreCompetencyInput {
    framework_id: Option<String>,
    subject_id: Option<String>,
    level_id: Option<String>,
    code: Option<String>,
    title: Option<String>,
    description: Option<String>,
    grade_year: Option<u32>,
    version: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct BasicCompetencyInput {
    framework_id: Option<String>,
    core_competency_id: Option<String>,
    code: Option<String>,
    title: Option<String>,
    description: Option<String>,
    grade_year: Option<u32>,
    version: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct StructureInput {
    framework_id: Option<String>,
    level_id: Option<String>,
    grade_year: Option<u32>,
    subject_id: Option<String>,
    hours_per_week: Option<f64>,
    sort_order: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct AssessmentDomainInput {
    code: Option<String>,
    name: Option<String>,
    parent_domain_id: Option<String>,
    description: Option<String>,
    sort_order: Option<i32>,
    is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct GradeDescriptorInput {
    framework_id: Option<String>,
    domain_id: Option<String>,
    min_score: Option<f64>,
    max_score: Option<f64>,
    grade_label: Option<String>,
    descriptor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LessonPlanInput {
    framework_id: Option<String>,
    subject_id: Option<String>,
    core_competency_id: Option<String>,
    basic_competency_id: Option<String>,
    title: Option<String>,
    objectives: Option<Vec<String>>,
    materials: Option<Vec<String>>,
    activities: Option<Vec<String>>,
    assessment_notes: Option<String>,
    time_minutes: Option<u32>,
    grade_year: Option<u32>,
    semester: Option<u32>,
    teacher_id: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReportCardInput {
    student_ref: Option<String>,
    framework_id: Option<String>,
    level_id: Option<String>,
    grade_year: Option<u32>,
    semester: Option<u32>,
    academic_year: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReportCardEntryInput {
    subject_id: Option<String>,
    domain_id: Option<String>,
    score: Option<f64>,
    grade_label: Option<String>,
    descriptor: Option<String>,
    notes: Option<String>,
    academic_grade_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ComponentMappingInput {
    academic_component_id: Option<String>,
    basic_competency_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InterventionInput {
    student_ref: Option<String>,
    basic_competency_id: Option<String>,
    kind: Option<String>,
    status: Option<String>,
    teacher_id: Option<String>,
    scheduled_at: Option<String>,
    notes: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SettingsInput {
    default_framework_code: Option<String>,
    default_paper_size: Option<String>,
    paper_sizes: Option<Vec<String>>,
    semesters: Option<Vec<u32>>,
    intervention_kinds: Option<Vec<String>>,
    lesson_plan_statuses: Option<Vec<String>>,
    report_card_statuses: Option<Vec<String>>,
    intervention_statuses: Option<Vec<String>>,
    default_grade_bands: Option<Vec<GradeBand>>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct CurriculumState {
    pub frameworks: Arc<DashMap<Uuid, CurriculumFramework>>,
    pub levels: Arc<DashMap<Uuid, SchoolLevel>>,
    pub subjects: Arc<DashMap<Uuid, Subject>>,
    pub core_competencies: Arc<DashMap<Uuid, CoreCompetency>>,
    pub basic_competencies: Arc<DashMap<Uuid, BasicCompetency>>,
    pub structures: Arc<DashMap<Uuid, CurriculumStructure>>,
    pub assessment_domains: Arc<DashMap<Uuid, AssessmentDomain>>,
    pub grade_descriptors: Arc<DashMap<Uuid, GradeDescriptor>>,
    pub lesson_plans: Arc<DashMap<Uuid, LessonPlan>>,
    pub report_cards: Arc<DashMap<Uuid, ReportCard>>,
    pub report_card_entries: Arc<DashMap<Uuid, ReportCardEntry>>,
    pub interventions: Arc<DashMap<Uuid, Intervention>>,
    pub known_students: Arc<DashMap<String, KnownStudent>>,
    pub known_enrollments: Arc<DashMap<String, KnownEnrollment>>,
    pub known_grades: Arc<DashMap<String, KnownGrade>>,
    pub component_mappings: Arc<DashMap<String, ComponentCompetencyMapping>>,
    pub settings: Arc<RwLock<CurriculumSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for CurriculumState {
    fn default() -> Self {
        Self {
            frameworks: Arc::new(DashMap::new()),
            levels: Arc::new(DashMap::new()),
            subjects: Arc::new(DashMap::new()),
            core_competencies: Arc::new(DashMap::new()),
            basic_competencies: Arc::new(DashMap::new()),
            structures: Arc::new(DashMap::new()),
            assessment_domains: Arc::new(DashMap::new()),
            grade_descriptors: Arc::new(DashMap::new()),
            lesson_plans: Arc::new(DashMap::new()),
            report_cards: Arc::new(DashMap::new()),
            report_card_entries: Arc::new(DashMap::new()),
            interventions: Arc::new(DashMap::new()),
            known_students: Arc::new(DashMap::new()),
            known_enrollments: Arc::new(DashMap::new()),
            known_grades: Arc::new(DashMap::new()),
            component_mappings: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(CurriculumSettings::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl CurriculumState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn settings(&self) -> CurriculumSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the e-learning module.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// True when the caller may manage curriculum content: the manage
/// capability or the teacher capability.
fn can_teach(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_CURRICULUM_MANAGE)
        || check_capability(ctx, role, PERM_CURRICULUM_TEACH)
}

fn require_teach(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    if can_teach(ctx, role) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{PERM_CURRICULUM_MANAGE}\" or \"{PERM_CURRICULUM_TEACH}\""
        )))
    }
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

/// A machine code: lowercase letters, digits, and hyphens only.
fn require_code(value: &str, field: &str) -> UnifiedResult<String> {
    let code = require_text(value, field, MAX_CODE_LEN)?;
    if !code
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(UnifiedError::bad_request(format!(
            "{field} must use lowercase letters, digits, hyphens, or underscores"
        )));
    }
    Ok(code)
}

/// Validate a value against a settings list. The list is the live
/// admin-configured one — never a constant in this file.
fn require_in_list(value: &str, field: &str, allowed: &[String]) -> UnifiedResult<String> {
    let checked = require_text(value, field, MAX_CODE_LEN)?;
    if !allowed.iter().any(|item| item == &checked) {
        return Err(UnifiedError::bad_request(format!(
            "{field} \"{checked}\" is not allowed; allowed values: {}",
            allowed.join(", ")
        )));
    }
    Ok(checked)
}

fn require_score(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < MIN_SCORE || value > MAX_SCORE {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between {MIN_SCORE} and {MAX_SCORE}"
        )));
    }
    Ok(value)
}

fn require_string_list(values: &[String], field: &str, max_item: usize) -> UnifiedResult<Vec<String>> {
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        out.push(require_text(value, field, max_item)?);
    }
    Ok(out)
}

fn optional_uuid(raw: &Option<String>, what: &str) -> UnifiedResult<Option<Uuid>> {
    match raw {
        Some(value) if !value.trim().is_empty() => Ok(Some(parse_uuid(value.trim(), what)?)),
        _ => Ok(None),
    }
}

fn required_uuid(raw: &Option<String>, what: &str) -> UnifiedResult<Uuid> {
    match raw {
        Some(value) => parse_uuid(value.trim(), what),
        None => Err(UnifiedError::bad_request(format!("{what} id is required"))),
    }
}

/// Shared list behaviour: free-text search plus exact-match filters
/// from the query, sorted by creation time, paginated.
trait Listable {
    fn search_text(&self) -> String;
    fn filter_value(&self, key: &str) -> Option<String>;
    fn created_at(&self) -> DateTime<Utc>;
}

fn list_response<T>(items: Vec<T>, params: &PaginationParams) -> Value
where
    T: Listable + Clone + Serialize,
{
    let pagination: Pagination = params.normalized();
    let search = pagination
        .search
        .clone()
        .unwrap_or_default()
        .to_lowercase();
    let mut items: Vec<T> = items
        .into_iter()
        .filter(|item| {
            if !search.is_empty() && !item.search_text().to_lowercase().contains(&search) {
                return false;
            }
            pagination.filters.iter().all(|(key, value)| {
                item.filter_value(key).is_some_and(|found| &found == value)
            })
        })
        .collect();
    items.sort_by(|a, b| {
        let ord = a.created_at().cmp(&b.created_at());
        if pagination.order == "desc" {
            ord.reverse()
        } else {
            ord
        }
    });
    let total = items.len() as u64;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(pagination.offset() as usize)
        .take(pagination.limit() as usize)
        .collect();
    let page = PageResult::new(page_items, total, &pagination);
    json!(page)
}

fn get_record<T: Clone>(map: &DashMap<Uuid, T>, id: &Uuid, what: &str) -> UnifiedResult<T> {
    map.get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("{what} not found: {id}")))
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

/// Validate a student reference against the academic module when it
/// is installed. The lookup is a hook query: when no handler answers
/// (academic module absent) the reference is accepted; when a
/// handler answers `exists: false` the request is rejected.
///
/// The answer is read from the hook RESULTS (the tree's hook-query
/// convention — see the logistics module's `lookup_order`), with a
/// fallback to the payload `exists` key for answerers that set it
/// there instead. Reading the payload alone is wrong: the emitted
/// payload is the request, not the answer. A confirmed student is
/// cached in `known_students` so repeat validations stay local.
fn check_student_reference(ctx: &AppContext, state: &CurriculumState, student_ref: &str) -> UnifiedResult<()> {
    if state.known_students.contains_key(student_ref) {
        return Ok(());
    }
    if let Some(context) = ctx.hooks.emit_if_registered(
        "academic.student.lookup",
        json!({ "student_ref": student_ref }),
    ) {
        let answered = context
            .results
            .iter()
            .find_map(|result| {
                result
                    .get("exists")
                    .or_else(|| result.get("found"))
                    .and_then(|value| value.as_bool())
            })
            .or_else(|| context.get("exists").and_then(|value| value.as_bool()));
        match answered {
            Some(true) => {
                let program_id = context
                    .results
                    .iter()
                    .find_map(|result| {
                        result
                            .get("program_id")
                            .and_then(|value| value.as_str())
                            .map(str::to_string)
                    });
                state.known_students.insert(
                    student_ref.to_string(),
                    KnownStudent {
                        student_ref: student_ref.to_string(),
                        level_id: None,
                        program_id,
                        source_event: "academic.student.lookup".to_string(),
                        seen_at: now(),
                    },
                );
                return Ok(());
            }
            Some(false) => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown student reference: {student_ref}"
                )));
            }
            None => {
                // A registered handler answered with no usable
                // `exists` value: treat it as no answer (standalone
                // tolerance) rather than rejecting a reference the
                // academic module did not actually deny.
            }
        }
    }
    Ok(())
}

/// Find the grade band label for a score from the configured bands.
fn grade_label_for(settings: &CurriculumSettings, score: f64) -> Option<String> {
    settings
        .default_grade_bands
        .iter()
        .find(|band| score >= band.min_score && score <= band.max_score)
        .map(|band| band.label.clone())
}

/// Find the stored descriptor text for a score, optionally scoped to
/// one assessment domain; falls back to the unscoped descriptor.
fn descriptor_for(
    state: &CurriculumState,
    framework_id: Uuid,
    domain_id: Option<Uuid>,
    score: f64,
) -> Option<String> {
    let mut best: Option<String> = None;
    for entry in state.grade_descriptors.iter() {
        let row = entry.value();
        if row.framework_id != framework_id || score < row.min_score || score > row.max_score {
            continue;
        }
        if row.domain_id == domain_id {
            return Some(row.descriptor.clone());
        }
        if row.domain_id.is_none() {
            best = Some(row.descriptor.clone());
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Hook handlers (academic module events)
// ---------------------------------------------------------------------------

/// Handle `academic.student.created` (canonical payload
/// `{student_id, user_id, program_id, application_id}`) and the
/// legacy `academic.student.enrolled` shape (`{student_ref,
/// level_id, ...}`). The canonical identity key is the academic
/// `student_id`, stored here as `student_ref`; `level_id` is kept
/// when the event carries one (the academic module has no level
/// concept, so the canonical event never does). Only the reference
/// is cached — students stay owned by the academic module.
fn on_academic_student(state: CurriculumState, ctx: &mut HookContext) {
    let Some(student_ref) = ctx
        .get("student_id")
        .or_else(|| ctx.get("student_ref"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let level_id = ctx
        .get("level_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let program_id = ctx
        .get("program_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    state.known_students.insert(
        student_ref.clone(),
        KnownStudent {
            student_ref,
            level_id,
            program_id,
            source_event: ctx.event.clone(),
            seen_at: now(),
        },
    );
}

/// Handle `academic.enrollment.approved` (`{enrollment_id,
/// student_id, subject_id, semester_id}`). Records the
/// enrollment-to-student link the grade handler resolves through:
/// a grade event carries only an `enrollment_id`, so without this
/// cache the grade's student is unknowable here. The student is
/// also recorded in the lookup cache. A dropped enrollment is not
/// removed: the cache records that the student was seen, and
/// report cards for past semesters must keep validating.
fn on_academic_enrollment_approved(state: CurriculumState, ctx: &mut HookContext) {
    let (Some(enrollment_id), Some(student_ref)) = (
        ctx.get("enrollment_id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .filter(|value| !value.is_empty()),
        ctx.get("student_id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .filter(|value| !value.is_empty()),
    ) else {
        return;
    };
    let subject_id = ctx
        .get("subject_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let semester_id = ctx
        .get("semester_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    state.known_enrollments.insert(
        enrollment_id.clone(),
        KnownEnrollment {
            enrollment_id,
            student_ref: student_ref.clone(),
            subject_id,
            semester_id,
            seen_at: now(),
        },
    );
    // Record the student only when the lookup cache does not
    // already hold them: `academic.student.created` carries the
    // program id, and this event does not — overwriting would drop
    // it.
    if !state.known_students.contains_key(&student_ref) {
        state.known_students.insert(
            student_ref.clone(),
            KnownStudent {
                student_ref,
                level_id: None,
                program_id: None,
                source_event: ctx.event.clone(),
                seen_at: now(),
            },
        );
    }
}

/// Resolve one academic grade component to a basic competency
/// through the admin-managed mapping table. Returns the competency
/// id when a mapping row exists AND the competency still exists in
/// this module's store; otherwise `None` (a mapping that points at
/// a deleted competency resolves to nothing — never to a guessed
/// replacement).
fn competency_for_component(state: &CurriculumState, component_id: &str) -> Option<String> {
    let mapping = state.component_mappings.get(component_id)?;
    let competency_id = mapping.value().basic_competency_id.clone();
    let parsed = Uuid::parse_str(&competency_id).ok()?;
    if state.basic_competencies.contains_key(&parsed) {
        Some(competency_id)
    } else {
        None
    }
}

/// Handle `academic.grade.recorded`.
///
/// Canonical payload `{grade_id, enrollment_id, component_id,
/// score}` — resolved in two steps, both inside this module:
///   1. enrollment → student: `enrollment_id` is looked up in the
///      cache filled by `academic.enrollment.approved`. When the
///      enrollment was never seen (approved before this module
///      registered, or the approval event was missed), the student
///      is unknown and the grade is recorded unmapped with reason
///      `unknown_enrollment` — the student is never guessed.
///   2. component → competency: `component_id` is looked up in the
///      admin-managed grade-component mapping table. When no row
///      exists (or the row points at a deleted competency), the
///      grade is recorded unmapped with reason
///      `unmapped_component` — a mapping is never fabricated.
///
/// Legacy payload `{academic_grade_id, student_ref,
/// basic_competency_id, score}` is still accepted: the student and
/// competency arrive with the event, so the grade records as
/// `mapped` when a competency is given (and exists) and `unmapped`
/// otherwise.
///
/// Every grade that carries an id is recorded, mapped or not: a
/// grade is never dropped silently. The score is kept only as link
/// context; the grade record itself stays in the academic module.
fn on_academic_grade_recorded(state: CurriculumState, ctx: &mut HookContext) {
    let Some(grade_id) = ctx
        .get("grade_id")
        .or_else(|| ctx.get("academic_grade_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let score = ctx
        .get("score")
        .and_then(|value| value.as_f64())
        .filter(|value| value.is_finite());
    // Legacy shape: student and competency arrive with the event.
    if let Some(student_ref) = ctx
        .get("student_ref")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|value| !value.is_empty())
    {
        let raw_competency = ctx
            .get("basic_competency_id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .filter(|value| !value.is_empty());
        let competency = raw_competency.clone().filter(|value| {
            Uuid::parse_str(value.as_str())
                .is_ok_and(|id| state.basic_competencies.contains_key(&id))
        });
        let (mapping_status, mapping_reason) = match (&competency, &raw_competency) {
            (Some(_), _) => ("mapped".to_string(), None),
            (None, Some(_)) => (
                "unmapped".to_string(),
                Some("unknown_basic_competency".to_string()),
            ),
            (None, None) => (
                "unmapped".to_string(),
                Some("no_basic_competency_in_event".to_string()),
            ),
        };
        state.known_grades.insert(
            grade_id.clone(),
            KnownGrade {
                academic_grade_id: grade_id,
                student_ref,
                basic_competency_id: competency,
                enrollment_id: ctx
                    .get("enrollment_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                component_id: ctx
                    .get("component_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                score,
                mapping_status,
                mapping_reason,
                recorded_at: now(),
            },
        );
        return;
    }
    // Canonical shape: resolve student via the enrollment cache,
    // competency via the mapping table.
    let enrollment_id = ctx
        .get("enrollment_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|value| !value.is_empty());
    let component_id = ctx
        .get("component_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|value| !value.is_empty());
    let enrollment = enrollment_id
        .as_ref()
        .and_then(|id| state.known_enrollments.get(id))
        .map(|entry| entry.value().clone());
    let (student_ref, student_reason) = match &enrollment {
        Some(link) => (link.student_ref.clone(), None),
        None => (
            String::new(),
            Some("unknown_enrollment".to_string()),
        ),
    };
    let (competency, competency_reason) = match &component_id {
        Some(component) => match competency_for_component(&state, component) {
            Some(id) => (Some(id), None),
            None => (
                None,
                Some("unmapped_component".to_string()),
            ),
        },
        None => (
            None,
            Some("missing_component_id".to_string()),
        ),
    };
    let mapping_reason = student_reason.or(competency_reason);
    let mapping_status = if mapping_reason.is_none() && competency.is_some() {
        "mapped".to_string()
    } else {
        "unmapped".to_string()
    };
    state.known_grades.insert(
        grade_id.clone(),
        KnownGrade {
            academic_grade_id: grade_id,
            student_ref,
            basic_competency_id: competency,
            enrollment_id,
            component_id,
            score,
            mapping_status,
            mapping_reason,
            recorded_at: now(),
        },
    );
}

// ---------------------------------------------------------------------------
// Listable implementations (search text + exact-match filters)
// ---------------------------------------------------------------------------

macro_rules! impl_listable {
    ($t:ty, search: [$($search_expr:expr),*], filters: { $($key:literal => $filter_expr:expr),* }) => {
        impl Listable for $t {
            fn search_text(&self) -> String {
                vec![$($search_expr),*].join(" ")
            }
            fn filter_value(&self, key: &str) -> Option<String> {
                match key {
                    $($key => $filter_expr,)*
                    _ => None,
                }
            }
            fn created_at(&self) -> DateTime<Utc> {
                self.created_at
            }
        }
    };
}

impl_listable!(CurriculumFramework, search: [self.code.clone(), self.name.clone(), self.description.clone()], filters: {
    "code" => Some(self.code.clone()),
    "country" => Some(self.country.clone()),
    "is_active" => Some(self.is_active.to_string()),
    "is_built_in" => Some(self.is_built_in.to_string())
});

impl_listable!(SchoolLevel, search: [self.code.clone(), self.name.clone()], filters: {
    "framework_id" => Some(self.framework_id.to_string()),
    "code" => Some(self.code.clone())
});

impl_listable!(Subject, search: [self.code.clone(), self.name.clone()], filters: {
    "framework_id" => Some(self.framework_id.to_string()),
    "level_id" => Some(self.level_id.to_string()),
    "code" => Some(self.code.clone())
});

impl_listable!(CoreCompetency, search: [self.code.clone(), self.title.clone(), self.description.clone()], filters: {
    "framework_id" => Some(self.framework_id.to_string()),
    "subject_id" => Some(self.subject_id.to_string()),
    "level_id" => Some(self.level_id.to_string()),
    "code" => Some(self.code.clone()),
    "grade_year" => Some(self.grade_year.to_string()),
    "version" => Some(self.version.to_string())
});

impl_listable!(BasicCompetency, search: [self.code.clone(), self.title.clone(), self.description.clone()], filters: {
    "framework_id" => Some(self.framework_id.to_string()),
    "core_competency_id" => Some(self.core_competency_id.to_string()),
    "code" => Some(self.code.clone()),
    "grade_year" => Some(self.grade_year.to_string()),
    "version" => Some(self.version.to_string())
});

impl_listable!(CurriculumStructure, search: [self.framework_id.to_string(), self.subject_id.to_string()], filters: {
    "framework_id" => Some(self.framework_id.to_string()),
    "level_id" => Some(self.level_id.to_string()),
    "grade_year" => Some(self.grade_year.to_string()),
    "subject_id" => Some(self.subject_id.to_string())
});

impl_listable!(AssessmentDomain, search: [self.code.clone(), self.name.clone(), self.description.clone()], filters: {
    "code" => Some(self.code.clone()),
    "parent_domain_id" => self.parent_domain_id.map(|value| value.to_string()),
    "is_active" => Some(self.is_active.to_string())
});

impl_listable!(GradeDescriptor, search: [self.grade_label.clone(), self.descriptor.clone()], filters: {
    "framework_id" => Some(self.framework_id.to_string()),
    "domain_id" => self.domain_id.map(|value| value.to_string()),
    "grade_label" => Some(self.grade_label.clone())
});

impl_listable!(LessonPlan, search: [self.title.clone(), self.assessment_notes.clone()], filters: {
    "framework_id" => Some(self.framework_id.to_string()),
    "subject_id" => Some(self.subject_id.to_string()),
    "core_competency_id" => self.core_competency_id.map(|value| value.to_string()),
    "basic_competency_id" => self.basic_competency_id.map(|value| value.to_string()),
    "grade_year" => Some(self.grade_year.to_string()),
    "semester" => Some(self.semester.to_string()),
    "teacher_id" => Some(self.teacher_id.clone()),
    "status" => Some(self.status.clone())
});

impl_listable!(ReportCard, search: [self.student_ref.clone(), self.academic_year.clone()], filters: {
    "student_ref" => Some(self.student_ref.clone()),
    "framework_id" => Some(self.framework_id.to_string()),
    "level_id" => Some(self.level_id.to_string()),
    "grade_year" => Some(self.grade_year.to_string()),
    "semester" => Some(self.semester.to_string()),
    "academic_year" => Some(self.academic_year.clone()),
    "status" => Some(self.status.clone())
});

impl_listable!(ReportCardEntry, search: [self.grade_label.clone().unwrap_or_default(), self.notes.clone()], filters: {
    "report_card_id" => Some(self.report_card_id.to_string()),
    "subject_id" => Some(self.subject_id.to_string()),
    "domain_id" => Some(self.domain_id.to_string()),
    "grade_label" => self.grade_label.clone(),
    "academic_grade_id" => self.academic_grade_id.clone()
});

impl_listable!(Intervention, search: [self.student_ref.clone(), self.kind.clone(), self.notes.clone()], filters: {
    "student_ref" => Some(self.student_ref.clone()),
    "basic_competency_id" => Some(self.basic_competency_id.to_string()),
    "kind" => Some(self.kind.clone()),
    "status" => Some(self.status.clone()),
    "teacher_id" => Some(self.teacher_id.clone())
});

// ---------------------------------------------------------------------------
// Framework handlers
// ---------------------------------------------------------------------------

async fn list_frameworks(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let items: Vec<CurriculumFramework> =
            state.frameworks.iter().map(|entry| entry.value().clone()).collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

async fn create_framework(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<FrameworkInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let input = body.into_inner();
        let code = require_code(
            input.code.as_deref().unwrap_or(""),
            "code",
        )?;
        if state.frameworks.iter().any(|entry| entry.value().code == code) {
            return Err(UnifiedError::conflict(format!(
                "A framework with code \"{code}\" already exists"
            )));
        }
        let name = require_text(input.name.as_deref().unwrap_or(""), "name", MAX_NAME_LEN)?;
        let description = optional_text(&input.description, "description", MAX_TEXT_LEN)?
            .unwrap_or_default();
        let country =
            optional_text(&input.country, "country", MAX_NAME_LEN)?.unwrap_or_default();
        let framework = CurriculumFramework {
            id: Uuid::new_v4(),
            code: code.clone(),
            name,
            description,
            country,
            is_active: input.is_active.unwrap_or(true),
            is_built_in: false,
            created_at: now(),
            updated_at: now(),
        };
        state.frameworks.insert(framework.id, framework.clone());
        state.emit("curriculum.framework.created", json!({ "framework": framework }));
        Ok(json!(framework))
    })();
    json::respond(result, debug)
}

async fn get_framework(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "framework")?;
        Ok(json!(get_record(&state.frameworks, &id, "Framework")?))
    })();
    json::respond(result, debug)
}

async fn update_framework(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FrameworkInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "framework")?;
        let input = body.into_inner();
        let mut framework = get_record(&state.frameworks, &id, "Framework")?;
        if let Some(raw) = &input.code {
            let code = require_code(raw, "code")?;
            if state
                .frameworks
                .iter()
                .any(|entry| entry.value().code == code && entry.value().id != id)
            {
                return Err(UnifiedError::conflict(format!(
                    "A framework with code \"{code}\" already exists"
                )));
            }
            framework.code = code;
        }
        if let Some(raw) = &input.name {
            framework.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(raw) = &input.description {
            framework.description = require_text(raw, "description", MAX_TEXT_LEN)?;
        }
        if let Some(raw) = &input.country {
            framework.country = require_text(raw, "country", MAX_NAME_LEN)?;
        }
        if let Some(active) = input.is_active {
            framework.is_active = active;
        }
        framework.updated_at = now();
        state.frameworks.insert(framework.id, framework.clone());
        state.emit("curriculum.framework.updated", json!({ "framework": framework }));
        Ok(json!(framework))
    })();
    json::respond(result, debug)
}

async fn delete_framework(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "framework")?;
        let framework = get_record(&state.frameworks, &id, "Framework")?;
        if framework.is_built_in {
            return Err(UnifiedError::conflict(
                "A built-in framework cannot be deleted",
            ));
        }
        let referenced = state.levels.iter().any(|entry| entry.value().framework_id == id)
            || state.subjects.iter().any(|entry| entry.value().framework_id == id)
            || state.core_competencies.iter().any(|entry| entry.value().framework_id == id)
            || state.basic_competencies.iter().any(|entry| entry.value().framework_id == id)
            || state.structures.iter().any(|entry| entry.value().framework_id == id)
            || state.grade_descriptors.iter().any(|entry| entry.value().framework_id == id)
            || state.lesson_plans.iter().any(|entry| entry.value().framework_id == id)
            || state.report_cards.iter().any(|entry| entry.value().framework_id == id);
        if referenced {
            return Err(UnifiedError::conflict(
                "Framework is still referenced by levels, subjects, competencies, structures, descriptors, lesson plans, or report cards",
            ));
        }
        state.frameworks.remove(&id);
        state.emit("curriculum.framework.deleted", json!({ "framework_id": id }));
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

async fn set_default_framework(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "framework")?;
        let framework = get_record(&state.frameworks, &id, "Framework")?;
        if !framework.is_active {
            return Err(UnifiedError::conflict(
                "Only an active framework can be set as the default",
            ));
        }
        {
            let mut settings = state
                .settings
                .write()
                .expect("settings lock poisoned");
            settings.default_framework_code = framework.code.clone();
        }
        state.emit(
            "curriculum.framework.default_changed",
            json!({ "framework": framework }),
        );
        Ok(json!(framework))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// School level handlers
// ---------------------------------------------------------------------------

async fn list_levels(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let items: Vec<SchoolLevel> =
            state.levels.iter().map(|entry| entry.value().clone()).collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

async fn create_level(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<LevelInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let input = body.into_inner();
        let framework_id = required_uuid(&input.framework_id, "framework")?;
        get_record(&state.frameworks, &framework_id, "Framework")?;
        let code = require_code(input.code.as_deref().unwrap_or(""), "code")?;
        if state.levels.iter().any(|entry| {
            entry.value().framework_id == framework_id && entry.value().code == code
        }) {
            return Err(UnifiedError::conflict(format!(
                "A level with code \"{code}\" already exists in this framework"
            )));
        }
        let level = SchoolLevel {
            id: Uuid::new_v4(),
            framework_id,
            code,
            name: require_text(input.name.as_deref().unwrap_or(""), "name", MAX_NAME_LEN)?,
            grade_count: input.grade_count.unwrap_or(0),
            sort_order: input.sort_order.unwrap_or(0),
            created_at: now(),
            updated_at: now(),
        };
        state.levels.insert(level.id, level.clone());
        state.emit("curriculum.level.created", json!({ "level": level }));
        Ok(json!(level))
    })();
    json::respond(result, debug)
}

async fn get_level(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "level")?;
        Ok(json!(get_record(&state.levels, &id, "School level")?))
    })();
    json::respond(result, debug)
}

async fn update_level(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<LevelInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "level")?;
        let input = body.into_inner();
        let mut level = get_record(&state.levels, &id, "School level")?;
        if let Some(raw) = &input.framework_id {
            let framework_id = parse_uuid(raw, "framework")?;
            get_record(&state.frameworks, &framework_id, "Framework")?;
            level.framework_id = framework_id;
        }
        if let Some(raw) = &input.code {
            let code = require_code(raw, "code")?;
            if state.levels.iter().any(|entry| {
                entry.value().framework_id == level.framework_id
                    && entry.value().code == code
                    && entry.value().id != id
            }) {
                return Err(UnifiedError::conflict(format!(
                    "A level with code \"{code}\" already exists in this framework"
                )));
            }
            level.code = code;
        }
        if let Some(raw) = &input.name {
            level.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(count) = input.grade_count {
            level.grade_count = count;
        }
        if let Some(order) = input.sort_order {
            level.sort_order = order;
        }
        level.updated_at = now();
        state.levels.insert(level.id, level.clone());
        state.emit("curriculum.level.updated", json!({ "level": level }));
        Ok(json!(level))
    })();
    json::respond(result, debug)
}

async fn delete_level(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "level")?;
        get_record(&state.levels, &id, "School level")?;
        let referenced = state.subjects.iter().any(|entry| entry.value().level_id == id)
            || state.core_competencies.iter().any(|entry| entry.value().level_id == id)
            || state.structures.iter().any(|entry| entry.value().level_id == id)
            || state.report_cards.iter().any(|entry| entry.value().level_id == id);
        if referenced {
            return Err(UnifiedError::conflict(
                "Level is still referenced by subjects, competencies, structures, or report cards",
            ));
        }
        state.levels.remove(&id);
        state.emit("curriculum.level.deleted", json!({ "level_id": id }));
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Subject handlers
// ---------------------------------------------------------------------------

async fn list_subjects(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let items: Vec<Subject> =
            state.subjects.iter().map(|entry| entry.value().clone()).collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

async fn create_subject(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<SubjectInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let input = body.into_inner();
        let framework_id = required_uuid(&input.framework_id, "framework")?;
        let level_id = required_uuid(&input.level_id, "level")?;
        get_record(&state.frameworks, &framework_id, "Framework")?;
        get_record(&state.levels, &level_id, "School level")?;
        let code = require_code(input.code.as_deref().unwrap_or(""), "code")?;
        if state.subjects.iter().any(|entry| {
            entry.value().framework_id == framework_id
                && entry.value().level_id == level_id
                && entry.value().code == code
        }) {
            return Err(UnifiedError::conflict(format!(
                "A subject with code \"{code}\" already exists at this level in this framework"
            )));
        }
        let subject = Subject {
            id: Uuid::new_v4(),
            framework_id,
            level_id,
            code,
            name: require_text(input.name.as_deref().unwrap_or(""), "name", MAX_NAME_LEN)?,
            sort_order: input.sort_order.unwrap_or(0),
            created_at: now(),
            updated_at: now(),
        };
        state.subjects.insert(subject.id, subject.clone());
        state.emit("curriculum.subject.created", json!({ "subject": subject }));
        Ok(json!(subject))
    })();
    json::respond(result, debug)
}

async fn get_subject(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "subject")?;
        Ok(json!(get_record(&state.subjects, &id, "Subject")?))
    })();
    json::respond(result, debug)
}

async fn update_subject(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SubjectInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "subject")?;
        let input = body.into_inner();
        let mut subject = get_record(&state.subjects, &id, "Subject")?;
        if let Some(raw) = &input.framework_id {
            let framework_id = parse_uuid(raw, "framework")?;
            get_record(&state.frameworks, &framework_id, "Framework")?;
            subject.framework_id = framework_id;
        }
        if let Some(raw) = &input.level_id {
            let level_id = parse_uuid(raw, "level")?;
            get_record(&state.levels, &level_id, "School level")?;
            subject.level_id = level_id;
        }
        if let Some(raw) = &input.code {
            let code = require_code(raw, "code")?;
            if state.subjects.iter().any(|entry| {
                entry.value().framework_id == subject.framework_id
                    && entry.value().level_id == subject.level_id
                    && entry.value().code == code
                    && entry.value().id != id
            }) {
                return Err(UnifiedError::conflict(format!(
                    "A subject with code \"{code}\" already exists at this level in this framework"
                )));
            }
            subject.code = code;
        }
        if let Some(raw) = &input.name {
            subject.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(order) = input.sort_order {
            subject.sort_order = order;
        }
        subject.updated_at = now();
        state.subjects.insert(subject.id, subject.clone());
        state.emit("curriculum.subject.updated", json!({ "subject": subject }));
        Ok(json!(subject))
    })();
    json::respond(result, debug)
}

async fn delete_subject(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "subject")?;
        get_record(&state.subjects, &id, "Subject")?;
        let referenced = state
            .core_competencies
            .iter()
            .any(|entry| entry.value().subject_id == id)
            || state.structures.iter().any(|entry| entry.value().subject_id == id)
            || state.lesson_plans.iter().any(|entry| entry.value().subject_id == id)
            || state
                .report_card_entries
                .iter()
                .any(|entry| entry.value().subject_id == id);
        if referenced {
            return Err(UnifiedError::conflict(
                "Subject is still referenced by competencies, structures, lesson plans, or report-card entries",
            ));
        }
        state.subjects.remove(&id);
        state.emit("curriculum.subject.deleted", json!({ "subject_id": id }));
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Core competency handlers
// ---------------------------------------------------------------------------

async fn list_core_competencies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let items: Vec<CoreCompetency> = state
            .core_competencies
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

async fn create_core_competency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<CoreCompetencyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let input = body.into_inner();
        let framework_id = required_uuid(&input.framework_id, "framework")?;
        let subject_id = required_uuid(&input.subject_id, "subject")?;
        let level_id = required_uuid(&input.level_id, "level")?;
        get_record(&state.frameworks, &framework_id, "Framework")?;
        let subject = get_record(&state.subjects, &subject_id, "Subject")?;
        if subject.framework_id != framework_id || subject.level_id != level_id {
            return Err(UnifiedError::bad_request(
                "Subject does not belong to the given framework and level",
            ));
        }
        get_record(&state.levels, &level_id, "School level")?;
        let code = require_code(input.code.as_deref().unwrap_or(""), "code")?;
        let version = input.version.unwrap_or(2013);
        if state.core_competencies.iter().any(|entry| {
            entry.value().subject_id == subject_id
                && entry.value().code == code
                && entry.value().version == version
        }) {
            return Err(UnifiedError::conflict(format!(
                "A core competency with code \"{code}\" already exists for this subject and version"
            )));
        }
        let competency = CoreCompetency {
            id: Uuid::new_v4(),
            framework_id,
            subject_id,
            level_id,
            code,
            title: require_text(input.title.as_deref().unwrap_or(""), "title", MAX_TITLE_LEN)?,
            description: optional_text(&input.description, "description", MAX_TEXT_LEN)?
                .unwrap_or_default(),
            grade_year: input.grade_year.unwrap_or(1),
            version,
            created_at: now(),
            updated_at: now(),
        };
        state.core_competencies.insert(competency.id, competency.clone());
        state.emit(
            "curriculum.core_competency.created",
            json!({ "core_competency": competency }),
        );
        Ok(json!(competency))
    })();
    json::respond(result, debug)
}

async fn get_core_competency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "core competency")?;
        Ok(json!(get_record(&state.core_competencies, &id, "Core competency")?))
    })();
    json::respond(result, debug)
}

async fn update_core_competency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CoreCompetencyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "core competency")?;
        let input = body.into_inner();
        let mut competency = get_record(&state.core_competencies, &id, "Core competency")?;
        if let Some(raw) = &input.subject_id {
            let subject_id = parse_uuid(raw, "subject")?;
            let subject = get_record(&state.subjects, &subject_id, "Subject")?;
            if subject.framework_id != competency.framework_id {
                return Err(UnifiedError::bad_request(
                    "Subject does not belong to this competency's framework",
                ));
            }
            competency.subject_id = subject_id;
            competency.level_id = subject.level_id;
        }
        if let Some(raw) = &input.code {
            competency.code = require_code(raw, "code")?;
        }
        if let Some(raw) = &input.title {
            competency.title = require_text(raw, "title", MAX_TITLE_LEN)?;
        }
        if let Some(raw) = &input.description {
            competency.description = require_text(raw, "description", MAX_TEXT_LEN)?;
        }
        if let Some(year) = input.grade_year {
            competency.grade_year = year;
        }
        if let Some(version) = input.version {
            competency.version = version;
        }
        competency.updated_at = now();
        state.core_competencies.insert(competency.id, competency.clone());
        state.emit(
            "curriculum.core_competency.updated",
            json!({ "core_competency": competency }),
        );
        Ok(json!(competency))
    })();
    json::respond(result, debug)
}

async fn delete_core_competency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "core competency")?;
        get_record(&state.core_competencies, &id, "Core competency")?;
        let referenced = state
            .basic_competencies
            .iter()
            .any(|entry| entry.value().core_competency_id == id)
            || state
                .lesson_plans
                .iter()
                .any(|entry| entry.value().core_competency_id == Some(id));
        if referenced {
            return Err(UnifiedError::conflict(
                "Core competency is still referenced by basic competencies or lesson plans",
            ));
        }
        state.core_competencies.remove(&id);
        state.emit(
            "curriculum.core_competency.deleted",
            json!({ "core_competency_id": id }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Basic competency handlers
// ---------------------------------------------------------------------------

async fn list_basic_competencies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let items: Vec<BasicCompetency> = state
            .basic_competencies
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

async fn create_basic_competency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<BasicCompetencyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let input = body.into_inner();
        let framework_id = required_uuid(&input.framework_id, "framework")?;
        let core_id = required_uuid(&input.core_competency_id, "core competency")?;
        get_record(&state.frameworks, &framework_id, "Framework")?;
        let core = get_record(&state.core_competencies, &core_id, "Core competency")?;
        if core.framework_id != framework_id {
            return Err(UnifiedError::bad_request(
                "Core competency does not belong to the given framework",
            ));
        }
        let code = require_code(input.code.as_deref().unwrap_or(""), "code")?;
        let version = input.version.unwrap_or(core.version);
        if state.basic_competencies.iter().any(|entry| {
            entry.value().core_competency_id == core_id
                && entry.value().code == code
                && entry.value().version == version
        }) {
            return Err(UnifiedError::conflict(format!(
                "A basic competency with code \"{code}\" already exists under this core competency and version"
            )));
        }
        let competency = BasicCompetency {
            id: Uuid::new_v4(),
            framework_id,
            core_competency_id: core_id,
            code,
            title: require_text(input.title.as_deref().unwrap_or(""), "title", MAX_TITLE_LEN)?,
            description: optional_text(&input.description, "description", MAX_TEXT_LEN)?
                .unwrap_or_default(),
            grade_year: input.grade_year.unwrap_or(core.grade_year),
            version,
            created_at: now(),
            updated_at: now(),
        };
        state.basic_competencies.insert(competency.id, competency.clone());
        state.emit(
            "curriculum.basic_competency.created",
            json!({ "basic_competency": competency }),
        );
        Ok(json!(competency))
    })();
    json::respond(result, debug)
}

async fn get_basic_competency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "basic competency")?;
        Ok(json!(get_record(&state.basic_competencies, &id, "Basic competency")?))
    })();
    json::respond(result, debug)
}

async fn update_basic_competency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<BasicCompetencyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "basic competency")?;
        let input = body.into_inner();
        let mut competency = get_record(&state.basic_competencies, &id, "Basic competency")?;
        if let Some(raw) = &input.core_competency_id {
            let core_id = parse_uuid(raw, "core competency")?;
            let core = get_record(&state.core_competencies, &core_id, "Core competency")?;
            if core.framework_id != competency.framework_id {
                return Err(UnifiedError::bad_request(
                    "Core competency does not belong to this competency's framework",
                ));
            }
            competency.core_competency_id = core_id;
        }
        if let Some(raw) = &input.code {
            competency.code = require_code(raw, "code")?;
        }
        if let Some(raw) = &input.title {
            competency.title = require_text(raw, "title", MAX_TITLE_LEN)?;
        }
        if let Some(raw) = &input.description {
            competency.description = require_text(raw, "description", MAX_TEXT_LEN)?;
        }
        if let Some(year) = input.grade_year {
            competency.grade_year = year;
        }
        if let Some(version) = input.version {
            competency.version = version;
        }
        competency.updated_at = now();
        state.basic_competencies.insert(competency.id, competency.clone());
        state.emit(
            "curriculum.basic_competency.updated",
            json!({ "basic_competency": competency }),
        );
        Ok(json!(competency))
    })();
    json::respond(result, debug)
}

async fn delete_basic_competency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "basic competency")?;
        get_record(&state.basic_competencies, &id, "Basic competency")?;
        let referenced = state
            .interventions
            .iter()
            .any(|entry| entry.value().basic_competency_id == id)
            || state
                .lesson_plans
                .iter()
                .any(|entry| entry.value().basic_competency_id == Some(id));
        if referenced {
            return Err(UnifiedError::conflict(
                "Basic competency is still referenced by interventions or lesson plans",
            ));
        }
        state.basic_competencies.remove(&id);
        state.emit(
            "curriculum.basic_competency.deleted",
            json!({ "basic_competency_id": id }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Curriculum structure handlers
// ---------------------------------------------------------------------------

async fn list_structures(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let items: Vec<CurriculumStructure> =
            state.structures.iter().map(|entry| entry.value().clone()).collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

async fn create_structure(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<StructureInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let input = body.into_inner();
        let framework_id = required_uuid(&input.framework_id, "framework")?;
        let level_id = required_uuid(&input.level_id, "level")?;
        let subject_id = required_uuid(&input.subject_id, "subject")?;
        get_record(&state.frameworks, &framework_id, "Framework")?;
        get_record(&state.levels, &level_id, "School level")?;
        let subject = get_record(&state.subjects, &subject_id, "Subject")?;
        if subject.framework_id != framework_id || subject.level_id != level_id {
            return Err(UnifiedError::bad_request(
                "Subject does not belong to the given framework and level",
            ));
        }
        let grade_year = input.grade_year.unwrap_or(1);
        if state.structures.iter().any(|entry| {
            let row = entry.value();
            row.framework_id == framework_id
                && row.level_id == level_id
                && row.grade_year == grade_year
                && row.subject_id == subject_id
        }) {
            return Err(UnifiedError::conflict(
                "This subject is already in the structure for this framework, level, and grade year",
            ));
        }
        let hours = input.hours_per_week.unwrap_or(0.0);
        if !hours.is_finite() || hours < 0.0 || hours > 168.0 {
            return Err(UnifiedError::bad_request(
                "hours_per_week must be between 0 and 168",
            ));
        }
        let row = CurriculumStructure {
            id: Uuid::new_v4(),
            framework_id,
            level_id,
            grade_year,
            subject_id,
            hours_per_week: hours,
            sort_order: input.sort_order.unwrap_or(0),
            created_at: now(),
            updated_at: now(),
        };
        state.structures.insert(row.id, row.clone());
        state.emit("curriculum.structure.created", json!({ "structure": row }));
        Ok(json!(row))
    })();
    json::respond(result, debug)
}

async fn get_structure(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "structure")?;
        Ok(json!(get_record(&state.structures, &id, "Curriculum structure")?))
    })();
    json::respond(result, debug)
}

async fn update_structure(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<StructureInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "structure")?;
        let input = body.into_inner();
        let mut row = get_record(&state.structures, &id, "Curriculum structure")?;
        if let Some(hours) = input.hours_per_week {
            if !hours.is_finite() || hours < 0.0 || hours > 168.0 {
                return Err(UnifiedError::bad_request(
                    "hours_per_week must be between 0 and 168",
                ));
            }
            row.hours_per_week = hours;
        }
        if let Some(year) = input.grade_year {
            row.grade_year = year;
        }
        if let Some(order) = input.sort_order {
            row.sort_order = order;
        }
        row.updated_at = now();
        state.structures.insert(row.id, row.clone());
        state.emit("curriculum.structure.updated", json!({ "structure": row }));
        Ok(json!(row))
    })();
    json::respond(result, debug)
}

async fn delete_structure(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "structure")?;
        get_record(&state.structures, &id, "Curriculum structure")?;
        state.structures.remove(&id);
        state.emit("curriculum.structure.deleted", json!({ "structure_id": id }));
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Assessment domain handlers
// ---------------------------------------------------------------------------

async fn list_assessment_domains(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let items: Vec<AssessmentDomain> = state
            .assessment_domains
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

async fn create_assessment_domain(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<AssessmentDomainInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let input = body.into_inner();
        let code = require_code(input.code.as_deref().unwrap_or(""), "code")?;
        if state.assessment_domains.iter().any(|entry| entry.value().code == code) {
            return Err(UnifiedError::conflict(format!(
                "An assessment domain with code \"{code}\" already exists"
            )));
        }
        let parent_domain_id = optional_uuid(&input.parent_domain_id, "assessment domain")?;
        if let Some(parent_id) = parent_domain_id {
            let parent = get_record(&state.assessment_domains, &parent_id, "Assessment domain")?;
            if parent.parent_domain_id.is_some() {
                return Err(UnifiedError::bad_request(
                    "A sub-domain cannot have a parent that is itself a sub-domain (one nesting level only)",
                ));
            }
        }
        let domain = AssessmentDomain {
            id: Uuid::new_v4(),
            code,
            name: require_text(input.name.as_deref().unwrap_or(""), "name", MAX_NAME_LEN)?,
            parent_domain_id,
            description: optional_text(&input.description, "description", MAX_TEXT_LEN)?
                .unwrap_or_default(),
            sort_order: input.sort_order.unwrap_or(0),
            is_active: input.is_active.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state.assessment_domains.insert(domain.id, domain.clone());
        state.emit(
            "curriculum.assessment_domain.created",
            json!({ "assessment_domain": domain }),
        );
        Ok(json!(domain))
    })();
    json::respond(result, debug)
}

async fn get_assessment_domain(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "assessment domain")?;
        Ok(json!(get_record(&state.assessment_domains, &id, "Assessment domain")?))
    })();
    json::respond(result, debug)
}

async fn update_assessment_domain(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssessmentDomainInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "assessment domain")?;
        let input = body.into_inner();
        let mut domain = get_record(&state.assessment_domains, &id, "Assessment domain")?;
        if let Some(raw) = &input.code {
            let code = require_code(raw, "code")?;
            if state
                .assessment_domains
                .iter()
                .any(|entry| entry.value().code == code && entry.value().id != id)
            {
                return Err(UnifiedError::conflict(format!(
                    "An assessment domain with code \"{code}\" already exists"
                )));
            }
            domain.code = code;
        }
        if let Some(raw) = &input.name {
            domain.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(raw) = &input.parent_domain_id {
            let parent_id = if raw.trim().is_empty() {
                None
            } else {
                Some(parse_uuid(raw.trim(), "assessment domain")?)
            };
            if let Some(parent_id) = parent_id {
                if parent_id == id {
                    return Err(UnifiedError::bad_request(
                        "An assessment domain cannot be its own parent",
                    ));
                }
                let parent =
                    get_record(&state.assessment_domains, &parent_id, "Assessment domain")?;
                if parent.parent_domain_id.is_some() {
                    return Err(UnifiedError::bad_request(
                        "A sub-domain cannot have a parent that is itself a sub-domain (one nesting level only)",
                    ));
                }
            }
            domain.parent_domain_id = parent_id;
        }
        if let Some(raw) = &input.description {
            domain.description = require_text(raw, "description", MAX_TEXT_LEN)?;
        }
        if let Some(order) = input.sort_order {
            domain.sort_order = order;
        }
        if let Some(active) = input.is_active {
            domain.is_active = active;
        }
        domain.updated_at = now();
        state.assessment_domains.insert(domain.id, domain.clone());
        state.emit(
            "curriculum.assessment_domain.updated",
            json!({ "assessment_domain": domain }),
        );
        Ok(json!(domain))
    })();
    json::respond(result, debug)
}

async fn delete_assessment_domain(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "assessment domain")?;
        get_record(&state.assessment_domains, &id, "Assessment domain")?;
        let referenced = state
            .assessment_domains
            .iter()
            .any(|entry| entry.value().parent_domain_id == Some(id))
            || state
                .grade_descriptors
                .iter()
                .any(|entry| entry.value().domain_id == Some(id))
            || state
                .report_card_entries
                .iter()
                .any(|entry| entry.value().domain_id == id);
        if referenced {
            return Err(UnifiedError::conflict(
                "Assessment domain is still referenced by sub-domains, grade descriptors, or report-card entries",
            ));
        }
        state.assessment_domains.remove(&id);
        state.emit(
            "curriculum.assessment_domain.deleted",
            json!({ "assessment_domain_id": id }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Grade descriptor handlers
// ---------------------------------------------------------------------------

async fn list_grade_descriptors(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let items: Vec<GradeDescriptor> = state
            .grade_descriptors
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

async fn create_grade_descriptor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<GradeDescriptorInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let input = body.into_inner();
        let framework_id = required_uuid(&input.framework_id, "framework")?;
        get_record(&state.frameworks, &framework_id, "Framework")?;
        let domain_id = optional_uuid(&input.domain_id, "assessment domain")?;
        if let Some(domain_id) = domain_id {
            get_record(&state.assessment_domains, &domain_id, "Assessment domain")?;
        }
        let min_score = require_score(input.min_score.unwrap_or(0.0), "min_score")?;
        let max_score = require_score(input.max_score.unwrap_or(100.0), "max_score")?;
        if min_score > max_score {
            return Err(UnifiedError::bad_request(
                "min_score must not be greater than max_score",
            ));
        }
        let row = GradeDescriptor {
            id: Uuid::new_v4(),
            framework_id,
            domain_id,
            min_score,
            max_score,
            grade_label: require_text(
                input.grade_label.as_deref().unwrap_or(""),
                "grade_label",
                MAX_CODE_LEN,
            )?,
            descriptor: require_text(
                input.descriptor.as_deref().unwrap_or(""),
                "descriptor",
                MAX_TEXT_LEN,
            )?,
            created_at: now(),
            updated_at: now(),
        };
        state.grade_descriptors.insert(row.id, row.clone());
        state.emit(
            "curriculum.grade_descriptor.created",
            json!({ "grade_descriptor": row }),
        );
        Ok(json!(row))
    })();
    json::respond(result, debug)
}

async fn get_grade_descriptor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "grade descriptor")?;
        Ok(json!(get_record(&state.grade_descriptors, &id, "Grade descriptor")?))
    })();
    json::respond(result, debug)
}

async fn update_grade_descriptor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<GradeDescriptorInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "grade descriptor")?;
        let input = body.into_inner();
        let mut row = get_record(&state.grade_descriptors, &id, "Grade descriptor")?;
        if let Some(raw) = &input.domain_id {
            let domain_id = if raw.trim().is_empty() {
                None
            } else {
                Some(parse_uuid(raw.trim(), "assessment domain")?)
            };
            if let Some(domain_id) = domain_id {
                get_record(&state.assessment_domains, &domain_id, "Assessment domain")?;
            }
            row.domain_id = domain_id;
        }
        if let Some(value) = input.min_score {
            row.min_score = require_score(value, "min_score")?;
        }
        if let Some(value) = input.max_score {
            row.max_score = require_score(value, "max_score")?;
        }
        if row.min_score > row.max_score {
            return Err(UnifiedError::bad_request(
                "min_score must not be greater than max_score",
            ));
        }
        if let Some(raw) = &input.grade_label {
            row.grade_label = require_text(raw, "grade_label", MAX_CODE_LEN)?;
        }
        if let Some(raw) = &input.descriptor {
            row.descriptor = require_text(raw, "descriptor", MAX_TEXT_LEN)?;
        }
        row.updated_at = now();
        state.grade_descriptors.insert(row.id, row.clone());
        state.emit(
            "curriculum.grade_descriptor.updated",
            json!({ "grade_descriptor": row }),
        );
        Ok(json!(row))
    })();
    json::respond(result, debug)
}

async fn delete_grade_descriptor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "grade descriptor")?;
        get_record(&state.grade_descriptors, &id, "Grade descriptor")?;
        state.grade_descriptors.remove(&id);
        state.emit(
            "curriculum.grade_descriptor.deleted",
            json!({ "grade_descriptor_id": id }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Lesson plan handlers
// ---------------------------------------------------------------------------

async fn list_lesson_plans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let items: Vec<LessonPlan> =
            state.lesson_plans.iter().map(|entry| entry.value().clone()).collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

async fn create_lesson_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<LessonPlanInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let input = body.into_inner();
        let settings = state.settings();
        let framework_id = required_uuid(&input.framework_id, "framework")?;
        let subject_id = required_uuid(&input.subject_id, "subject")?;
        get_record(&state.frameworks, &framework_id, "Framework")?;
        let subject = get_record(&state.subjects, &subject_id, "Subject")?;
        if subject.framework_id != framework_id {
            return Err(UnifiedError::bad_request(
                "Subject does not belong to the given framework",
            ));
        }
        let core_competency_id = optional_uuid(&input.core_competency_id, "core competency")?;
        if let Some(core_id) = core_competency_id {
            let core = get_record(&state.core_competencies, &core_id, "Core competency")?;
            if core.subject_id != subject_id {
                return Err(UnifiedError::bad_request(
                    "Core competency does not belong to the given subject",
                ));
            }
        }
        let basic_competency_id =
            optional_uuid(&input.basic_competency_id, "basic competency")?;
        if let Some(basic_id) = basic_competency_id {
            let basic = get_record(&state.basic_competencies, &basic_id, "Basic competency")?;
            if Some(basic.core_competency_id) != core_competency_id {
                return Err(UnifiedError::bad_request(
                    "Basic competency does not belong to the given core competency",
                ));
            }
        }
        let semester = input.semester.unwrap_or(1);
        if !settings.semesters.contains(&semester) {
            return Err(UnifiedError::bad_request(format!(
                "semester \"{semester}\" is not allowed; allowed values: {}",
                settings
                    .semesters
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let status = match &input.status {
            Some(raw) => require_in_list(raw, "status", &settings.lesson_plan_statuses)?,
            None => settings
                .lesson_plan_statuses
                .first()
                .cloned()
                .unwrap_or_else(|| "draft".to_string()),
        };
        let plan = LessonPlan {
            id: Uuid::new_v4(),
            framework_id,
            subject_id,
            core_competency_id,
            basic_competency_id,
            title: require_text(input.title.as_deref().unwrap_or(""), "title", MAX_TITLE_LEN)?,
            objectives: require_string_list(
                &input.objectives.unwrap_or_default(),
                "objectives",
                MAX_TEXT_LEN,
            )?,
            materials: require_string_list(
                &input.materials.unwrap_or_default(),
                "materials",
                MAX_TEXT_LEN,
            )?,
            activities: require_string_list(
                &input.activities.unwrap_or_default(),
                "activities",
                MAX_TEXT_LEN,
            )?,
            assessment_notes: optional_text(&input.assessment_notes, "assessment_notes", MAX_TEXT_LEN)?
                .unwrap_or_default(),
            time_minutes: input.time_minutes.unwrap_or(0),
            grade_year: input.grade_year.unwrap_or(1),
            semester,
            teacher_id: require_text(
                input.teacher_id.as_deref().unwrap_or(&caller.actor),
                "teacher_id",
                MAX_REF_LEN,
            )?,
            status,
            created_at: now(),
            updated_at: now(),
        };
        state.lesson_plans.insert(plan.id, plan.clone());
        state.emit("curriculum.lesson_plan.created", json!({ "lesson_plan": plan }));
        Ok(json!(plan))
    })();
    json::respond(result, debug)
}

async fn get_lesson_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "lesson plan")?;
        Ok(json!(get_record(&state.lesson_plans, &id, "Lesson plan")?))
    })();
    json::respond(result, debug)
}

async fn update_lesson_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<LessonPlanInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let id = parse_uuid(&path.into_inner(), "lesson plan")?;
        let input = body.into_inner();
        let settings = state.settings();
        let mut plan = get_record(&state.lesson_plans, &id, "Lesson plan")?;
        if check_capability(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)
            || plan.teacher_id == caller.actor
        {
            // Teacher owns the plan, or caller manages the module.
        } else {
            return Err(UnifiedError::forbidden(
                "Only a manager or the plan teacher may change this lesson plan",
            ));
        }
        if let Some(raw) = &input.title {
            plan.title = require_text(raw, "title", MAX_TITLE_LEN)?;
        }
        if let Some(values) = &input.objectives {
            plan.objectives = require_string_list(values, "objectives", MAX_TEXT_LEN)?;
        }
        if let Some(values) = &input.materials {
            plan.materials = require_string_list(values, "materials", MAX_TEXT_LEN)?;
        }
        if let Some(values) = &input.activities {
            plan.activities = require_string_list(values, "activities", MAX_TEXT_LEN)?;
        }
        if let Some(raw) = &input.assessment_notes {
            plan.assessment_notes = require_text(raw, "assessment_notes", MAX_TEXT_LEN)?;
        }
        if let Some(minutes) = input.time_minutes {
            plan.time_minutes = minutes;
        }
        if let Some(year) = input.grade_year {
            plan.grade_year = year;
        }
        if let Some(semester) = input.semester {
            if !settings.semesters.contains(&semester) {
                return Err(UnifiedError::bad_request(format!(
                    "semester \"{semester}\" is not allowed"
                )));
            }
            plan.semester = semester;
        }
        if let Some(raw) = &input.status {
            plan.status = require_in_list(raw, "status", &settings.lesson_plan_statuses)?;
        }
        plan.updated_at = now();
        state.lesson_plans.insert(plan.id, plan.clone());
        state.emit("curriculum.lesson_plan.updated", json!({ "lesson_plan": plan }));
        Ok(json!(plan))
    })();
    json::respond(result, debug)
}

async fn delete_lesson_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let id = parse_uuid(&path.into_inner(), "lesson plan")?;
        let plan = get_record(&state.lesson_plans, &id, "Lesson plan")?;
        if !check_capability(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)
            && plan.teacher_id != caller.actor
        {
            return Err(UnifiedError::forbidden(
                "Only a manager or the plan teacher may delete this lesson plan",
            ));
        }
        state.lesson_plans.remove(&id);
        state.emit("curriculum.lesson_plan.deleted", json!({ "lesson_plan_id": id }));
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

fn set_lesson_plan_status(
    ctx: &AppContext,
    state: &CurriculumState,
    caller: &Caller,
    id: Uuid,
    status: &str,
    event: &str,
) -> UnifiedResult<LessonPlan> {
    require_teach(ctx, &caller.role)?;
    let settings = state.settings();
    let status = require_in_list(status, "status", &settings.lesson_plan_statuses)?;
    let mut plan = get_record(&state.lesson_plans, &id, "Lesson plan")?;
    if !check_capability(ctx, &caller.role, PERM_CURRICULUM_MANAGE)
        && plan.teacher_id != caller.actor
    {
        return Err(UnifiedError::forbidden(
            "Only a manager or the plan teacher may change this lesson plan",
        ));
    }
    plan.status = status;
    plan.updated_at = now();
    state.lesson_plans.insert(plan.id, plan.clone());
    state.emit(event, json!({ "lesson_plan": plan }));
    Ok(plan)
}

async fn publish_lesson_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let id = parse_uuid(&path.into_inner(), "lesson plan")?;
        let plan = set_lesson_plan_status(
            &ctx,
            &state,
            &caller,
            id,
            "published",
            "curriculum.lesson_plan.published",
        )?;
        Ok(json!(plan))
    })();
    json::respond(result, debug)
}

async fn archive_lesson_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let id = parse_uuid(&path.into_inner(), "lesson plan")?;
        let plan = set_lesson_plan_status(
            &ctx,
            &state,
            &caller,
            id,
            "archived",
            "curriculum.lesson_plan.archived",
        )?;
        Ok(json!(plan))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Report card handlers
// ---------------------------------------------------------------------------

async fn list_report_cards(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let items: Vec<ReportCard> =
            state.report_cards.iter().map(|entry| entry.value().clone()).collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

async fn create_report_card(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<ReportCardInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let input = body.into_inner();
        let settings = state.settings();
        let student_ref = require_text(
            input.student_ref.as_deref().unwrap_or(""),
            "student_ref",
            MAX_REF_LEN,
        )?;
        check_student_reference(&ctx, &state, &student_ref)?;
        let framework_id = required_uuid(&input.framework_id, "framework")?;
        let level_id = required_uuid(&input.level_id, "level")?;
        get_record(&state.frameworks, &framework_id, "Framework")?;
        get_record(&state.levels, &level_id, "School level")?;
        let semester = input.semester.unwrap_or(1);
        if !settings.semesters.contains(&semester) {
            return Err(UnifiedError::bad_request(format!(
                "semester \"{semester}\" is not allowed"
            )));
        }
        let academic_year = require_text(
            input.academic_year.as_deref().unwrap_or(""),
            "academic_year",
            MAX_CODE_LEN,
        )?;
        let grade_year = input.grade_year.unwrap_or(1);
        if state.report_cards.iter().any(|entry| {
            let row = entry.value();
            row.student_ref == student_ref
                && row.framework_id == framework_id
                && row.level_id == level_id
                && row.grade_year == grade_year
                && row.semester == semester
                && row.academic_year == academic_year
        }) {
            return Err(UnifiedError::conflict(
                "A report card already exists for this student, framework, level, grade year, semester, and academic year",
            ));
        }
        let status = match &input.status {
            Some(raw) => require_in_list(raw, "status", &settings.report_card_statuses)?,
            None => settings
                .report_card_statuses
                .first()
                .cloned()
                .unwrap_or_else(|| "draft".to_string()),
        };
        let card = ReportCard {
            id: Uuid::new_v4(),
            student_ref,
            framework_id,
            level_id,
            grade_year,
            semester,
            academic_year,
            status,
            issued_at: None,
            issued_by: None,
            created_at: now(),
            updated_at: now(),
        };
        state.report_cards.insert(card.id, card.clone());
        state.emit("curriculum.report_card.created", json!({ "report_card": card }));
        Ok(json!(card))
    })();
    json::respond(result, debug)
}

async fn get_report_card(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "report card")?;
        let card = get_record(&state.report_cards, &id, "Report card")?;
        let entries: Vec<ReportCardEntry> = state
            .report_card_entries
            .iter()
            .filter(|entry| entry.value().report_card_id == id)
            .map(|entry| entry.value().clone())
            .collect();
        Ok(json!({ "report_card": card, "entries": entries }))
    })();
    json::respond(result, debug)
}

async fn update_report_card(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReportCardInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let id = parse_uuid(&path.into_inner(), "report card")?;
        let input = body.into_inner();
        let settings = state.settings();
        let mut card = get_record(&state.report_cards, &id, "Report card")?;
        if card.status == "issued" && input.status.is_none() {
            return Err(UnifiedError::conflict(
                "An issued report card cannot be changed; archive it first or create a new one",
            ));
        }
        if let Some(raw) = &input.status {
            card.status = require_in_list(raw, "status", &settings.report_card_statuses)?;
        }
        if let Some(year) = input.grade_year {
            card.grade_year = year;
        }
        if let Some(semester) = input.semester {
            if !settings.semesters.contains(&semester) {
                return Err(UnifiedError::bad_request(format!(
                    "semester \"{semester}\" is not allowed"
                )));
            }
            card.semester = semester;
        }
        card.updated_at = now();
        state.report_cards.insert(card.id, card.clone());
        state.emit("curriculum.report_card.updated", json!({ "report_card": card }));
        Ok(json!(card))
    })();
    json::respond(result, debug)
}

async fn delete_report_card(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let id = parse_uuid(&path.into_inner(), "report card")?;
        let card = get_record(&state.report_cards, &id, "Report card")?;
        if card.status == "issued" {
            return Err(UnifiedError::conflict(
                "An issued report card cannot be deleted",
            ));
        }
        let entry_ids: Vec<Uuid> = state
            .report_card_entries
            .iter()
            .filter(|entry| entry.value().report_card_id == id)
            .map(|entry| *entry.key())
            .collect();
        for entry_id in entry_ids {
            state.report_card_entries.remove(&entry_id);
        }
        state.report_cards.remove(&id);
        state.emit("curriculum.report_card.deleted", json!({ "report_card_id": id }));
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

async fn issue_report_card(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let id = parse_uuid(&path.into_inner(), "report card")?;
        let settings = state.settings();
        let mut card = get_record(&state.report_cards, &id, "Report card")?;
        if !settings.report_card_statuses.contains(&"issued".to_string()) {
            return Err(UnifiedError::conflict(
                "The configured report-card statuses do not include an issued status",
            ));
        }
        if card.status == "issued" {
            return Err(UnifiedError::conflict("Report card is already issued"));
        }
        card.status = "issued".to_string();
        card.issued_at = Some(now());
        card.issued_by = Some(caller.actor.clone());
        card.updated_at = now();
        state.report_cards.insert(card.id, card.clone());
        state.emit("curriculum.report_card.issued", json!({ "report_card": card }));
        Ok(json!(card))
    })();
    json::respond(result, debug)
}

async fn print_report_card(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let id = parse_uuid(&path.into_inner(), "report card")?;
        let card = get_record(&state.report_cards, &id, "Report card")?;
        let settings = state.settings();
        let payload = json!({
            "report_card_id": card.id,
            "student_ref": card.student_ref,
            "academic_year": card.academic_year,
            "semester": card.semester,
            "paper_size": settings.default_paper_size,
            "document_type": "report_card",
            "requested_by": caller.actor,
        });
        // Event for the printing module (batch 12): it subscribes to
        // `curriculum.report_card.print_requested`. The same payload
        // is also emitted as the generic `print.job.requested` event.
        // This module never calls a printing module directly.
        state.emit("curriculum.report_card.print_requested", payload.clone());
        state.emit("print.job.requested", payload);
        Ok(json!({
            "print_requested": true,
            "report_card_id": card.id,
            "paper_size": settings.default_paper_size,
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Report card entry handlers
// ---------------------------------------------------------------------------

async fn list_report_card_entries(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let card_id = parse_uuid(&path.into_inner(), "report card")?;
        get_record(&state.report_cards, &card_id, "Report card")?;
        let items: Vec<ReportCardEntry> = state
            .report_card_entries
            .iter()
            .filter(|entry| entry.value().report_card_id == card_id)
            .map(|entry| entry.value().clone())
            .collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

async fn add_report_card_entry(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReportCardEntryInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let card_id = parse_uuid(&path.into_inner(), "report card")?;
        let input = body.into_inner();
        let settings = state.settings();
        let card = get_record(&state.report_cards, &card_id, "Report card")?;
        if card.status == "issued" {
            return Err(UnifiedError::conflict(
                "Entries cannot be added to an issued report card",
            ));
        }
        let subject_id = required_uuid(&input.subject_id, "subject")?;
        let domain_id = required_uuid(&input.domain_id, "assessment domain")?;
        let subject = get_record(&state.subjects, &subject_id, "Subject")?;
        if subject.framework_id != card.framework_id {
            return Err(UnifiedError::bad_request(
                "Subject does not belong to this report card's framework",
            ));
        }
        let domain = get_record(&state.assessment_domains, &domain_id, "Assessment domain")?;
        if !domain.is_active {
            return Err(UnifiedError::conflict("Assessment domain is not active"));
        }
        if state.report_card_entries.iter().any(|entry| {
            let row = entry.value();
            row.report_card_id == card_id && row.subject_id == subject_id && row.domain_id == domain_id
        }) {
            return Err(UnifiedError::conflict(
                "This subject and assessment domain already have an entry on this report card",
            ));
        }
        let score = match input.score {
            Some(value) => Some(require_score(value, "score")?),
            None => None,
        };
        // Grade label and descriptor are derived from data when the
        // caller does not supply them: the label from the configured
        // grade bands, the descriptor from the descriptor rows.
        let grade_label = match &input.grade_label {
            Some(raw) => Some(require_text(raw, "grade_label", MAX_CODE_LEN)?),
            None => score.and_then(|value| grade_label_for(&settings, value)),
        };
        let descriptor = match &input.descriptor {
            Some(raw) => Some(require_text(raw, "descriptor", MAX_TEXT_LEN)?),
            None => score.and_then(|value| {
                descriptor_for(&state, card.framework_id, Some(domain_id), value)
            }),
        };
        // The academic module may not have announced this grade
        // yet; the link is still accepted — the grade record stays
        // owned by the academic module either way.
        let academic_grade_id = optional_text(&input.academic_grade_id, "academic_grade_id", MAX_REF_LEN)?;
        let entry = ReportCardEntry {
            id: Uuid::new_v4(),
            report_card_id: card_id,
            subject_id,
            domain_id,
            score,
            grade_label,
            descriptor,
            notes: optional_text(&input.notes, "notes", MAX_TEXT_LEN)?.unwrap_or_default(),
            academic_grade_id,
            created_at: now(),
            updated_at: now(),
        };
        state.report_card_entries.insert(entry.id, entry.clone());
        state.emit(
            "curriculum.report_card.entry.added",
            json!({ "report_card_entry": entry }),
        );
        Ok(json!(entry))
    })();
    json::respond(result, debug)
}

async fn update_report_card_entry(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReportCardEntryInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let entry_id = parse_uuid(&path.into_inner(), "report card entry")?;
        let input = body.into_inner();
        let settings = state.settings();
        let mut entry = get_record(&state.report_card_entries, &entry_id, "Report card entry")?;
        let card = get_record(&state.report_cards, &entry.report_card_id, "Report card")?;
        if card.status == "issued" {
            return Err(UnifiedError::conflict(
                "Entries of an issued report card cannot be changed",
            ));
        }
        if let Some(value) = input.score {
            entry.score = Some(require_score(value, "score")?);
            if input.grade_label.is_none() {
                entry.grade_label = grade_label_for(&settings, value);
            }
            if input.descriptor.is_none() {
                entry.descriptor = descriptor_for(
                    &state,
                    card.framework_id,
                    Some(entry.domain_id),
                    value,
                );
            }
        }
        if let Some(raw) = &input.grade_label {
            entry.grade_label = Some(require_text(raw, "grade_label", MAX_CODE_LEN)?);
        }
        if let Some(raw) = &input.descriptor {
            entry.descriptor = Some(require_text(raw, "descriptor", MAX_TEXT_LEN)?);
        }
        if let Some(raw) = &input.notes {
            entry.notes = require_text(raw, "notes", MAX_TEXT_LEN)?;
        }
        if let Some(raw) = &input.academic_grade_id {
            entry.academic_grade_id =
                Some(require_text(raw, "academic_grade_id", MAX_REF_LEN)?);
        }
        entry.updated_at = now();
        state.report_card_entries.insert(entry.id, entry.clone());
        state.emit(
            "curriculum.report_card.entry.updated",
            json!({ "report_card_entry": entry }),
        );
        Ok(json!(entry))
    })();
    json::respond(result, debug)
}

async fn delete_report_card_entry(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let entry_id = parse_uuid(&path.into_inner(), "report card entry")?;
        let entry = get_record(&state.report_card_entries, &entry_id, "Report card entry")?;
        let card = get_record(&state.report_cards, &entry.report_card_id, "Report card")?;
        if card.status == "issued" {
            return Err(UnifiedError::conflict(
                "Entries of an issued report card cannot be removed",
            ));
        }
        state.report_card_entries.remove(&entry_id);
        state.emit(
            "curriculum.report_card.entry.removed",
            json!({ "report_card_entry_id": entry_id, "report_card_id": card.id }),
        );
        Ok(json!({ "deleted": true, "id": entry_id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Intervention handlers (remedial + enrichment)
// ---------------------------------------------------------------------------

async fn list_interventions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let items: Vec<Intervention> =
            state.interventions.iter().map(|entry| entry.value().clone()).collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

async fn create_intervention(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<InterventionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let input = body.into_inner();
        let settings = state.settings();
        let student_ref = require_text(
            input.student_ref.as_deref().unwrap_or(""),
            "student_ref",
            MAX_REF_LEN,
        )?;
        check_student_reference(&ctx, &state, &student_ref)?;
        let basic_competency_id =
            required_uuid(&input.basic_competency_id, "basic competency")?;
        get_record(
            &state.basic_competencies,
            &basic_competency_id,
            "Basic competency",
        )?;
        let kind = match &input.kind {
            Some(raw) => require_in_list(raw, "kind", &settings.intervention_kinds)?,
            None => settings
                .intervention_kinds
                .first()
                .cloned()
                .unwrap_or_else(|| "remedial".to_string()),
        };
        let status = match &input.status {
            Some(raw) => require_in_list(raw, "status", &settings.intervention_statuses)?,
            None => settings
                .intervention_statuses
                .first()
                .cloned()
                .unwrap_or_else(|| "scheduled".to_string()),
        };
        let scheduled_at = match &input.scheduled_at {
            Some(raw) => {
                let parsed = DateTime::parse_from_rfc3339(raw.trim()).map_err(|_| {
                    UnifiedError::bad_request(
                        "scheduled_at must be an RFC 3339 date-time",
                    )
                })?;
                Some(parsed.with_timezone(&Utc))
            }
            None => None,
        };
        let intervention = Intervention {
            id: Uuid::new_v4(),
            student_ref,
            basic_competency_id,
            kind,
            status,
            teacher_id: require_text(
                input.teacher_id.as_deref().unwrap_or(&caller.actor),
                "teacher_id",
                MAX_REF_LEN,
            )?,
            scheduled_at,
            completed_at: None,
            notes: optional_text(&input.notes, "notes", MAX_TEXT_LEN)?.unwrap_or_default(),
            created_at: now(),
            updated_at: now(),
        };
        state.interventions.insert(intervention.id, intervention.clone());
        state.emit(
            "curriculum.intervention.created",
            json!({ "intervention": intervention }),
        );
        Ok(json!(intervention))
    })();
    json::respond(result, debug)
}

async fn get_intervention(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "intervention")?;
        Ok(json!(get_record(&state.interventions, &id, "Intervention")?))
    })();
    json::respond(result, debug)
}

async fn update_intervention(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<InterventionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let id = parse_uuid(&path.into_inner(), "intervention")?;
        let input = body.into_inner();
        let settings = state.settings();
        let mut intervention = get_record(&state.interventions, &id, "Intervention")?;
        if let Some(raw) = &input.kind {
            intervention.kind = require_in_list(raw, "kind", &settings.intervention_kinds)?;
        }
        if let Some(raw) = &input.status {
            intervention.status =
                require_in_list(raw, "status", &settings.intervention_statuses)?;
        }
        if let Some(raw) = &input.scheduled_at {
            let parsed = DateTime::parse_from_rfc3339(raw.trim()).map_err(|_| {
                UnifiedError::bad_request("scheduled_at must be an RFC 3339 date-time")
            })?;
            intervention.scheduled_at = Some(parsed.with_timezone(&Utc));
        }
        if let Some(raw) = &input.notes {
            intervention.notes = require_text(raw, "notes", MAX_TEXT_LEN)?;
        }
        intervention.updated_at = now();
        state.interventions.insert(intervention.id, intervention.clone());
        state.emit(
            "curriculum.intervention.updated",
            json!({ "intervention": intervention }),
        );
        Ok(json!(intervention))
    })();
    json::respond(result, debug)
}

async fn delete_intervention(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let id = parse_uuid(&path.into_inner(), "intervention")?;
        get_record(&state.interventions, &id, "Intervention")?;
        state.interventions.remove(&id);
        state.emit(
            "curriculum.intervention.deleted",
            json!({ "intervention_id": id }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

fn set_intervention_status(
    ctx: &AppContext,
    state: &CurriculumState,
    caller: &Caller,
    id: Uuid,
    status: &str,
    event: &str,
    mark_completed: bool,
) -> UnifiedResult<Intervention> {
    require_teach(ctx, &caller.role)?;
    let settings = state.settings();
    let status = require_in_list(status, "status", &settings.intervention_statuses)?;
    let mut intervention = get_record(&state.interventions, &id, "Intervention")?;
    intervention.status = status;
    if mark_completed {
        intervention.completed_at = Some(now());
    }
    intervention.updated_at = now();
    state.interventions.insert(intervention.id, intervention.clone());
    state.emit(event, json!({ "intervention": intervention }));
    Ok(intervention)
}

async fn complete_intervention(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let id = parse_uuid(&path.into_inner(), "intervention")?;
        let intervention = set_intervention_status(
            &ctx,
            &state,
            &caller,
            id,
            "completed",
            "curriculum.intervention.completed",
            true,
        )?;
        Ok(json!(intervention))
    })();
    json::respond(result, debug)
}

async fn cancel_intervention(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let id = parse_uuid(&path.into_inner(), "intervention")?;
        let intervention = set_intervention_status(
            &ctx,
            &state,
            &caller,
            id,
            "cancelled",
            "curriculum.intervention.cancelled",
            false,
        )?;
        Ok(json!(intervention))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Grade-component mapping handlers
// ---------------------------------------------------------------------------

/// List the grade-component mappings (academic component id →
/// basic competency id), sorted by component id. The mapping table
/// is the admin-managed bridge that lets `academic.grade.recorded`
/// events resolve to curriculum competencies.
async fn list_component_mappings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let mut items: Vec<ComponentCompetencyMapping> = state
            .component_mappings
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| a.academic_component_id.cmp(&b.academic_component_id));
        Ok(json!(items))
    })();
    json::respond(result, debug)
}

/// Create one grade-component mapping. The basic competency must
/// exist in this module; the academic component id is an opaque
/// reference owned by the academic module and is stored as given
/// (one mapping per component — a second row for the same component
/// is a conflict). Grades recorded before the mapping exists stay
/// unmapped; creating the row does not rewrite them.
async fn create_component_mapping(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<ComponentMappingInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let input = body.into_inner();
        let component_id = require_text(
            input.academic_component_id.as_deref().unwrap_or(""),
            "academic_component_id",
            MAX_REF_LEN,
        )?;
        let competency_id = required_uuid(
            &input.basic_competency_id,
            "basic competency",
        )?;
        get_record(
            &state.basic_competencies,
            &competency_id,
            "Basic competency",
        )?;
        if state.component_mappings.contains_key(&component_id) {
            return Err(UnifiedError::conflict(format!(
                "A mapping already exists for academic component \"{component_id}\""
            )));
        }
        let mapping = ComponentCompetencyMapping {
            academic_component_id: component_id.clone(),
            basic_competency_id: competency_id.to_string(),
            created_at: now(),
        };
        state
            .component_mappings
            .insert(component_id, mapping.clone());
        state.emit(
            "curriculum.grade_component_mapping.created",
            json!({ "grade_component_mapping": mapping }),
        );
        Ok(json!(mapping))
    })();
    json::respond(result, debug)
}

/// Delete one grade-component mapping. Grades already recorded
/// against the component keep the competency they resolved to at
/// the time; later grades for the component record as unmapped
/// until a new mapping row is created.
async fn delete_component_mapping(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let component_id = require_text(path.as_str(), "academic_component_id", MAX_REF_LEN)?;
        let mapping = state
            .component_mappings
            .remove(&component_id)
            .map(|(_, value)| value)
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No mapping exists for academic component \"{component_id}\""
                ))
            })?;
        state.emit(
            "curriculum.grade_component_mapping.deleted",
            json!({ "grade_component_mapping": mapping }),
        );
        Ok(json!({ "deleted": true, "academic_component_id": component_id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Settings handlers
// ---------------------------------------------------------------------------

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        Ok(json!(state.settings()))
    })();
    json::respond(result, debug)
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<SettingsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(raw) = &input.default_framework_code {
            let code = require_code(raw, "default_framework_code")?;
            if !state.frameworks.iter().any(|entry| entry.value().code == code) {
                return Err(UnifiedError::bad_request(format!(
                    "No framework with code \"{code}\" exists"
                )));
            }
            settings.default_framework_code = code;
        }
        if let Some(raw) = &input.default_paper_size {
            let size = require_code(raw, "default_paper_size")?;
            if let Some(sizes) = &input.paper_sizes {
                if !sizes.iter().any(|item| item == &size) {
                    return Err(UnifiedError::bad_request(
                        "default_paper_size must be one of paper_sizes",
                    ));
                }
            } else if !settings.paper_sizes.iter().any(|item| item == &size) {
                return Err(UnifiedError::bad_request(
                    "default_paper_size must be one of paper_sizes",
                ));
            }
            settings.default_paper_size = size;
        }
        if let Some(values) = &input.paper_sizes {
            if values.is_empty() {
                return Err(UnifiedError::bad_request("paper_sizes must not be empty"));
            }
            settings.paper_sizes =
                require_string_list(values, "paper_sizes", MAX_CODE_LEN)?;
        }
        if let Some(values) = &input.semesters {
            if values.is_empty() {
                return Err(UnifiedError::bad_request("semesters must not be empty"));
            }
            settings.semesters = values.clone();
        }
        if let Some(values) = &input.intervention_kinds {
            if values.is_empty() {
                return Err(UnifiedError::bad_request(
                    "intervention_kinds must not be empty",
                ));
            }
            settings.intervention_kinds =
                require_string_list(values, "intervention_kinds", MAX_CODE_LEN)?;
        }
        if let Some(values) = &input.lesson_plan_statuses {
            if values.is_empty() {
                return Err(UnifiedError::bad_request(
                    "lesson_plan_statuses must not be empty",
                ));
            }
            settings.lesson_plan_statuses = require_string_list(
                values,
                "lesson_plan_statuses",
                MAX_CODE_LEN,
            )?;
        }
        if let Some(values) = &input.report_card_statuses {
            if values.is_empty() {
                return Err(UnifiedError::bad_request(
                    "report_card_statuses must not be empty",
                ));
            }
            settings.report_card_statuses = require_string_list(
                values,
                "report_card_statuses",
                MAX_CODE_LEN,
            )?;
        }
        if let Some(values) = &input.intervention_statuses {
            if values.is_empty() {
                return Err(UnifiedError::bad_request(
                    "intervention_statuses must not be empty",
                ));
            }
            settings.intervention_statuses = require_string_list(
                values,
                "intervention_statuses",
                MAX_CODE_LEN,
            )?;
        }
        if let Some(bands) = input.default_grade_bands {
            if bands.is_empty() {
                return Err(UnifiedError::bad_request(
                    "default_grade_bands must not be empty",
                ));
            }
            for band in &bands {
                require_score(band.min_score, "grade band min_score")?;
                require_score(band.max_score, "grade band max_score")?;
                if band.min_score > band.max_score {
                    return Err(UnifiedError::bad_request(
                        "Grade band min_score must not exceed max_score",
                    ));
                }
                require_text(&band.label, "grade band label", MAX_CODE_LEN)?;
            }
            settings.default_grade_bands = bands;
        }
        {
            let mut guard = state
                .settings
                .write()
                .expect("settings lock poisoned");
            *guard = settings.clone();
        }
        state.emit("curriculum.settings.updated", json!({ "settings": settings }));
        Ok(json!(settings))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------
// AI capabilities (AI Capabilities Contract, see `crate::modules::ai`).
//
// Declared here, registered once in `init`, grounded through the
// `curriculum.ai.draft_lesson_plan_requested` hook, and run through
// the AI module's gated runner. Drafts are returned inline and are
// never stored, published, or issued by this module: creating a
// lesson plan stays on the existing human endpoints.
// ---------------------------------------------------------------------------

/// AI capabilities this module exposes through the AI module's
/// contract. The capability is non-PII: competency texts and the
/// topic are curriculum data, redacted before reaching the model.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "draft_lesson_plan",
        module_id: "curriculum",
        name: "Draft lesson plan",
        description: "Draft a lesson plan from a core competency, a basic competency, and a topic. The output is returned inline; it is never stored or published by this module.",
        input_fields: &["core_competency_id", "basic_competency_id", "topic"],
        allows_pii: false,
        use_case: "curriculum.draft_lesson_plan",
    },
];

/// Grounding for a lesson-plan draft: the named core and basic
/// competencies with their codes, titles, and descriptions, plus
/// the subject and framework they belong to. Unknown ids ground
/// nothing (the endpoint 404s on them before delegating).
fn lesson_plan_grounding(
    state: &CurriculumState,
    core_competency_id: &str,
    basic_competency_id: &str,
) -> Value {
    let core = Uuid::parse_str(core_competency_id)
        .ok()
        .and_then(|id| {
            state
                .core_competencies
                .get(&id)
                .map(|entry| entry.value().clone())
        });
    let basic = Uuid::parse_str(basic_competency_id)
        .ok()
        .and_then(|id| {
            state
                .basic_competencies
                .get(&id)
                .map(|entry| entry.value().clone())
        });
    let subject = core.as_ref().and_then(|competency| {
        state
            .subjects
            .get(&competency.subject_id)
            .map(|entry| entry.value().clone())
    });
    let framework = core.as_ref().and_then(|competency| {
        state
            .frameworks
            .get(&competency.framework_id)
            .map(|entry| entry.value().clone())
    });
    json!({
        "core_competency": core.map(|competency| json!({
            "code": competency.code,
            "title": competency.title,
            "description": competency.description,
            "grade_year": competency.grade_year,
        })),
        "basic_competency": basic.map(|competency| json!({
            "code": competency.code,
            "title": competency.title,
            "description": competency.description,
            "grade_year": competency.grade_year,
        })),
        "subject": subject.map(|subject| json!({
            "code": subject.code,
            "name": subject.name,
        })),
        "framework": framework.map(|framework| json!({
            "code": framework.code,
            "name": framework.name,
        })),
    })
}

/// Run one registered curriculum AI capability:
/// `POST /api/v1/curriculum/ai/{capability}` with body
/// `{"input": {...}}`. Requires `ai:use` plus the module's own
/// `curriculum:view`; for `draft_lesson_plan` the named
/// competencies must exist (404 otherwise). Delegates to the AI
/// module's gated runner.
async fn ai_capability(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ai::CapabilityCallInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let capability = path.into_inner();
    let input = body.into_inner().input;
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW) {
        return json::error_response(&error, debug);
    }
    if capability == "draft_lesson_plan" {
        for field in ["core_competency_id", "basic_competency_id"] {
            let raw = input
                .get(field)
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let id = match Uuid::parse_str(raw) {
                Ok(id) => id,
                Err(_) => {
                    return json::error_response(
                        &UnifiedError::bad_request(format!(
                            "input.{field} must be a valid id"
                        )),
                        debug,
                    )
                }
            };
            let exists = if field == "core_competency_id" {
                state.core_competencies.contains_key(&id)
            } else {
                state.basic_competencies.contains_key(&id)
            };
            if !exists {
                return json::error_response(
                    &UnifiedError::not_found(format!("Competency not found: {id}")),
                    debug,
                );
            }
        }
    }
    let ai_caller = match ai::caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = ai::require_permission(&ctx, &ai_caller.role, ai::PERM_AI_USE) {
        return json::error_response(&error, debug);
    }
    let company_id = ai::resolve_capability_company(&ctx, &req, &ai_caller);
    match ai::run_ai_capability(
        &ctx,
        &ai_caller,
        company_id,
        "curriculum",
        &capability,
        input,
    )
    .await
    {
        Ok(data) => json::success_response(data, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct CurriculumModule {
    state: CurriculumState,
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
        let state = self.state.clone();
        bus.register(
            "academic.student.enrolled",
            "curriculum",
            Arc::new(move |ctx: &mut HookContext| {
                on_academic_student(state.clone(), ctx)
            }),
        );
        // Canonical enrollment event: the enrollment-to-student
        // link the grade handler resolves through. The academic
        // module never emits `academic.student.enrolled`; this is
        // the event that actually carries enrollment ids.
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
            RouteInfo { method: "GET", path: "/api/v1/curriculum/frameworks", owner: "curriculum", description: "List curriculum frameworks (paginated, filter by code, country, is_active, is_built_in)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/frameworks", owner: "curriculum", description: "Create a curriculum framework (new frameworks are data rows; no code change)." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/frameworks/{id}", owner: "curriculum", description: "Get one curriculum framework." },
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/frameworks/{id}", owner: "curriculum", description: "Update a curriculum framework." },
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/frameworks/{id}", owner: "curriculum", description: "Delete a curriculum framework (built-in and referenced frameworks are refused)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/frameworks/{id}/set-default", owner: "curriculum", description: "Make a framework the default framework." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/levels", owner: "curriculum", description: "List school levels (paginated, filter by framework_id, code)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/levels", owner: "curriculum", description: "Create a school level." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/levels/{id}", owner: "curriculum", description: "Get one school level." },
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/levels/{id}", owner: "curriculum", description: "Update a school level." },
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/levels/{id}", owner: "curriculum", description: "Delete a school level (referenced levels are refused)." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/subjects", owner: "curriculum", description: "List subjects (paginated, filter by framework_id, level_id, code)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/subjects", owner: "curriculum", description: "Create a subject for a framework and level." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/subjects/{id}", owner: "curriculum", description: "Get one subject." },
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/subjects/{id}", owner: "curriculum", description: "Update a subject." },
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/subjects/{id}", owner: "curriculum", description: "Delete a subject (referenced subjects are refused)." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/core-competencies", owner: "curriculum", description: "List core competencies (paginated, filter by framework_id, subject_id, level_id, code, grade_year, version)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/core-competencies", owner: "curriculum", description: "Create a core competency (versioned per curriculum year)." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/core-competencies/{id}", owner: "curriculum", description: "Get one core competency." },
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/core-competencies/{id}", owner: "curriculum", description: "Update a core competency." },
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/core-competencies/{id}", owner: "curriculum", description: "Delete a core competency (referenced competencies are refused)." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/basic-competencies", owner: "curriculum", description: "List basic competencies (paginated, filter by framework_id, core_competency_id, code, grade_year, version)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/basic-competencies", owner: "curriculum", description: "Create a basic competency under a core competency." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/basic-competencies/{id}", owner: "curriculum", description: "Get one basic competency." },
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/basic-competencies/{id}", owner: "curriculum", description: "Update a basic competency." },
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/basic-competencies/{id}", owner: "curriculum", description: "Delete a basic competency (referenced competencies are refused)." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/structure", owner: "curriculum", description: "List curriculum structure rows (paginated, filter by framework_id, level_id, grade_year, subject_id)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/structure", owner: "curriculum", description: "Add a subject to a level and grade year with weekly hours." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/structure/{id}", owner: "curriculum", description: "Get one curriculum structure row." },
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/structure/{id}", owner: "curriculum", description: "Update hours or grade year of a structure row." },
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/structure/{id}", owner: "curriculum", description: "Remove a subject from a level and grade year." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/assessment-domains", owner: "curriculum", description: "List assessment domains (paginated, filter by code, parent_domain_id, is_active)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/assessment-domains", owner: "curriculum", description: "Create an assessment domain or sub-domain." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/assessment-domains/{id}", owner: "curriculum", description: "Get one assessment domain." },
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/assessment-domains/{id}", owner: "curriculum", description: "Update an assessment domain." },
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/assessment-domains/{id}", owner: "curriculum", description: "Delete an assessment domain (referenced domains are refused)." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/grade-descriptors", owner: "curriculum", description: "List grade descriptors (paginated, filter by framework_id, domain_id, grade_label)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/grade-descriptors", owner: "curriculum", description: "Create a grade descriptor for a score band." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/grade-descriptors/{id}", owner: "curriculum", description: "Get one grade descriptor." },
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/grade-descriptors/{id}", owner: "curriculum", description: "Update a grade descriptor." },
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/grade-descriptors/{id}", owner: "curriculum", description: "Delete a grade descriptor." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/grade-component-mappings", owner: "curriculum", description: "List grade-component mappings (academic grade component id to basic competency id)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/grade-component-mappings", owner: "curriculum", description: "Create a grade-component mapping (manager or teacher; the basic competency must exist)." },
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/grade-component-mappings/{component_id}", owner: "curriculum", description: "Delete a grade-component mapping (manager or teacher; later grades for the component record as unmapped)." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/lesson-plans", owner: "curriculum", description: "List lesson plans (paginated, filter by framework_id, subject_id, competency ids, grade_year, semester, teacher_id, status)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/lesson-plans", owner: "curriculum", description: "Create a lesson plan tied to competencies." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/lesson-plans/{id}", owner: "curriculum", description: "Get one lesson plan." },
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/lesson-plans/{id}", owner: "curriculum", description: "Update a lesson plan (manager or the plan teacher)." },
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/lesson-plans/{id}", owner: "curriculum", description: "Delete a lesson plan (manager or the plan teacher)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/lesson-plans/{id}/publish", owner: "curriculum", description: "Publish a lesson plan." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/lesson-plans/{id}/archive", owner: "curriculum", description: "Archive a lesson plan." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/report-cards", owner: "curriculum", description: "List report cards (paginated, filter by student_ref, framework_id, level_id, grade_year, semester, academic_year, status)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/report-cards", owner: "curriculum", description: "Create a per-semester report card for a student." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/report-cards/{id}", owner: "curriculum", description: "Get one report card with its entries." },
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/report-cards/{id}", owner: "curriculum", description: "Update a report card (issued cards are locked)." },
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/report-cards/{id}", owner: "curriculum", description: "Delete a draft report card and its entries (issued cards are refused)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/report-cards/{id}/issue", owner: "curriculum", description: "Issue a report card (locks its entries)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/report-cards/{id}/print-request", owner: "curriculum", description: "Request printing of a report card via print events (no direct printing-module call)." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/report-cards/{id}/entries", owner: "curriculum", description: "List entries of a report card (paginated, filter by subject_id, domain_id, grade_label)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/report-cards/{id}/entries", owner: "curriculum", description: "Add a subject/domain entry to a report card (grade label and descriptor derived from data when not supplied)." },
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/report-cards/entries/{entry_id}", owner: "curriculum", description: "Update a report-card entry." },
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/report-cards/entries/{entry_id}", owner: "curriculum", description: "Remove a report-card entry." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/interventions", owner: "curriculum", description: "List remedial and enrichment interventions (paginated, filter by student_ref, basic_competency_id, kind, status, teacher_id)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/interventions", owner: "curriculum", description: "Create a remedial or enrichment intervention for a student and competency." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/interventions/{id}", owner: "curriculum", description: "Get one intervention." },
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/interventions/{id}", owner: "curriculum", description: "Update an intervention." },
            RouteInfo { method: "DELETE", path: "/api/v1/curriculum/interventions/{id}", owner: "curriculum", description: "Delete an intervention." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/interventions/{id}/complete", owner: "curriculum", description: "Mark an intervention completed (sets completed_at)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/interventions/{id}/cancel", owner: "curriculum", description: "Cancel an intervention." },
            RouteInfo { method: "GET", path: "/api/v1/curriculum/settings", owner: "curriculum", description: "Get curriculum settings (configurable defaults and vocabularies)." },
            RouteInfo { method: "PATCH", path: "/api/v1/curriculum/settings", owner: "curriculum", description: "Update curriculum settings (defaults and vocabularies)." },
            RouteInfo { method: "POST", path: "/api/v1/curriculum/ai/{capability}", owner: "curriculum", description: "Run one registered curriculum AI capability (draft_lesson_plan; requires ai:use plus curriculum:view)." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CurriculumModule::new()));
}
