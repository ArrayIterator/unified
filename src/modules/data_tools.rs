// Data Tools module — orphaned-data resolver, expired-data purging,
// and cleanup rules + runs.
//
// Full module notes (routes, ownership, hook contract) live at
// the top of `module.rs` in this directory.

pub mod ai_capabilities;
pub mod events;
mod handlers;
pub mod module;
pub mod state;
pub mod types;
mod validation;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub(crate) use self::events::on_cleanup_requested;
pub(crate) use self::handlers::admin_page;
pub(crate) use self::handlers::list_rules;
pub(crate) use self::handlers::list_runs;
pub(crate) use self::handlers::run_now;
pub(crate) use self::handlers::scan_now;
pub(crate) use self::handlers::update_rule;
pub use self::module::DataToolsModule;
pub use self::module::register;
pub use self::state::DataToolsState;
pub use self::types::CleanupRule;
pub use self::types::CleanupRun;
pub use self::types::DocumentShareRecord;
pub(crate) use self::types::PERM_ADMIN;
pub(crate) use self::types::PERM_READ;
pub use self::types::RulePatch;
pub use self::types::RunRequest;
pub use self::types::ScanReport;
pub use self::types::TARGETS;
pub use self::types::TargetOutcome;
pub(crate) use self::validation::authorize;
pub(crate) use self::validation::escape_html;
pub(crate) use self::validation::new_id;
pub(crate) use self::validation::now;
pub(crate) use self::validation::paginate;
