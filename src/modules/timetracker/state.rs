// timetracker - In-memory state and its methods.

use super::*;
use unified_core::hooks::HookBus;
use unified_core::storage::StorageRegistry;
use dashmap::DashMap;
use serde_json::Value;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use uuid::Uuid;

// Shared state

/// In-memory stores for TimeTracker. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission, except the capability
/// and membership checks, which fail closed when there is no bus
/// to ask. Durable rows for these types are defined by migration
/// `2026-11-22-000032_timetracker`; until that migration lands,
/// nothing here survives a restart.
#[derive(Clone)]
pub struct TimeTrackerState {
    pub timers: Arc<DashMap<Uuid, TimeTrackerTimer>>,
    pub entries: Arc<DashMap<Uuid, TimeEntry>>,
    pub activity: Arc<DashMap<Uuid, ActivitySample>>,
    pub usage: Arc<DashMap<Uuid, AppUsageRecord>>,
    pub screenshots: Arc<DashMap<Uuid, ScreenshotRecord>>,
    pub rates: Arc<DashMap<Uuid, HourlyRate>>,
    pub timesheets: Arc<DashMap<Uuid, Timesheet>>,
    pub settings: Arc<RwLock<TrackerSettings>>,
    pub user_settings: Arc<DashMap<String, UserTrackerSettings>>,
    pub teams: Arc<DashMap<Uuid, Team>>,
    /// Keyed by `"{team_id}:{user_id}"`.
    pub team_members: Arc<DashMap<String, TeamMember>>,
    /// Keyed by user id. One staff record per user.
    pub staff: Arc<DashMap<String, StaffSettings>>,
    pub invites: Arc<DashMap<Uuid, StaffInvite>>,
    /// Keyed by company id.
    pub buyer_settings: Arc<DashMap<Uuid, BuyerSettings>>,
    /// Per-key per-minute agent call buckets: key id -> (window
    /// start, calls in this window).
    pub agent_buckets: Arc<DashMap<String, (Instant, u32)>>,
    /// One mutex per user, serialising timer starts for that user:
    /// the check for an existing active timer and the insert of the
    /// new timer happen under the same lock, so two concurrent
    /// starts can never both pass the check (see
    /// `insert_timer_if_none_active`).
    pub timer_start_locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
    /// Shared storage registry, attached in `init` (the registry
    /// lives in `AppContext`). The purge hook handler needs it
    /// without an HTTP request, so it is kept here next to the bus.
    pub storage_registry: Arc<RwLock<Option<Arc<StorageRegistry>>>>,
}

impl Default for TimeTrackerState {
    fn default() -> Self {
        Self {
            timers: Arc::new(DashMap::new()),
            entries: Arc::new(DashMap::new()),
            activity: Arc::new(DashMap::new()),
            usage: Arc::new(DashMap::new()),
            screenshots: Arc::new(DashMap::new()),
            rates: Arc::new(DashMap::new()),
            timesheets: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(TrackerSettings::default())),
            user_settings: Arc::new(DashMap::new()),
            teams: Arc::new(DashMap::new()),
            team_members: Arc::new(DashMap::new()),
            staff: Arc::new(DashMap::new()),
            invites: Arc::new(DashMap::new()),
            buyer_settings: Arc::new(DashMap::new()),
            agent_buckets: Arc::new(DashMap::new()),
            timer_start_locks: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
            storage_registry: Arc::new(RwLock::new(None)),
        }
    }
}

