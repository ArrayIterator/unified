// TimeTracker module - Time Doctor-style time tracking, activity
// monitoring, screenshots, payroll, and timesheet approvals.

// Full module notes (routes, auth, money, storage) live at
// the top of `module.rs` in this directory.

pub mod ai_capabilities;
mod auth;
pub mod events;
mod handlers;
mod handlers_activity;
mod handlers_admin;
mod handlers_agent;
mod handlers_rates;
mod handlers_staff;
mod handlers_teams;
pub mod module;
pub mod requests;
mod service;
pub mod state;
pub mod types;
mod validation;

pub(crate) use self::auth::require_permission;
pub(crate) use self::auth::{Caller, authenticate_agent, caller, can_view_screenshots};
pub(crate) use self::auth::{can_view_user, claimed_company_id, require_company_member};
pub(crate) use self::auth::{
    caller_company, in_company_scope, in_company_scope_opt, require_company_for_write,
    require_company_match, require_company_match_for, require_record_in_company_scope,
    require_tenant,
};
pub(crate) use self::handlers::update_entry;
pub(crate) use self::handlers::{approve_entry, create_entry, delete_entry, get_active_timer};
pub(crate) use self::handlers::{get_entry, get_status, list_entries, list_timers, pause_timer};
pub(crate) use self::handlers::{reject_entry, resume_timer, start_timer, stop_timer};
pub(crate) use self::handlers_activity::{delete_screenshot, get_screenshot, list_activity};
pub(crate) use self::handlers_activity::{list_screenshots, list_usage, purge_screenshots};
pub(crate) use self::handlers_activity::{report_activity, report_usage};
pub(crate) use self::handlers_admin::update_settings;
pub(crate) use self::handlers_admin::{get_buyer_settings, get_my_settings, get_settings};
pub(crate) use self::handlers_admin::{team_dashboard, update_buyer_settings, update_my_settings};
pub(crate) use self::handlers_agent::agent_usage;
pub(crate) use self::handlers_agent::{agent_activity, agent_heartbeat, agent_start_timer};
pub(crate) use self::handlers_agent::{agent_status, agent_stop_timer, agent_upload_screenshot};
pub(crate) use self::handlers_rates::{approve_timesheet, create_rate, create_timesheet};
pub(crate) use self::handlers_rates::{delete_rate, get_payroll, get_rate, get_timesheet};
pub(crate) use self::handlers_rates::{list_rates, list_timesheets, reject_timesheet};
pub(crate) use self::handlers_rates::{report_daily, report_summary, report_weekly};
pub(crate) use self::handlers_rates::{submit_timesheet, update_rate};
pub(crate) use self::handlers_staff::update_staff;
pub(crate) use self::handlers_staff::{add_staff, create_invite, get_staff, list_invites};
pub(crate) use self::handlers_staff::{list_staff, remove_staff, revoke_invite, staff_activity};
pub(crate) use self::handlers_teams::update_team;
pub(crate) use self::handlers_teams::{add_team_member, create_team, delete_team, get_team};
pub(crate) use self::handlers_teams::{list_team_members, list_teams, remove_team_member};
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::{TimeTrackerModule, register};
pub use self::requests::UpdateTeamRequest;
pub use self::requests::{AddStaffRequest, AddTeamMemberRequest, AgentHeartbeatRequest};
pub use self::requests::{CreateEntryRequest, CreateInviteRequest, CreateRateRequest};
pub use self::requests::{CreateTeamRequest, CreateTimesheetRequest, DeleteEntryRequest};
pub use self::requests::{ReportActivityRequest, ReportUsageRequest, ReviewEntryRequest};
pub use self::requests::{ReviewTimesheetRequest, ScreenshotUploadQuery, StartTimerRequest};
pub use self::requests::{UpdateBuyerSettingsRequest, UpdateEntryRequest, UpdateMySettingsRequest};
pub use self::requests::{UpdateRateRequest, UpdateSettingsRequest, UpdateStaffRequest};
pub(crate) use self::service::{auto_pause_on_idle, compute_payroll, emit_time_logged, load_team};
pub(crate) use self::service::{load_timesheet, on_project_task_completed};
pub(crate) use self::service::{on_project_task_created, on_project_time_logged};
pub(crate) use self::service::{on_screenshots_purge_requested, on_user_created, parse_window};
pub(crate) use self::service::{purge_expired_screenshots, record_activity, record_usage};
pub(crate) use self::service::{report_rows, request_screenshot_purge, staff_company};
pub(crate) use self::service::{staff_overview, stop_timer_inner};
pub use self::state::TimeTrackerState;
pub(crate) use self::types::SCREENSHOT_SIGNED_URL_SECS;
pub(crate) use self::types::{AGENT_RATE_PER_MINUTE, MAX_APP_NAME_LEN, MAX_CURRENCY_LEN};
pub use self::types::{ActivityLevel, ActivitySample, AppUsageRecord, BuyerSettings};
pub use self::types::{EntryApproval, HourlyRate, PERM_APPROVE, PERM_MANAGE};
pub(crate) use self::types::{MAX_DESCRIPTION_LEN, MAX_ENTRY_SECONDS, MAX_INTERVAL_SECONDS};
pub(crate) use self::types::{MAX_NOTE_LEN, MAX_REASON_LEN, MAX_SCREENSHOT_BYTES, MAX_TARGET_LEN};
pub(crate) use self::types::{ONLINE_WINDOW_SECS, SCREENSHOT_RATE_PER_MINUTE};
pub use self::types::{PERM_MANAGE_BUYER_SETTINGS, PERM_MANAGE_RATES, PERM_MANAGE_SETTINGS};
pub use self::types::{PERM_MANAGE_STAFF, PERM_PAYROLL, PERM_READ, PERM_VIEW_TEAM};
pub use self::types::{PERM_VIEW_TEAM_SCREENSHOTS, PERM_WRITE, PayrollLine, ReportRow};
pub use self::types::{ResolvedSettings, ScreenshotRecord, StaffInvite, StaffOverview};
pub use self::types::{StaffSettings, Team, TeamMember, TeamMemberStatus, TimeEntry};
pub use self::types::{TimeTrackerTimer, TimerStatus, Timesheet, TimesheetStatus, TrackerRole};
pub use self::types::{TrackerSettings, UsageCategory, UserTrackerSettings};
pub(crate) use self::validation::{active_timer_for, created, load_entry, load_timer, ok};
pub(crate) use self::validation::{optional_text, paginate, parse_uuid, require_text, round4};
pub(crate) use self::validation::{sha256_hex, team_ids_for_user, team_member_key, valid_currency};
pub(crate) use self::validation::{valid_email, valid_hhmm, valid_timezone, valid_title_hash};
pub(crate) use self::validation::{validate_interval, validate_list};
