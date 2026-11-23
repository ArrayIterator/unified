// Curriculum module — competency-based school curricula.
//
// Full module notes (routes, honesty notes) live at the
// top of `src/modules/curriculum/module.rs`.

pub mod ai_capabilities;
mod assessment_domains;
mod basic_competencies;
mod constants;
mod core_competencies;
mod frameworks;
mod grade_component_mappings;
mod grade_descriptors;
mod helpers;
mod hooks;
mod inputs;
mod interventions;
mod lesson_plans;
mod levels;
mod listable;
pub mod module;
mod report_card_entries;
mod report_cards;
mod settings;
pub mod state;
mod structures;
mod subjects;
pub mod types;

// Re-exported from `types`.
pub use self::types::AssessmentDomain;
pub use self::types::BasicCompetency;
pub use self::types::ComponentCompetencyMapping;
pub use self::types::CoreCompetency;
pub use self::types::CurriculumFramework;
pub use self::types::CurriculumSettings;
pub use self::types::CurriculumStructure;
pub use self::types::GradeBand;
pub use self::types::GradeDescriptor;
pub use self::types::Intervention;
pub use self::types::KnownEnrollment;
pub use self::types::KnownGrade;
pub use self::types::KnownStudent;
pub use self::types::LessonPlan;
pub use self::types::ReportCard;
pub use self::types::ReportCardEntry;
pub use self::types::SchoolLevel;
pub use self::types::Subject;
// Re-exported from `state`.
pub use self::state::CurriculumState;
// Re-exported from `ai_capabilities`.
pub use self::ai_capabilities::AI_CAPABILITIES;
// Re-exported from `module`.
pub use self::module::register;
pub use self::module::CurriculumModule;