impl TimeTrackerState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(crate) fn global_settings(&self) -> TrackerSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    pub(crate) fn user_settings_for(&self, user_id: &str) -> UserTrackerSettings {
        self.user_settings
            .get(user_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| UserTrackerSettings::for_user(user_id))
    }

    pub(crate) fn buyer_settings_for(&self, company_id: Uuid) -> BuyerSettings {
        self.buyer_settings
            .get(&company_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| BuyerSettings::for_company(company_id))
    }

    /// The per-user timer-start mutex for `user_id`, creating it
    /// on first use. The returned `Arc` must be kept alive while
    /// its guard is held.
    pub(crate) fn timer_start_lock(&self, user_id: &str) -> Arc<Mutex<()>> {
        self.timer_start_locks
            .entry(user_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .value()
            .clone()
    }

    /// Insert `timer` only when the user has no active timer,
    /// atomically: the active-timer check and the insert run under
    /// the user's timer-start lock, so two concurrent starts for
    /// the same user cannot both succeed. Returns false (and
    /// inserts nothing) when an active timer already exists.
    /// Callers must use this instead of checking
    /// `active_timer_for` and then inserting.
    pub(crate) fn insert_timer_if_none_active(&self, timer: TimeTrackerTimer) -> bool {
        let lock = self.timer_start_lock(&timer.user_id);
        let _guard = lock.lock().expect("timer start lock poisoned");
        if active_timer_for(self, &timer.user_id).is_some() {
            return false;
        }
        self.timers.insert(timer.id, timer);
        true
    }

    /// Resolve the effective settings for one user: the user's own
    /// settings win first (idle threshold and blur flag), then the
    /// per-staff override, then the buyer's setting, then the
    /// global product default. This is the only settings lookup the
    /// screenshot, activity, idle, and retention paths use. The
    /// user's own `screenshots_enabled` consent flag is deliberately
    /// NOT folded in here: consent stays a separate gate (see the
    /// agent upload and the heartbeat's effective value), so the
    /// staff / buyer switch and the user's consent remain
    /// distinguishable.
    pub(crate) fn resolve_settings_for_user(&self, user_id: &str) -> ResolvedSettings {
        let global = self.global_settings();
        let user = self.user_settings_for(user_id);
        let staff = self.staff.get(user_id).map(|entry| entry.value().clone());
        let company_id = staff.as_ref().map(|record| record.company_id);
        let buyer = company_id.map(|id| self.buyer_settings_for(id));
        let screenshots_enabled = staff
            .as_ref()
            .and_then(|record| record.screenshots_enabled)
            .or_else(|| buyer.as_ref().and_then(|settings| settings.screenshots_enabled))
            .unwrap_or(true);
        let screenshot_interval_secs = buyer
            .as_ref()
            .and_then(|settings| settings.screenshot_interval_secs)
            .unwrap_or(300);
        let activity_level = staff
            .as_ref()
            .and_then(|record| record.activity_level)
            .or_else(|| buyer.as_ref().and_then(|settings| settings.activity_level))
            .unwrap_or_default();
        let idle_threshold_secs = user
            .idle_threshold_secs
            .or_else(|| staff.as_ref().and_then(|record| record.idle_threshold_secs))
            .or_else(|| buyer.as_ref().and_then(|settings| settings.idle_timeout_secs))
            .unwrap_or(global.idle_threshold_secs);
        let blur_screenshots = user.blur_screenshots
            || staff
                .as_ref()
                .and_then(|record| record.blur_screenshots)
                .or_else(|| buyer.as_ref().and_then(|settings| settings.blur_screenshots))
                .unwrap_or(false);
        let allowed_apps = buyer
            .as_ref()
            .map(|settings| settings.allowed_apps.clone())
            .unwrap_or_default();
        let blocked_apps = buyer
            .as_ref()
            .map(|settings| settings.blocked_apps.clone())
            .unwrap_or_default();
        let allowed_websites = buyer
            .as_ref()
            .map(|settings| settings.allowed_websites.clone())
            .unwrap_or_default();
        let blocked_websites = buyer
            .as_ref()
            .map(|settings| settings.blocked_websites.clone())
            .unwrap_or_default();
        let timezone = staff
            .as_ref()
            .and_then(|record| record.timezone.clone())
            .or_else(|| buyer.as_ref().and_then(|settings| settings.timezone.clone()))
            .unwrap_or_else(|| "UTC".to_string());
        let working_hours_start = staff
            .as_ref()
            .and_then(|record| record.working_hours_start.clone())
            .or_else(|| {
                buyer
                    .as_ref()
                    .and_then(|settings| settings.working_hours_start.clone())
            });
        let working_hours_end = staff
            .as_ref()
            .and_then(|record| record.working_hours_end.clone())
            .or_else(|| {
                buyer
                    .as_ref()
                    .and_then(|settings| settings.working_hours_end.clone())
            });
        let retention_days = buyer
            .as_ref()
            .and_then(|settings| settings.retention_days)
            .unwrap_or(global.retention_days);
        ResolvedSettings {
            company_id,
            screenshots_enabled,
            screenshot_interval_secs,
            activity_level,
            idle_threshold_secs,
            blur_screenshots,
            allowed_apps,
            blocked_apps,
            allowed_websites,
            blocked_websites,
            timezone,
            working_hours_start,
            working_hours_end,
            retention_days,
        }
    }

    /// Resolve the effective settings for a whole company (no
    /// per-staff layer): buyer setting, then global default.
    pub(crate) fn resolve_settings_for_company(&self, company_id: Uuid) -> ResolvedSettings {
        let global = self.global_settings();
        let buyer = self.buyer_settings_for(company_id);
        ResolvedSettings {
            company_id: Some(company_id),
            screenshots_enabled: buyer.screenshots_enabled.unwrap_or(true),
            screenshot_interval_secs: buyer.screenshot_interval_secs.unwrap_or(300),
            activity_level: buyer.activity_level.unwrap_or_default(),
            idle_threshold_secs: buyer
                .idle_timeout_secs
                .unwrap_or(global.idle_threshold_secs),
            blur_screenshots: buyer.blur_screenshots.unwrap_or(false),
            allowed_apps: buyer.allowed_apps.clone(),
            blocked_apps: buyer.blocked_apps.clone(),
            allowed_websites: buyer.allowed_websites.clone(),
            blocked_websites: buyer.blocked_websites.clone(),
            timezone: buyer.timezone.clone().unwrap_or_else(|| "UTC".to_string()),
            working_hours_start: buyer.working_hours_start.clone(),
            working_hours_end: buyer.working_hours_end.clone(),
            retention_days: buyer.retention_days.unwrap_or(global.retention_days),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn staff_for(user_id: &str, company_id: Uuid) -> StaffSettings {
        let now = Utc::now();
        StaffSettings {
            user_id: user_id.to_string(),
            company_id,
            role: TrackerRole::Member,
            manager_id: None,
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

    fn timer_for(user_id: &str) -> TimeTrackerTimer {
        let now = Utc::now();
        TimeTrackerTimer {
            id: Uuid::new_v4(),
            user_id: user_id.to_string(),
            project_id: None,
            task_id: None,
            description: None,
            billable: true,
            status: TimerStatus::Running,
            started_at: now,
            paused_total_secs: 0,
            paused_at: None,
            idle_total_secs: 0,
            stopped_at: None,
            entry_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn user_idle_threshold_beats_staff_buyer_and_global() {
        let state = TimeTrackerState::default();
        let company = Uuid::new_v4();
        // Staff says 600, buyer says 900, global default is 300.
        let mut staff = staff_for("u1", company);
        staff.idle_threshold_secs = Some(600);
        state.staff.insert("u1".to_string(), staff);
        let mut buyer = BuyerSettings::for_company(company);
        buyer.idle_timeout_secs = Some(900);
        state.buyer_settings.insert(company, buyer);
        assert_eq!(
            state.resolve_settings_for_user("u1").idle_threshold_secs,
            600
        );
        // The user's own override wins over all three.
        let mut mine = state.user_settings_for("u1");
        mine.idle_threshold_secs = Some(120);
        state.user_settings.insert("u1".to_string(), mine);
        assert_eq!(
            state.resolve_settings_for_user("u1").idle_threshold_secs,
            120
        );
        // A user without any override falls through to the global default.
        assert_eq!(
            state.resolve_settings_for_user("u2").idle_threshold_secs,
            300
        );
    }

    #[test]
    fn user_blur_flag_is_included_in_resolved_settings() {
        let state = TimeTrackerState::default();
        assert!(!state.resolve_settings_for_user("u1").blur_screenshots);
        let mut mine = state.user_settings_for("u1");
        mine.blur_screenshots = true;
        state.user_settings.insert("u1".to_string(), mine);
        assert!(state.resolve_settings_for_user("u1").blur_screenshots);
    }

    #[test]
    fn timer_start_lock_is_per_user() {
        let state = TimeTrackerState::default();
        assert!(Arc::ptr_eq(
            &state.timer_start_lock("u1"),
            &state.timer_start_lock("u1")
        ));
        assert!(!Arc::ptr_eq(
            &state.timer_start_lock("u1"),
            &state.timer_start_lock("u2")
        ));
    }

    #[test]
    fn insert_timer_if_none_active_rejects_a_second_active_timer() {
        let state = TimeTrackerState::default();
        assert!(state.insert_timer_if_none_active(timer_for("u1")));
        assert!(!state.insert_timer_if_none_active(timer_for("u1")));
        // Another user is unaffected.
        assert!(state.insert_timer_if_none_active(timer_for("u2")));
        // Once the first timer is stopped, a new start succeeds.
        let mut stored = state
            .timers
            .iter()
            .map(|entry| entry.value().clone())
            .find(|timer| timer.user_id == "u1")
            .unwrap();
        stored.status = TimerStatus::Stopped;
        state.timers.insert(stored.id, stored);
        assert!(state.insert_timer_if_none_active(timer_for("u1")));
    }

    #[test]
    fn concurrent_timer_starts_for_one_user_create_only_one_timer() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let state = Arc::new(TimeTrackerState::default());
        let successes = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let state = Arc::clone(&state);
            let successes = Arc::clone(&successes);
            handles.push(std::thread::spawn(move || {
                if state.insert_timer_if_none_active(timer_for("u1")) {
                    successes.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }
        for handle in handles {
            handle.join().expect("timer start thread panicked");
        }
        assert_eq!(successes.load(Ordering::SeqCst), 1);
        assert_eq!(
            state
                .timers
                .iter()
                .filter(|entry| entry.value().user_id == "u1")
                .count(),
            1
        );
    }
}
