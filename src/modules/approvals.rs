// Generic Approvals module — data-driven approval workflows for any
// record in any module, without duplicating another module's data.

// Split into per-concern files under `src/modules/approvals/`; the full
// module documentation is preserved at the top of `module.rs`.

pub mod ai_capabilities;
mod constants;
pub mod types;
pub mod store;
mod helpers;
mod validation;
mod service;
mod handlers_definitions;
mod handlers_requests;
mod handlers_dashboards;
mod hooks;
pub mod module;

// Re-exported from `types`.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::StepApprover;
pub use self::types::ApprovalDefinition;
pub use self::types::ApprovalStep;
pub use self::types::ApprovalRequest;
pub use self::types::ApprovalDecision;
pub use self::types::ApprovalDelegation;
pub use self::types::ApprovalEscalation;
pub use self::types::ApprovalSettings;
pub use self::types::ApprovalRequestDetail;
pub use self::types::ApprovalStepView;
pub use self::types::CreateDefinitionInput;
pub use self::types::UpdateDefinitionInput;
pub use self::types::CreateStepInput;
pub use self::types::CreateRequestInput;
pub use self::types::DecideInput;
pub use self::types::ReassignInput;
pub use self::types::CreateDelegationInput;
pub use self::types::UpdateSettingsInput;

// Re-exported from `store`.
pub use self::store::ApprovalsState;

// Re-exported from `module`.
pub use self::module::ApprovalsModule;
pub use self::module::register;
