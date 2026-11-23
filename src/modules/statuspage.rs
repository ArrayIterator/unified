//! Status Page module.
//!
//! Split into one file per concern under `statuspage/`; this file only
//! declares the submodules and re-exports the public API. The full
//! module design notes live at the top of `statuspage/module.rs`.
pub mod ai_capabilities;
pub mod types;
mod helpers;
mod handlers;
pub mod module;

// Re-exports from types.rs.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::OVERALL_OPERATIONAL;
pub use self::types::OVERALL_DEGRADED;
pub use self::types::OVERALL_OUTAGE;
pub use self::types::OVERALL_MAINTENANCE;
pub use self::types::COMPONENT_OPERATIONAL;
pub use self::types::COMPONENT_DEGRADED;
pub use self::types::COMPONENT_PARTIAL_OUTAGE;
pub use self::types::COMPONENT_MAJOR_OUTAGE;
pub use self::types::COMPONENT_MAINTENANCE;
pub use self::types::INCIDENT_INVESTIGATING;
pub use self::types::INCIDENT_IDENTIFIED;
pub use self::types::INCIDENT_MONITORING;
pub use self::types::INCIDENT_RESOLVED;
pub use self::types::PERM_STATUSPAGE_MANAGE;
pub(crate) use self::types::MAX_INCIDENT_TITLE_LEN;
pub(crate) use self::types::MAX_DESCRIPTION_LEN;
pub use self::types::StatusComponent;
pub use self::types::DailyUptime;
pub use self::types::Incident;
pub use self::types::IncidentUpdate;
pub use self::types::CreateComponentRequest;
pub use self::types::UpdateComponentRequest;
pub use self::types::RecordUptimeRequest;
pub use self::types::CreateIncidentRequest;
pub use self::types::UpdateIncidentRequest;
pub use self::types::AddIncidentUpdateRequest;
pub use self::types::ComponentStatusView;
pub use self::types::StatusPageState;

// Re-exports from helpers.rs.
pub(crate) use self::helpers::valid_component_status;
pub(crate) use self::helpers::validate_incident_title;
pub(crate) use self::helpers::validate_description;
pub(crate) use self::helpers::require_statuspage_manage;
pub(crate) use self::helpers::valid_incident_status;
pub(crate) use self::helpers::is_open_incident;
pub(crate) use self::helpers::valid_severity;
pub(crate) use self::helpers::uptime_key;
pub(crate) use self::helpers::round_percent;
pub(crate) use self::helpers::parse_uuid;
pub(crate) use self::helpers::parse_date;
pub(crate) use self::helpers::validate_component_name;
pub(crate) use self::helpers::paginate;
pub(crate) use self::helpers::matches_search;
pub(crate) use self::helpers::render_status_html;
pub(crate) use self::helpers::sorted_components;
pub(crate) use self::helpers::open_incidents;

// Re-exports from handlers.rs.
pub(crate) use self::handlers::public_summary;
pub(crate) use self::handlers::public_page;
pub(crate) use self::handlers::list_components;
pub(crate) use self::handlers::uptime_history;
pub(crate) use self::handlers::list_incidents;
pub(crate) use self::handlers::get_incident;
pub(crate) use self::handlers::list_incident_updates;
pub(crate) use self::handlers::create_component;
pub(crate) use self::handlers::get_component;
pub(crate) use self::handlers::update_component;
pub(crate) use self::handlers::delete_component;
pub(crate) use self::handlers::record_uptime;
pub(crate) use self::handlers::create_incident;
pub(crate) use self::handlers::update_incident;
pub(crate) use self::handlers::resolve_incident;
pub(crate) use self::handlers::add_incident_update;

// Re-exports from module.rs.
pub use self::module::StatusPageModule;
pub use self::module::register;

