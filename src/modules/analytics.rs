//! Analytics: event ingestion, sales/license/health metrics, reports.

pub mod ai_capabilities;
pub mod events;
mod helpers;
pub mod types;
mod views;
mod inputs;
pub mod state;
mod hooks;
mod reports;
mod handlers_dashboard;
mod handlers_definitions;
mod handlers_runs;
mod handlers_scheduled;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::events::ANALYTICS_REPORT_GENERATED;
pub use self::events::ANALYTICS_REPORT_EXPORTED;
pub use self::events::ANALYTICS_REPORT_RUN_REQUESTED;
pub use self::types::SaleRecord;
pub use self::types::LicenseSnapshot;
pub use self::types::MetricSample;
pub use self::types::HealthSample;
pub use self::types::ReportDefinition;
pub use self::types::ReportRun;
pub use self::types::ScheduledReport;
pub use self::state::AnalyticsState;
pub use self::module::AnalyticsModule;
pub use self::module::register;
