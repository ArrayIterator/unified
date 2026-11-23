//! Advisor: rule-based business suggestions from invoices, payments, licenses.

pub mod ai_capabilities;
pub mod rules;
pub mod types;
pub mod state;
mod helpers;
mod handlers_suggestions;
mod handlers_rules;
mod hooks;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::rules::RuleDefinition;
pub use self::rules::RULE_DEFINITIONS;
pub use self::types::InvoiceFact;
pub use self::types::PaymentFact;
pub use self::types::LicenseFact;
pub use self::types::CrmActivitySnapshot;
pub use self::types::SubscriptionSnapshot;
pub use self::types::Suggestion;
pub use self::types::RuleRun;
pub use self::types::EvaluateRequest;
pub use self::types::SnoozeRequest;
pub use self::types::ActorQuery;
pub use self::state::AdvisorState;
pub use self::module::AdvisorModule;
pub use self::module::register;
