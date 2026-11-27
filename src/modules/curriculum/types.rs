//! Curriculum module - competency-based school curricula. - Types.
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
use super::module::*;
use super::report_card_entries::*;
use super::report_cards::*;
use super::settings::*;
use super::state::*;
use super::structures::*;
use super::subjects::*;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One curriculum framework (the registry). A newer national
/// curriculum is added as another row here - no code change.
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
/// a lookup cache only - students are owned by the academic module
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

pub(super) fn default_mapping_status() -> String {
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
/// default / vocabulary - change it through `PATCH /settings` and
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
                GradeBand { min_score: 80.0, max_score: 90.0, label: "B".to_string() },
                GradeBand { min_score: 70.0, max_score: 80.0, label: "C".to_string() },
                GradeBand { min_score: 60.0, max_score: 70.0, label: "D".to_string() },
                GradeBand { min_score: 0.0, max_score: 60.0, label: "E".to_string() },
            ],
        }
    }
}
