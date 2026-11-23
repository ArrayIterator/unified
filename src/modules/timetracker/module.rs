// TimeTracker module — Time Doctor-style time tracking, activity
// monitoring, screenshots, payroll, and timesheet approvals.
//
// Routes live under `/api/v1/timetracker` (browser / dashboard
// callers, capability-checked through the roles module) and
// `/api/v1/timetracker/agent/*` (desktop-agent callers, API-key
// authenticated through the apikeys module):
//
//   Status:
//     GET  / — module status and counts.
//   Timers:
//     GET  /timers — list timers (own timers; managers may filter
//       by user_id with `timetracker:view_team`).
//     POST /timers/start — start a timer (one active timer per user).
//     POST /timers/{id}/stop, /pause, /resume.
//     GET  /timers/active — the caller's active timer, if any.
//   Manual time entries:
//     GET/POST /entries — list / create a manual entry (with a
//       reason; approval state starts at `pending`).
//     GET/PATCH/DELETE /entries/{id} — read / edit / delete (edit
//       and delete only while the entry is still `pending`).
//     POST /entries/{id}/approve, /reject — approval with a note.
//   Activity (agent-reported):
//     GET/POST /activity — activity samples: score 0-100 per
//       interval plus agent-reported idle seconds.
//     GET/POST /usage — application / website usage intervals.
//   Screenshots:
//     GET  /screenshots, /screenshots/{id} — metadata plus a
//       short-lived signed URL (never raw bytes in the envelope).
//     DELETE /screenshots/{id} — delete metadata and the stored object.
//     POST /screenshots/purge — retention purge (also the manual
//       trigger for the scheduled purge; see below).
//   Payroll and rates:
//     GET/POST /rates, GET/PATCH/DELETE /rates/{id} — hourly rates
//       per user (currency + amount).
//     GET  /payroll — payable hours x rate, with overtime.
//   Timesheets:
//     GET/POST /timesheets, GET /timesheets/{id}.
//     POST /timesheets/{id}/submit, /approve, /reject.
//   Reports and monitoring:
//     GET  /reports/daily, /reports/weekly, /reports/summary.
//     GET  /dashboard/team — online now, today's activity,
//       low-activity flags.
//   Settings:
//     GET/PUT /settings — global settings (admin).
//     GET/PUT /settings/me — the caller's own tracking settings,
//       including the screenshot consent flag.
//   Agent endpoints (API-key authenticated):
//     POST /agent/heartbeat — presence + idle report; may auto-pause
//       the caller's timer past the idle threshold.
//     POST /agent/activity, /agent/usage — agent-reported samples.
//     POST /agent/screenshots — screenshot upload (raw bytes).
//     GET  /agent/status — the agent's own settings, consent, and
//       active timer.
//     POST /agent/timers/start, /agent/timers/stop.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// the server side of time tracking: timers, manual entries,
// approvals, activity and app-usage records, screenshot metadata
// with bytes stored through the storage module, hourly rates,
// payroll computation, timesheets, reports, and team monitoring.
// What is NOT built here: the desktop agent itself. No binary
// captures keystrokes, mouse movement, application windows, or
// screen pixels in this codebase; the agent endpoints below only
// receive and validate what an external agent reports (activity
// scores, idle seconds, application names, window-title hashes,
// screenshot bytes). Until a desktop agent exists, the agent
// endpoints can be exercised by any HTTP client holding a scoped
// API key, which is exactly how the agent will call them later.
// Follow-up (not started here): build the desktop agent (per-OS
// capture of input activity, foreground application, and periodic
// screenshots) and have it call `/agent/*` with a scoped key.
//
// Project / task linkage is by id only: timers and entries carry
// optional `project_id` / `task_id` (UUIDs from the projects
// module) and this module never imports or calls the projects
// module. All cross-module contact goes through the hook bus.
//
// Events emitted on the hook bus:
//   - `timetracker.timer.started`, `timetracker.timer.stopped`,
//     `timetracker.timer.paused`, `timetracker.timer.resumed`,
//     `timetracker.timer.auto_paused` (idle threshold reached)
//   - `timetracker.time.logged` — one entry became billable-tracked
//     time (manual create and timer stop); a manual edit re-emits it
//     with the new duration, and a delete emits a compensating void
//     payload (`"void": true`) for the same entry id, so billing can
//     re-price or reverse the earlier pricing (see the payload shape
//     in the `emit_time_logged` helper; billing does not listen yet —
//     the integrator adds a handler there).
//   - `timetracker.entry.updated`, `timetracker.entry.deleted`,
//     `timetracker.entry.approved`, `timetracker.entry.rejected`
//   - `timetracker.activity.reported`, `timetracker.usage.reported`
//   - `timetracker.screenshot.uploaded`,
//     `timetracker.screenshot.deleted`,
//     `timetracker.screenshots.purged`
//   - `timetracker.timesheet.created`,
//     `timetracker.timesheet.submitted`,
//     `timetracker.timesheet.approved`,
//     `timetracker.timesheet.rejected`
//   - `timetracker.rate.created`, `timetracker.rate.updated`,
//     `timetracker.rate.deleted`
//   - `timetracker.settings.updated`
// Events this module listens to:
//   - `project.task.created` — acknowledged only (the task id is
//     returned so the caller can verify the link; no record is
//     created here). The name is confirmed by the audit module's
//     audited-events list; the projects module file itself was not
//     present in the staging tree when this module was written, so
//     the handler tolerates any payload shape.
//   - `project.task.completed` — any running timer linked to that
//     task is stopped automatically (the work is done; the timer
//     must not keep running) and its entry is logged.
//   - `project.time.logged` — acknowledged only. The projects
//     module keeps its own time entries; importing them here would
//     double-count billable hours, so this handler records nothing
//     and only answers `{"acknowledged": true}`.
//   - `timetracker.screenshots.purge_requested` — the retention
//     purge itself (deletes expired screenshot metadata and the
//     stored objects). The scheduler is expected to emit this event
//     through a `timetracker.screenshots.purge` job kind; see the
//     note on `request_screenshot_purge` — the scheduler's
//     `KNOWN_KINDS` list does not contain that kind yet, so today
//     the purge runs through `POST /screenshots/purge` instead.
//   - `timetracker.company.membership.check` — answered from this
//     module's own staff map and team memberships (see
//     `on_company_membership_check`); the company module has no
//     membership hook, so nothing else can answer it.
//   - `timetracker.team.check` — answered from this module's own
//     teams, team memberships, and staff manager relations (see
//     `on_team_check`); there is no separate team module.
// Events this module emits to other modules:
//   - `role.check` with `{"role", "permission", "allowed"}` — the
//     roles module answers capability checks (same shape as the
//     ai module's `check_capability`).
//   - `timetracker.team.check` with `{"manager_id", "user_id"}`,
//     expecting `{"manages": true}` in the first hook result. A
//     manager may only see another user's time, activity, or
//     screenshots when this module answers that the user is on
//     their team. Answered by this module itself (see
//     `on_team_check`): there is no separate team module, this
//     module owns teams, so it answers from its own `team_members`
//     and staff `manager_id` relations. Fail-closed: no answer
//     means not allowed.
//   - `timetracker.company.membership.check` with
//     `{"company_id", "user_id"}`, expecting `{"member": true}`.
//     Answered by this module itself (see
//     `on_company_membership_check`) from its own staff map and
//     team memberships: the company module answers only
//     `company.exists` and has no membership hook to fall back to,
//     so a caller counts as a member when they hold a staff record
//     for the company or sit on one of its teams. Fail-closed.
//   - `scheduler.enqueue` with `{"kind":
//     "timetracker.screenshots.purge", "payload": {}}` — asks the
//     scheduler to run the retention purge (see
//     `request_screenshot_purge`).
//   - `storage.object.uploaded` / `storage.object.deleted` are NOT
//     emitted here: the storage module owns those events and emits
//     them on its own object routes. Screenshot uploads go through
//     the storage provider directly (see `upload_screenshot`), so
//     no duplicate storage event is faked.
//
// Screenshots and privacy:
//   - Screenshot bytes are NEVER stored in this module's tables:
//     only metadata (storage target + key, size, content type,
//     capture time, blur flag) lives here; bytes go to the storage
//     module's provider named by settings (`storage_target`,
//     default `"timetracker-screenshots"`). The target must exist;
//     a missing target fails the upload with 503 instead of
//     silently storing bytes somewhere else.
//   - Consent: screenshots are disabled for a user until that user
//     sets `screenshots_enabled` (their own `/settings/me`). The
//     agent upload is rejected with 403 without consent.
//   - Blur: when the user's `blur_screenshots` flag is set, the
//     agent is expected to blur the image before upload and the
//     record is marked `blurred: true`; the server records the flag
//     but cannot verify pixel content — the agent is trusted to
//     honour it, and the flag is visible on every screenshot record
//     so a manager can see which captures were blurred.
//   - Visibility: a user sees only their own screenshots. A manager
//     sees a team member's screenshots only with the
//     `timetracker:view_team_screenshots` capability AND a positive
//     `timetracker.team.check` answer. Screenshot reads return a
//     short-lived signed URL (15 minutes), never the bytes.
//   - Retention: screenshots older than `retention_days`
//     (default 90) are deleted — metadata and stored object — by
//     the purge. The purge never deletes anything younger than the
//     retention window, and every deletion is reported in the
//     `timetracker.screenshots.purged` event payload.
//
// Money: hourly rates and payroll amounts are `f64` in memory
// (the billing module's in-memory convention) and NUMERIC(19,4)
// in the migration. Amounts are rounded to 4 decimal places on
// write; the Diesel persistence types use `PgNumeric` (see the
// schema / models snippets in the module report).
//
// Actor identity: browser / dashboard callers send `X-User-Id`
// and `X-Role` (the ai module's shape); agent callers send an API
// key as `Authorization: Bearer <key>` or `X-Api-Key`, and the
// agent's user is the key's owner id (the apikeys module's
// `authenticate_key` + `key_has_scope`).

