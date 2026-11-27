// Admissions module - new-student registration.
//
// Admission periods per academic year and term, online registration
// through the Forms module, selection stages with weighted scoring,
// result announcement, registration fees through Billing, a document
// checklist through the Documents module, an ordered waitlist, and
// enrolment handoff to the academic system. Interconnection happens
// only through the hook bus - no module calls another module directly.
//
// The full module documentation (roles, status vocabularies, events)
// lives in `admissions/module.rs`.


pub mod ai_capabilities;
pub mod constants;
pub mod types;
pub mod state;
mod helpers;
mod hooks;
mod handlers_periods;
mod handlers_quotas;
mod handlers_stages;
mod handlers_documents;
mod handlers_summary;
mod handlers_applications;
mod handlers_scores;
mod handlers_checklist;
mod handlers_fees;
mod handlers_waitlist;
mod handlers_results;
mod handlers_settings;
pub mod module;

// Public API (unchanged): re-exported from the split files.
// From constants.rs:
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::constants::PERM_ADMISSIONS_APPLY;
pub use self::constants::PERM_ADMISSIONS_MANAGE;
pub use self::constants::PERM_ADMISSIONS_READ;
pub use self::constants::PERM_ADMISSIONS_READ_ALL;
pub use self::constants::PERM_ADMISSIONS_SCORE;
// From types.rs:
pub use self::types::AdmissionPeriod;
pub use self::types::AdmissionResult;
pub use self::types::AdmissionSetting;
pub use self::types::Application;
pub use self::types::ApplicationScore;
pub use self::types::AttachDocument;
pub use self::types::ChecklistItem;
pub use self::types::CreateApplication;
pub use self::types::CreatePeriod;
pub use self::types::CreateQuota;
pub use self::types::CreateRequiredDocument;
pub use self::types::CreateScore;
pub use self::types::CreateStage;
pub use self::types::DecideApplication;
pub use self::types::FeeAction;
pub use self::types::PeriodQuota;
pub use self::types::PromoteWaitlist;
pub use self::types::RejectChecklistItem;
pub use self::types::RequiredDocument;
pub use self::types::ScoreBreakdown;
pub use self::types::ScoreTotal;
pub use self::types::StageDefinition;
pub use self::types::UpdateApplication;
pub use self::types::UpdatePeriod;
pub use self::types::UpdateQuota;
pub use self::types::UpdateRequiredDocument;
pub use self::types::UpdateScore;
pub use self::types::UpdateSetting;
pub use self::types::UpdateStage;
pub use self::types::WaitlistEntry;
// From state.rs:
pub use self::state::AdmissionsModule;
pub use self::state::AdmissionsState;
// From module.rs:
pub use self::module::register;
