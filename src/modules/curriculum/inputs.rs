//! Curriculum module - competency-based school curricula. - Input shapes.
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
use super::types::*;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(super) struct FrameworkInput {
    pub(super) code: Option<String>,
    pub(super) name: Option<String>,
    pub(super) description: Option<String>,
    pub(super) country: Option<String>,
    pub(super) is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(super) struct LevelInput {
    pub(super) framework_id: Option<String>,
    pub(super) code: Option<String>,
    pub(super) name: Option<String>,
    pub(super) grade_count: Option<u32>,
    pub(super) sort_order: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct SubjectInput {
    pub(super) framework_id: Option<String>,
    pub(super) level_id: Option<String>,
    pub(super) code: Option<String>,
    pub(super) name: Option<String>,
    pub(super) sort_order: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct CoreCompetencyInput {
    pub(super) framework_id: Option<String>,
    pub(super) subject_id: Option<String>,
    pub(super) level_id: Option<String>,
    pub(super) code: Option<String>,
    pub(super) title: Option<String>,
    pub(super) description: Option<String>,
    pub(super) grade_year: Option<u32>,
    pub(super) version: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct BasicCompetencyInput {
    pub(super) framework_id: Option<String>,
    pub(super) core_competency_id: Option<String>,
    pub(super) code: Option<String>,
    pub(super) title: Option<String>,
    pub(super) description: Option<String>,
    pub(super) grade_year: Option<u32>,
    pub(super) version: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct StructureInput {
    pub(super) framework_id: Option<String>,
    pub(super) level_id: Option<String>,
    pub(super) grade_year: Option<u32>,
    pub(super) subject_id: Option<String>,
    pub(super) hours_per_week: Option<f64>,
    pub(super) sort_order: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AssessmentDomainInput {
    pub(super) code: Option<String>,
    pub(super) name: Option<String>,
    pub(super) parent_domain_id: Option<String>,
    pub(super) description: Option<String>,
    pub(super) sort_order: Option<i32>,
    pub(super) is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GradeDescriptorInput {
    pub(super) framework_id: Option<String>,
    pub(super) domain_id: Option<String>,
    pub(super) min_score: Option<f64>,
    pub(super) max_score: Option<f64>,
    pub(super) grade_label: Option<String>,
    pub(super) descriptor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct LessonPlanInput {
    pub(super) framework_id: Option<String>,
    pub(super) subject_id: Option<String>,
    pub(super) core_competency_id: Option<String>,
    pub(super) basic_competency_id: Option<String>,
    pub(super) title: Option<String>,
    pub(super) objectives: Option<Vec<String>>,
    pub(super) materials: Option<Vec<String>>,
    pub(super) activities: Option<Vec<String>>,
    pub(super) assessment_notes: Option<String>,
    pub(super) time_minutes: Option<u32>,
    pub(super) grade_year: Option<u32>,
    pub(super) semester: Option<u32>,
    pub(super) teacher_id: Option<String>,
    pub(super) status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ReportCardInput {
    pub(super) student_ref: Option<String>,
    pub(super) framework_id: Option<String>,
    pub(super) level_id: Option<String>,
    pub(super) grade_year: Option<u32>,
    pub(super) semester: Option<u32>,
    pub(super) academic_year: Option<String>,
    pub(super) status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ReportCardEntryInput {
    pub(super) subject_id: Option<String>,
    pub(super) domain_id: Option<String>,
    pub(super) score: Option<f64>,
    pub(super) grade_label: Option<String>,
    pub(super) descriptor: Option<String>,
    pub(super) notes: Option<String>,
    pub(super) academic_grade_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ComponentMappingInput {
    pub(super) academic_component_id: Option<String>,
    pub(super) basic_competency_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct InterventionInput {
    pub(super) student_ref: Option<String>,
    pub(super) basic_competency_id: Option<String>,
    pub(super) kind: Option<String>,
    pub(super) status: Option<String>,
    pub(super) teacher_id: Option<String>,
    pub(super) scheduled_at: Option<String>,
    pub(super) notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct SettingsInput {
    pub(super) default_framework_code: Option<String>,
    pub(super) default_paper_size: Option<String>,
    pub(super) paper_sizes: Option<Vec<String>>,
    pub(super) semesters: Option<Vec<u32>>,
    pub(super) intervention_kinds: Option<Vec<String>>,
    pub(super) lesson_plan_statuses: Option<Vec<String>>,
    pub(super) report_card_statuses: Option<Vec<String>>,
    pub(super) intervention_statuses: Option<Vec<String>>,
    pub(super) default_grade_bands: Option<Vec<GradeBand>>,
}
