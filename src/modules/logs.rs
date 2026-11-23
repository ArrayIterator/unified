// Logs module: central log viewer.
//
// Split into one file per concern. The ingestion sources and
// redaction rules live in `types.rs` and `redaction.rs`; every
// public item is re-exported below so
// `crate::modules::logs::<Item>` keeps working unchanged.

pub mod ai_capabilities;
pub mod redaction;
pub mod types;
pub mod store;
pub mod service;
pub mod tracing;
mod common;
mod handlers;
mod hooks;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::redaction::LOG_LEVELS;
pub use self::redaction::LOG_KINDS;
pub use self::redaction::DEFAULT_RETENTION_DAYS;
pub use self::redaction::DEFAULT_MAX_ENTRIES;
pub use self::types::LogEntry;
pub use self::types::LogsSettings;
pub use self::types::PruneReport;
pub use self::types::RecordLogRequest;
pub use self::types::UpdateLogsSettings;
pub use self::types::PruneRequest;
pub use self::types::ExportQuery;
pub use self::store::LogsState;
pub use self::service::global_state;
pub use self::service::push_log;
pub use self::service::push_log_full;
pub use self::service::record_access;
pub use self::service::record_error;
pub use self::tracing::LogsLayer;
pub use self::module::LogsModule;
pub use self::module::register;
