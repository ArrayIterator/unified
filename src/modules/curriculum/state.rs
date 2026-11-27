//! Curriculum module - competency-based school curricula. - State.
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
use super::structures::*;
use super::subjects::*;
use super::types::*;

use unified_core::hooks::HookBus;
use dashmap::DashMap;
use serde_json::Value;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

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
    pub(super) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(super) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(super) fn settings(&self) -> CurriculumSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }
}
