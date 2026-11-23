// Risk Mitigation module: a risk register with mitigation plans,
// compliance checklists, and threshold alerts.
//
// Split into one file per concern. The scoring formula and band
// thresholds live in `types.rs`; every public item is
// re-exported below so `crate::modules::risk::<Item>` keeps
// working unchanged.

pub mod ai_capabilities;
pub mod types;
mod store;
mod common;
mod handlers_risks;
mod handlers_plans;
mod handlers_templates;
mod handlers_instances;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::BAND_LOW_MAX;
pub use self::types::BAND_MEDIUM_MAX;
pub use self::types::BAND_HIGH_MAX;
pub use self::types::DEFAULT_ALERT_THRESHOLD;
pub use self::types::band_for;
pub use self::types::score_for;
pub use self::types::RISK_CATEGORIES;
pub use self::types::RISK_STATUSES;
pub use self::types::PLAN_STATUSES;
pub use self::types::TASK_STATUSES;
pub use self::types::Risk;
pub use self::types::CreateRisk;
pub use self::types::UpdateRisk;
pub use self::types::MitigationPlan;
pub use self::types::CreateMitigationPlan;
pub use self::types::UpdateMitigationPlan;
pub use self::types::MitigationTask;
pub use self::types::CreateMitigationTask;
pub use self::types::UpdateMitigationTask;
pub use self::types::ChecklistTemplate;
pub use self::types::CreateChecklistTemplate;
pub use self::types::UpdateChecklistTemplate;
pub use self::types::ChecklistTemplateItem;
pub use self::types::CreateChecklistTemplateItem;
pub use self::types::ChecklistInstance;
pub use self::types::CreateChecklistInstance;
pub use self::types::ChecklistInstanceItem;
pub use self::types::CompleteChecklistItem;
pub use self::types::RiskAlert;
pub use self::types::ActorQuery;
pub use self::types::RiskState;
pub use self::module::RiskModule;
pub use self::module::register;
