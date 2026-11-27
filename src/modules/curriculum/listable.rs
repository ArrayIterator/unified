//! Curriculum module - competency-based school curricula. - Listable implementations (search text + exact-match filters).
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
use super::types::*;

use chrono::{DateTime, Utc};


impl Listable for CurriculumFramework {
    fn search_text(&self) -> String {
        vec![self.code.clone(), self.name.clone(), self.description.clone()].join(" ")
    }
    fn filter_value(&self, key: &str) -> Option<String> {
        match key {
            "code" => Some(self.code.clone()),
            "country" => Some(self.country.clone()),
            "is_active" => Some(self.is_active.to_string()),
            "is_built_in" => Some(self.is_built_in.to_string()),
            _ => None,
        }
    }
    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

impl Listable for SchoolLevel {
    fn search_text(&self) -> String {
        vec![self.code.clone(), self.name.clone()].join(" ")
    }
    fn filter_value(&self, key: &str) -> Option<String> {
        match key {
            "framework_id" => Some(self.framework_id.to_string()),
            "code" => Some(self.code.clone()),
            _ => None,
        }
    }
    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

impl Listable for Subject {
    fn search_text(&self) -> String {
        vec![self.code.clone(), self.name.clone()].join(" ")
    }
    fn filter_value(&self, key: &str) -> Option<String> {
        match key {
            "framework_id" => Some(self.framework_id.to_string()),
            "level_id" => Some(self.level_id.to_string()),
            "code" => Some(self.code.clone()),
            _ => None,
        }
    }
    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

impl Listable for CoreCompetency {
    fn search_text(&self) -> String {
        vec![self.code.clone(), self.title.clone(), self.description.clone()].join(" ")
    }
    fn filter_value(&self, key: &str) -> Option<String> {
        match key {
            "framework_id" => Some(self.framework_id.to_string()),
            "subject_id" => Some(self.subject_id.to_string()),
            "level_id" => Some(self.level_id.to_string()),
            "code" => Some(self.code.clone()),
            "grade_year" => Some(self.grade_year.to_string()),
            "version" => Some(self.version.to_string()),
            _ => None,
        }
    }
    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

impl Listable for BasicCompetency {
    fn search_text(&self) -> String {
        vec![self.code.clone(), self.title.clone(), self.description.clone()].join(" ")
    }
    fn filter_value(&self, key: &str) -> Option<String> {
        match key {
            "framework_id" => Some(self.framework_id.to_string()),
            "core_competency_id" => Some(self.core_competency_id.to_string()),
            "code" => Some(self.code.clone()),
            "grade_year" => Some(self.grade_year.to_string()),
            "version" => Some(self.version.to_string()),
            _ => None,
        }
    }
    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

impl Listable for CurriculumStructure {
    fn search_text(&self) -> String {
        vec![self.framework_id.to_string(), self.subject_id.to_string()].join(" ")
    }
    fn filter_value(&self, key: &str) -> Option<String> {
        match key {
            "framework_id" => Some(self.framework_id.to_string()),
            "level_id" => Some(self.level_id.to_string()),
            "grade_year" => Some(self.grade_year.to_string()),
            "subject_id" => Some(self.subject_id.to_string()),
            _ => None,
        }
    }
    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

impl Listable for AssessmentDomain {
    fn search_text(&self) -> String {
        vec![self.code.clone(), self.name.clone(), self.description.clone()].join(" ")
    }
    fn filter_value(&self, key: &str) -> Option<String> {
        match key {
            "code" => Some(self.code.clone()),
            "parent_domain_id" => self.parent_domain_id.map(|value| value.to_string()),
            "is_active" => Some(self.is_active.to_string()),
            _ => None,
        }
    }
    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

impl Listable for GradeDescriptor {
    fn search_text(&self) -> String {
        vec![self.grade_label.clone(), self.descriptor.clone()].join(" ")
    }
    fn filter_value(&self, key: &str) -> Option<String> {
        match key {
            "framework_id" => Some(self.framework_id.to_string()),
            "domain_id" => self.domain_id.map(|value| value.to_string()),
            "grade_label" => Some(self.grade_label.clone()),
            _ => None,
        }
    }
    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

impl Listable for LessonPlan {
    fn search_text(&self) -> String {
        vec![self.title.clone(), self.assessment_notes.clone()].join(" ")
    }
    fn filter_value(&self, key: &str) -> Option<String> {
        match key {
            "framework_id" => Some(self.framework_id.to_string()),
            "subject_id" => Some(self.subject_id.to_string()),
            "core_competency_id" => self.core_competency_id.map(|value| value.to_string()),
            "basic_competency_id" => self.basic_competency_id.map(|value| value.to_string()),
            "grade_year" => Some(self.grade_year.to_string()),
            "semester" => Some(self.semester.to_string()),
            "teacher_id" => Some(self.teacher_id.clone()),
            "status" => Some(self.status.clone()),
            _ => None,
        }
    }
    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

impl Listable for ReportCard {
    fn search_text(&self) -> String {
        vec![self.student_ref.clone(), self.academic_year.clone()].join(" ")
    }
    fn filter_value(&self, key: &str) -> Option<String> {
        match key {
            "student_ref" => Some(self.student_ref.clone()),
            "framework_id" => Some(self.framework_id.to_string()),
            "level_id" => Some(self.level_id.to_string()),
            "grade_year" => Some(self.grade_year.to_string()),
            "semester" => Some(self.semester.to_string()),
            "academic_year" => Some(self.academic_year.clone()),
            "status" => Some(self.status.clone()),
            _ => None,
        }
    }
    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

impl Listable for ReportCardEntry {
    fn search_text(&self) -> String {
        vec![self.grade_label.clone().unwrap_or_default(), self.notes.clone()].join(" ")
    }
    fn filter_value(&self, key: &str) -> Option<String> {
        match key {
            "report_card_id" => Some(self.report_card_id.to_string()),
            "subject_id" => Some(self.subject_id.to_string()),
            "domain_id" => Some(self.domain_id.to_string()),
            "grade_label" => self.grade_label.clone(),
            "academic_grade_id" => self.academic_grade_id.clone(),
            _ => None,
        }
    }
    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

impl Listable for Intervention {
    fn search_text(&self) -> String {
        vec![self.student_ref.clone(), self.kind.clone(), self.notes.clone()].join(" ")
    }
    fn filter_value(&self, key: &str) -> Option<String> {
        match key {
            "student_ref" => Some(self.student_ref.clone()),
            "basic_competency_id" => Some(self.basic_competency_id.to_string()),
            "kind" => Some(self.kind.clone()),
            "status" => Some(self.status.clone()),
            "teacher_id" => Some(self.teacher_id.clone()),
            _ => None,
        }
    }
    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}