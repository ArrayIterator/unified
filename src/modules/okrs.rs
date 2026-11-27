// OKR / Goals module - objectives and key results at company,
// team, and personal levels, with cycles / quarters, check-ins,
// cycle-end scoring, computed status, alignment (objective to
// parent objective, key result to project / task), and progress
// roll-up computed from linked work.

// Split into per-concern files under `src/modules/okrs/`; the full
// module documentation is preserved at the top of `module.rs`.

pub mod ai_capabilities;
mod constants;
pub mod types;
pub mod store;
mod helpers;
mod validation;
mod service;
mod handlers_cycles;
mod handlers_objectives;
mod handlers_key_results;
mod hooks;
pub mod module;

// Re-exported from `types`.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::CycleStatus;
pub use self::types::OkrCycle;
pub use self::types::OkrObjective;
pub use self::types::OkrKeyResult;
pub use self::types::OkrCheckin;
pub use self::types::OkrLink;
pub use self::types::ObjectiveView;
pub use self::types::KeyResultView;
pub use self::types::CreateCycleRequest;
pub use self::types::CreateObjectiveRequest;
pub use self::types::UpdateObjectiveRequest;
pub use self::types::CreateKeyResultRequest;
pub use self::types::UpdateKeyResultRequest;
pub use self::types::CreateCheckinRequest;
pub use self::types::OverrideRequest;
pub use self::types::CreateLinkRequest;

// Re-exported from `store`.
pub use self::store::OkrState;

// Re-exported from `module`.
pub use self::module::OkrsModule;
pub use self::module::register;