use super::*;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai;
use actix_web::web;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Module definition
// ---------------------------------------------------------------------------

pub struct TimeTrackerModule {
    state: TimeTrackerState,
}

impl TimeTrackerModule {
    pub fn new() -> Self {
        Self {
            state: TimeTrackerState::default(),
        }
    }
}

impl Default for TimeTrackerModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for TimeTrackerModule {
    fn id(&self) -> &'static str {
        "timetracker"
    }

    fn name(&self) -> &'static str {
        "TimeTracker"
    }

    fn description(&self) -> &'static str {
        "Time Doctor-style time tracking: timers, manual entries, approvals, activity and app-usage monitoring, screenshots through the storage module with consent, blur, and retention controls, hourly rates, payroll with overtime, timesheets, reports, team monitoring, staff and team management per company, and per-buyer settings. The desktop and mobile agent apps are a planned future product; the server-side agent API under /api/v1/timetracker/agent/* is what they will call."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "storage", "billing", "notifications", "scheduler"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "storage", "billing", "notifications", "scheduler"]
    }

    /// Sellable-product declaration (batch 2, moved here from the
    /// inherent methods: the `Module` trait now declares these, so
    /// the registry reads this override). Available as a product
    /// in the productivity category.
    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("timetracker", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        *self
            .state
            .storage_registry
            .write()
            .expect("storage registry lock poisoned") =
            Some(Arc::clone(&ctx.storage_registry));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "project.task.completed",
            "timetracker",
            Arc::new(move |ctx: &mut HookContext| {
                on_project_task_completed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "project.task.created",
            "timetracker",
            Arc::new(move |ctx: &mut HookContext| {
                on_project_task_created(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "project.time.logged",
            "timetracker",
            Arc::new(move |ctx: &mut HookContext| {
                on_project_time_logged(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "user.created",
            "timetracker",
            Arc::new(move |ctx: &mut HookContext| {
                on_user_created(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            events::SCREENSHOTS_PURGE_REQUESTED,
            "timetracker",
            Arc::new(move |ctx: &mut HookContext| {
                on_screenshots_purge_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "timetracker.company.membership.check",
            "timetracker",
            Arc::new(move |ctx: &mut HookContext| {
                on_company_membership_check(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "timetracker.team.check",
            "timetracker",
            Arc::new(move |ctx: &mut HookContext| {
                on_team_check(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/timetracker")
                .app_data(state)
                .route("", web::get().to(get_status))
                .route("/timers", web::get().to(list_timers))
                .route("/timers/active", web::get().to(get_active_timer))
                .route("/timers/start", web::post().to(start_timer))
                .route("/timers/{id}/stop", web::post().to(stop_timer))
                .route("/timers/{id}/pause", web::post().to(pause_timer))
                .route("/timers/{id}/resume", web::post().to(resume_timer))
                .route("/entries", web::get().to(list_entries))
                .route("/entries", web::post().to(create_entry))
                .route("/entries/{id}", web::get().to(get_entry))
                .route("/entries/{id}", web::patch().to(update_entry))
                .route("/entries/{id}", web::delete().to(delete_entry))
                .route("/entries/{id}/approve", web::post().to(approve_entry))
                .route("/entries/{id}/reject", web::post().to(reject_entry))
                .route("/activity", web::get().to(list_activity))
                .route("/activity", web::post().to(report_activity))
                .route("/usage", web::get().to(list_usage))
                .route("/usage", web::post().to(report_usage))
                .route("/screenshots", web::get().to(list_screenshots))
                .route("/screenshots/{id}", web::get().to(get_screenshot))
                .route("/screenshots/{id}", web::delete().to(delete_screenshot))
                .route("/screenshots/purge", web::post().to(purge_screenshots))
                .route("/rates", web::get().to(list_rates))
                .route("/rates", web::post().to(create_rate))
                .route("/rates/{id}", web::get().to(get_rate))
                .route("/rates/{id}", web::patch().to(update_rate))
                .route("/rates/{id}", web::delete().to(delete_rate))
                .route("/payroll", web::get().to(get_payroll))
                .route("/timesheets", web::get().to(list_timesheets))
                .route("/timesheets", web::post().to(create_timesheet))
                .route("/timesheets/{id}", web::get().to(get_timesheet))
                .route("/timesheets/{id}/submit", web::post().to(submit_timesheet))
                .route("/timesheets/{id}/approve", web::post().to(approve_timesheet))
                .route("/timesheets/{id}/reject", web::post().to(reject_timesheet))
                .route("/reports/daily", web::get().to(report_daily))
                .route("/reports/weekly", web::get().to(report_weekly))
                .route("/reports/summary", web::get().to(report_summary))
                .route("/dashboard/team", web::get().to(team_dashboard))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/settings/me", web::get().to(get_my_settings))
                .route("/settings/me", web::put().to(update_my_settings))
                .route("/settings/buyer", web::get().to(get_buyer_settings))
                .route("/settings/buyer", web::put().to(update_buyer_settings))
                .route("/staff/invites", web::get().to(list_invites))
                .route("/staff/invites", web::post().to(create_invite))
                .route("/staff/invites/{id}/revoke", web::post().to(revoke_invite))
                .route("/staff", web::get().to(list_staff))
                .route("/staff", web::post().to(add_staff))
                .route("/staff/{user_id}", web::get().to(get_staff))
                .route("/staff/{user_id}", web::patch().to(update_staff))
                .route("/staff/{user_id}", web::delete().to(remove_staff))
                .route("/staff/{user_id}/activity", web::get().to(staff_activity))
                .route("/teams", web::get().to(list_teams))
                .route("/teams", web::post().to(create_team))
                .route("/teams/{id}", web::get().to(get_team))
                .route("/teams/{id}", web::patch().to(update_team))
                .route("/teams/{id}", web::delete().to(delete_team))
                .route("/teams/{id}/members", web::get().to(list_team_members))
                .route("/teams/{id}/members", web::post().to(add_team_member))
                .route(
                    "/teams/{id}/members/{user_id}",
                    web::delete().to(remove_team_member),
                )
                .route("/agent/heartbeat", web::post().to(agent_heartbeat))
                .route("/agent/activity", web::post().to(agent_activity))
                .route("/agent/usage", web::post().to(agent_usage))
                .route("/agent/screenshots", web::post().to(agent_upload_screenshot))
                .route("/agent/status", web::get().to(agent_status))
                .route("/agent/timers/start", web::post().to(agent_start_timer))
                .route("/agent/timers/stop", web::post().to(agent_stop_timer)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/timetracker", owner: "timetracker", description: "Module status and counts.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/timers", owner: "timetracker", description: "List timers (own timers; managers may filter by user_id with timetracker:view_team).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/timers/active", owner: "timetracker", description: "The caller's active timer, if any, with elapsed seconds.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/timers/start", owner: "timetracker", description: "Start a timer (one active timer per user).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/timers/{id}/stop", owner: "timetracker", description: "Stop a timer and create its time entry (emits timetracker.time.logged).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/timers/{id}/pause", owner: "timetracker", description: "Pause a running timer.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/timers/{id}/resume", owner: "timetracker", description: "Resume a paused timer.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/entries", owner: "timetracker", description: "List time entries (paginated, filter by user_id, approval, project_id, billable).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/entries", owner: "timetracker", description: "Create a manual time entry with a reason (starts pending; emits timetracker.time.logged).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/entries/{id}", owner: "timetracker", description: "Get one time entry.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/timetracker/entries/{id}", owner: "timetracker", description: "Edit a pending entry (reason required).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/timetracker/entries/{id}", owner: "timetracker", description: "Delete a non-approved entry (reason required).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/entries/{id}/approve", owner: "timetracker", description: "Approve a pending entry (approver cannot be the owner).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/entries/{id}/reject", owner: "timetracker", description: "Reject a pending entry with a note.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/activity", owner: "timetracker", description: "List activity samples (paginated, filter by user_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/activity", owner: "timetracker", description: "Report one activity sample (score 0-100, idle seconds; may auto-pause the timer).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/usage", owner: "timetracker", description: "List app/website usage intervals (paginated, filter by user_id, category).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/usage", owner: "timetracker", description: "Report one app/website usage interval (window-title hash only, never the raw title).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/screenshots", owner: "timetracker", description: "List screenshot metadata (own screenshots; team screenshots need timetracker:view_team_screenshots).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/screenshots/{id}", owner: "timetracker", description: "Get one screenshot's metadata plus a 15-minute signed URL (never raw bytes).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/timetracker/screenshots/{id}", owner: "timetracker", description: "Delete a screenshot's metadata and its stored object.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/screenshots/purge", owner: "timetracker", description: "Run the retention purge now (deletes screenshots older than each owner's retention window).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/rates", owner: "timetracker", description: "List hourly rates (paginated, filter by user_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/rates", owner: "timetracker", description: "Create an hourly rate for a user (currency + amount).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/rates/{id}", owner: "timetracker", description: "Get one hourly rate.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/timetracker/rates/{id}", owner: "timetracker", description: "Update an hourly rate.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/timetracker/rates/{id}", owner: "timetracker", description: "Delete an hourly rate.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/payroll", owner: "timetracker", description: "Payroll lines for approved entries in a window (payable hours x rate, weekly overtime).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/timesheets", owner: "timetracker", description: "List timesheets (paginated, filter by user_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/timesheets", owner: "timetracker", description: "Create a timesheet for a period (explicit entries or all pending entries in the period).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/timesheets/{id}", owner: "timetracker", description: "Get one timesheet.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/timesheets/{id}/submit", owner: "timetracker", description: "Submit a draft timesheet for approval.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/timesheets/{id}/approve", owner: "timetracker", description: "Approve a submitted timesheet (also approves its pending entries).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/timesheets/{id}/reject", owner: "timetracker", description: "Reject a submitted timesheet with a note.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/reports/daily", owner: "timetracker", description: "Daily report rows per user/project (at most one day per call).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/reports/weekly", owner: "timetracker", description: "Weekly report rows per user/project (at most 8 days per call).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/reports/summary", owner: "timetracker", description: "Summary totals (billable vs non-billable) plus report rows for a window.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/dashboard/team", owner: "timetracker", description: "Team monitoring: online now, today's activity, low-activity flags (company-scoped).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/settings", owner: "timetracker", description: "Read the global product-default settings.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/timetracker/settings", owner: "timetracker", description: "Update the global product-default settings.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/settings/me", owner: "timetracker", description: "Read the caller's own tracking settings plus their resolved settings.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/timetracker/settings/me", owner: "timetracker", description: "Update the caller's own tracking settings (including screenshot consent).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/settings/buyer", owner: "timetracker", description: "Read a company's per-buyer settings overrides plus the resolved values (company-scoped).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/timetracker/settings/buyer", owner: "timetracker", description: "Update a company's per-buyer settings (company admin, own company only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/staff/invites", owner: "timetracker", description: "List staff invites for the caller's company.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/staff/invites", owner: "timetracker", description: "Invite a person to join the company as tracker staff (token shown once).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/staff/invites/{id}/revoke", owner: "timetracker", description: "Revoke a pending staff invite.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/staff", owner: "timetracker", description: "List staff of the caller's company with activity overviews (filter by role, team_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/staff", owner: "timetracker", description: "Add an existing user as tracker staff of the caller's company.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/staff/{user_id}", owner: "timetracker", description: "Get one staff member with their activity overview.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/timetracker/staff/{user_id}", owner: "timetracker", description: "Update a staff member's tracker role, manager, and per-staff setting overrides.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/timetracker/staff/{user_id}", owner: "timetracker", description: "Remove a staff member (tracker link and team memberships only; history is kept).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/staff/{user_id}/activity", owner: "timetracker", description: "Per-staff activity overview for a window (totals, average activity, idle seconds).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/teams", owner: "timetracker", description: "List teams of the caller's company.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/teams", owner: "timetracker", description: "Create a team in the caller's company.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/teams/{id}", owner: "timetracker", description: "Get one team.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/timetracker/teams/{id}", owner: "timetracker", description: "Update a team.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/timetracker/teams/{id}", owner: "timetracker", description: "Delete a team (memberships are removed with it).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/teams/{id}/members", owner: "timetracker", description: "List a team's members.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/teams/{id}/members", owner: "timetracker", description: "Add a staff member to a team.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/timetracker/teams/{id}/members/{user_id}", owner: "timetracker", description: "Remove a member from a team.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/agent/heartbeat", owner: "timetracker", description: "Agent heartbeat: presence + idle report; may auto-pause the timer (API-key authenticated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/agent/activity", owner: "timetracker", description: "Agent activity report (API-key authenticated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/agent/usage", owner: "timetracker", description: "Agent app/website usage report (API-key authenticated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/agent/screenshots", owner: "timetracker", description: "Agent screenshot upload, max 5 MiB, png/jpeg/webp (API-key authenticated; consent + buyer switch required).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timetracker/agent/status", owner: "timetracker", description: "Agent status: resolved settings, consent, active timer (API-key authenticated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/agent/timers/start", owner: "timetracker", description: "Agent timer start (API-key authenticated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/timetracker/agent/timers/stop", owner: "timetracker", description: "Agent timer stop (API-key authenticated; stops the active timer when no id is given).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(TimeTrackerModule::new()));
}

// ---------------------------------------------------------------------------
// Hook handlers: membership and team checks
//
// These two listeners answer the checks this module's own auth
// code emits. They live here (not in `service.rs`) because
// `register_hooks` needs them and the parent module file is the
// only place that can reference them without a new re-export.
// ---------------------------------------------------------------------------

/// True when `user_id` belongs to `company_id` in this module's
/// own records: a staff record for the company, or membership in
/// one of the company's teams. The company module answers only
/// `company.exists`; it has no membership hook, so there is no
/// cross-module fallback to consult — the staff map plus team
/// memberships are the source of truth here.
pub(crate) fn is_company_member(state: &TimeTrackerState, company_id: Uuid, user_id: &str) -> bool {
    if state
        .staff
        .get(user_id)
        .map(|record| record.company_id == company_id)
        .unwrap_or(false)
    {
        return true;
    }
    state.team_members.iter().any(|entry| {
        let member = entry.value();
        member.user_id == user_id
            && state
                .teams
                .get(&member.team_id)
                .map(|team| team.company_id == company_id)
                .unwrap_or(false)
    })
}

/// True when `manager_id` may view `user_id`'s tracked data: the
/// user themself, the user's direct manager on their staff record,
/// the manager of any team the user sits on, or a manager / admin
/// member of such a team.
pub(crate) fn manages_user(state: &TimeTrackerState, manager_id: &str, user_id: &str) -> bool {
    if manager_id == user_id {
        return true;
    }
    if state
        .staff
        .get(user_id)
        .map(|record| record.manager_id.as_deref() == Some(manager_id))
        .unwrap_or(false)
    {
        return true;
    }
    team_ids_for_user(state, user_id).iter().any(|team_id| {
        if state
            .teams
            .get(team_id)
            .map(|team| team.manager_id.as_deref() == Some(manager_id))
            .unwrap_or(false)
        {
            return true;
        }
        state
            .team_members
            .get(&team_member_key(*team_id, manager_id))
            .map(|member| matches!(member.role, TrackerRole::Manager | TrackerRole::Admin))
            .unwrap_or(false)
    })
}

/// Answer `timetracker.company.membership.check` with
/// `{"member": true}` when the user belongs to the company.
/// Missing or malformed ids answer `false` (fail closed).
pub(crate) fn on_company_membership_check(state: TimeTrackerState, ctx: &mut HookContext) {
    let company_id = ctx
        .data
        .get("company_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let user_id = ctx
        .data
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let member = match (company_id, user_id) {
        (Some(company_id), Some(user_id)) => is_company_member(&state, company_id, &user_id),
        _ => false,
    };
    ctx.add_result(json!({ "member": member }));
}

/// Answer `timetracker.team.check` with `{"manages": true}` when
/// the manager may view the user's tracked data. Missing or
/// malformed ids answer `false` (fail closed).
pub(crate) fn on_team_check(state: TimeTrackerState, ctx: &mut HookContext) {
    let manager_id = ctx
        .data
        .get("manager_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let user_id = ctx
        .data
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let manages = match (manager_id, user_id) {
        (Some(manager_id), Some(user_id)) => manages_user(&state, &manager_id, &user_id),
        _ => false,
    };
    ctx.add_result(json!({ "manages": manages }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn staff_for(user_id: &str, company_id: Uuid, manager_id: Option<&str>) -> StaffSettings {
        let now = Utc::now();
        StaffSettings {
            user_id: user_id.to_string(),
            company_id,
            role: TrackerRole::Member,
            manager_id: manager_id.map(str::to_string),
            screenshots_enabled: None,
            blur_screenshots: None,
            idle_threshold_secs: None,
            activity_level: None,
            timezone: None,
            working_hours_start: None,
            working_hours_end: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn team_for(company_id: Uuid, manager_id: Option<&str>) -> Team {
        let now = Utc::now();
        Team {
            id: Uuid::new_v4(),
            company_id,
            name: "Core".to_string(),
            description: None,
            manager_id: manager_id.map(str::to_string),
            created_at: now,
            updated_at: now,
        }
    }

    fn check(state: &TimeTrackerState, event: &str, payload: serde_json::Value) -> bool {
        let mut ctx = HookContext::new(event, payload);
        let state = state.clone();
        match event {
            "timetracker.company.membership.check" => on_company_membership_check(state, &mut ctx),
            "timetracker.team.check" => on_team_check(state, &mut ctx),
            // Unknown check event: no handler runs, no result is
            // recorded, and the caller below denies by default.
            _ => {}
        }
        ctx.results
            .first()
            .and_then(|result| {
                let key = if event.contains("membership") {
                    "member"
                } else {
                    "manages"
                };
                result.get(key).and_then(|value| value.as_bool())
            })
            .unwrap_or(false)
    }

    #[test]
    fn membership_check_answers_from_staff_map() {
        let state = TimeTrackerState::default();
        let company = Uuid::new_v4();
        state
            .staff
            .insert("u1".to_string(), staff_for("u1", company, None));
        assert!(check(
            &state,
            "timetracker.company.membership.check",
            json!({ "company_id": company.to_string(), "user_id": "u1" })
        ));
        assert!(!check(
            &state,
            "timetracker.company.membership.check",
            json!({ "company_id": company.to_string(), "user_id": "u2" })
        ));
        assert!(!check(
            &state,
            "timetracker.company.membership.check",
            json!({ "company_id": Uuid::new_v4().to_string(), "user_id": "u1" })
        ));
        // Malformed payload answers false, never errors.
        assert!(!check(
            &state,
            "timetracker.company.membership.check",
            json!({ "company_id": "not-a-uuid", "user_id": "u1" })
        ));
    }

    #[test]
    fn membership_check_answers_from_team_membership() {
        let state = TimeTrackerState::default();
        let company = Uuid::new_v4();
        let team = team_for(company, None);
        state.teams.insert(team.id, team.clone());
        state.team_members.insert(
            team_member_key(team.id, "u9"),
            TeamMember {
                team_id: team.id,
                user_id: "u9".to_string(),
                role: TrackerRole::Member,
                joined_at: Utc::now(),
            },
        );
        assert!(check(
            &state,
            "timetracker.company.membership.check",
            json!({ "company_id": company.to_string(), "user_id": "u9" })
        ));
    }

    #[test]
    fn team_check_answers_from_team_manager_and_direct_manager() {
        let state = TimeTrackerState::default();
        let company = Uuid::new_v4();
        // Direct manager relation on the staff record.
        state
            .staff
            .insert("u1".to_string(), staff_for("u1", company, Some("boss")));
        assert!(check(
            &state,
            "timetracker.team.check",
            json!({ "manager_id": "boss", "user_id": "u1" })
        ));
        assert!(!check(
            &state,
            "timetracker.team.check",
            json!({ "manager_id": "stranger", "user_id": "u1" })
        ));
        // Team manager relation.
        let team = team_for(company, Some("lead"));
        state.teams.insert(team.id, team.clone());
        state.team_members.insert(
            team_member_key(team.id, "u2"),
            TeamMember {
                team_id: team.id,
                user_id: "u2".to_string(),
                role: TrackerRole::Member,
                joined_at: Utc::now(),
            },
        );
        assert!(check(
            &state,
            "timetracker.team.check",
            json!({ "manager_id": "lead", "user_id": "u2" })
        ));
        // A fellow member is not a manager.
        state.team_members.insert(
            team_member_key(team.id, "u3"),
            TeamMember {
                team_id: team.id,
                user_id: "u3".to_string(),
                role: TrackerRole::Member,
                joined_at: Utc::now(),
            },
        );
        assert!(!check(
            &state,
            "timetracker.team.check",
            json!({ "manager_id": "u3", "user_id": "u2" })
        ));
        // A manager-role member of the same team may view members.
        state.team_members.insert(
            team_member_key(team.id, "u4"),
            TeamMember {
                team_id: team.id,
                user_id: "u4".to_string(),
                role: TrackerRole::Manager,
                joined_at: Utc::now(),
            },
        );
        assert!(check(
            &state,
            "timetracker.team.check",
            json!({ "manager_id": "u4", "user_id": "u2" })
        ));
        // Self always manages self.
        assert!(check(
            &state,
            "timetracker.team.check",
            json!({ "manager_id": "u2", "user_id": "u2" })
        ));
    }
}
