// timetracker — Hook event names emitted and listened to by this module.

pub const TIMER_STARTED: &str = "timetracker.timer.started";
pub const TIMER_STOPPED: &str = "timetracker.timer.stopped";
pub const TIMER_PAUSED: &str = "timetracker.timer.paused";
pub const TIMER_RESUMED: &str = "timetracker.timer.resumed";
pub const TIMER_AUTO_PAUSED: &str = "timetracker.timer.auto_paused";
pub const TIME_LOGGED: &str = "timetracker.time.logged";
pub const ENTRY_UPDATED: &str = "timetracker.entry.updated";
pub const ENTRY_DELETED: &str = "timetracker.entry.deleted";
pub const ENTRY_APPROVED: &str = "timetracker.entry.approved";
pub const ENTRY_REJECTED: &str = "timetracker.entry.rejected";
pub const ACTIVITY_REPORTED: &str = "timetracker.activity.reported";
pub const USAGE_REPORTED: &str = "timetracker.usage.reported";
pub const SCREENSHOT_UPLOADED: &str = "timetracker.screenshot.uploaded";
pub const SCREENSHOT_DELETED: &str = "timetracker.screenshot.deleted";
pub const SCREENSHOTS_PURGED: &str = "timetracker.screenshots.purged";
pub const SCREENSHOTS_PURGE_REQUESTED: &str = "timetracker.screenshots.purge_requested";
pub const TIMESHEET_CREATED: &str = "timetracker.timesheet.created";
pub const TIMESHEET_SUBMITTED: &str = "timetracker.timesheet.submitted";
pub const TIMESHEET_APPROVED: &str = "timetracker.timesheet.approved";
pub const TIMESHEET_REJECTED: &str = "timetracker.timesheet.rejected";
pub const RATE_CREATED: &str = "timetracker.rate.created";
pub const RATE_UPDATED: &str = "timetracker.rate.updated";
pub const RATE_DELETED: &str = "timetracker.rate.deleted";
pub const SETTINGS_UPDATED: &str = "timetracker.settings.updated";
/// Scheduler job kind for the retention purge (the scheduler
/// must add this kind to its KNOWN_KINDS list and a dispatch arm
/// that emits `timetracker.screenshots.purge_requested`; see
/// `request_screenshot_purge`).
pub const PURGE_JOB_KIND: &str = "timetracker.screenshots.purge";
pub const STAFF_ADDED: &str = "timetracker.staff.added";
pub const STAFF_UPDATED: &str = "timetracker.staff.updated";
pub const STAFF_REMOVED: &str = "timetracker.staff.removed";
pub const STAFF_INVITE_CREATED: &str = "timetracker.staff.invite.created";
pub const STAFF_INVITE_ACCEPTED: &str = "timetracker.staff.invite.accepted";
pub const STAFF_INVITE_REVOKED: &str = "timetracker.staff.invite.revoked";
pub const TEAM_CREATED: &str = "timetracker.team.created";
pub const TEAM_DELETED: &str = "timetracker.team.deleted";
pub const TEAM_MEMBER_ADDED: &str = "timetracker.team.member.added";
pub const TEAM_MEMBER_REMOVED: &str = "timetracker.team.member.removed";
