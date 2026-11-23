// Compliance Center module — GDPR / ISO support: data export
// requests, data deletion requests with an approval step, an
// append-only consent log, retention policies, processing-activity
// records (RoPA), compliance checklists with evidence, and a breach
// log.
//
// Full module notes (routes, honesty notes) live at the
// top of `src/modules/compliance/module.rs`.

pub mod ai_capabilities;
mod breaches;
mod checklists;
mod consents;
pub mod constants;
mod deletion_listeners;
mod deletions;
mod exports;
mod helpers;
pub mod module;
mod records;
pub mod requests;
mod retention;
pub mod state;
pub mod types;

// Re-exported from `constants`.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::constants::COMPLIANCE_RETENTION_JOB_KIND;
// Re-exported from `types`.
pub use self::types::BreachLog;
pub use self::types::BreachTimelineEntry;
pub use self::types::ComplianceChecklist;
pub use self::types::ConsentLog;
pub use self::types::DeletionItem;
pub use self::types::DeletionRequest;
pub use self::types::DeletionStatus;
pub use self::types::ExportRequest;
pub use self::types::ExportStatus;
pub use self::types::ProcessingRecord;
pub use self::types::RetentionPolicy;
pub use self::types::RetentionReview;
// Re-exported from `requests`.
pub use self::requests::AddTimelineRequest;
pub use self::requests::CompleteExportRequest;
pub use self::requests::CreateBreachRequest;
pub use self::requests::CreateChecklistRequest;
pub use self::requests::CreateDeletionRequest;
pub use self::requests::CreateExportRequest;
pub use self::requests::CreateProcessingRecordRequest;
pub use self::requests::CreateRetentionPolicyRequest;
pub use self::requests::DecisionRequest;
pub use self::requests::DeletionItemInput;
pub use self::requests::FailExportRequest;
pub use self::requests::MarkItemExecutedRequest;
pub use self::requests::RecordConsentRequest;
pub use self::requests::UpdateBreachRequest;
pub use self::requests::UpdateChecklistRequest;
pub use self::requests::UpdateProcessingRecordRequest;
pub use self::requests::UpdateRetentionPolicyRequest;
// Re-exported from `state`.
pub use self::state::ComplianceState;
// Re-exported from `module`.
pub use self::module::register;
pub use self::module::ComplianceModule;
