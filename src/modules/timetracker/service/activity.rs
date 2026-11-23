// service — activity.
// Activity and usage recording, screenshots, windows.
//
// Split from `src/modules/timetracker/service.rs`; moved verbatim, no behaviour changes.

use super::super::*;
use super::*;
use actix_web::HttpRequest;
use chrono::{DateTime, Datelike, Duration, Utc};
use serde_json::{Value, json};
use std::collections::HashMap;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use unified_core::storage::StorageRegistry;
use uuid::Uuid;
/// Enforce the buyer's application / website allowlist and
/// blocklist on one reported usage interval. An empty allowlist
/// means "everything is allowed" (only the blocklist applies).
fn check_usage_allowed(
    resolved: &ResolvedSettings,
    application_name: &str,
    domain: Option<&str>,
) -> UnifiedResult<()> {
    let app = application_name.trim().to_lowercase();
    let domain = domain.map(str::trim).map(str::to_lowercase);
    if resolved
        .blocked_apps
        .iter()
        .any(|blocked| blocked.trim().to_lowercase() == app)
    {
        return Err(UnifiedError::unprocessable(format!(
            "Application \"{application_name}\" is blocked by the buyer's settings"
        )));
    }
    if !resolved.allowed_apps.is_empty()
        && !resolved
            .allowed_apps
            .iter()
            .any(|allowed| allowed.trim().to_lowercase() == app)
    {
        return Err(UnifiedError::unprocessable(format!(
            "Application \"{application_name}\" is not on the buyer's allowed-applications list"
        )));
    }
    if let Some(domain) = domain {
        if resolved
            .blocked_websites
            .iter()
            .any(|blocked| blocked.trim().to_lowercase() == domain)
        {
            return Err(UnifiedError::unprocessable(format!(
                "Website \"{domain}\" is blocked by the buyer's settings"
            )));
        }
        if !resolved.allowed_websites.is_empty()
            && !resolved
                .allowed_websites
                .iter()
                .any(|allowed| allowed.trim().to_lowercase() == domain)
        {
            return Err(UnifiedError::unprocessable(format!(
                "Website \"{domain}\" is not on the buyer's allowed-websites list"
            )));
        }
    }
    Ok(())
}

/// Ask the scheduler to run the screenshot retention purge: emit
/// `scheduler.enqueue` with kind `timetracker.screenshots.purge`.
/// The scheduler answers `{"queued": true, "job_id": ...}` when it
/// knows the kind. CURRENT LIMIT, stated plainly: the scheduler's
/// `KNOWN_KINDS` list does not contain this kind yet, so today the
/// scheduler declines and the purge runs through
/// `POST /screenshots/purge` instead. The follow-up is to add the
/// kind to `KNOWN_KINDS` plus this dispatch arm in scheduler.rs:
/// `"timetracker.screenshots.purge" => { state.emit(
/// "timetracker.screenshots.purge_requested", payload.clone());
/// Ok(json!({ "dispatched": true, "event":
/// "timetracker.screenshots.purge_requested" })) }`
pub(crate) fn request_screenshot_purge(state: &TimeTrackerState) -> bool {
    let Some(bus) = state.bus() else {
        return false;
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": events::PURGE_JOB_KIND,
            "payload": {},
        }),
    );
    answer
        .results
        .first()
        .and_then(|result| result.get("queued"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Delete every screenshot older than its owner's retention
/// window: the per-buyer retention when the owner belongs to a
/// company, else the global default. Both the metadata record and
/// the stored object are deleted, and the purge is reported with
/// `timetracker.screenshots.purged`. Returns (removed, failed).
pub(crate) async fn purge_expired_screenshots(
    state: &TimeTrackerState,
    registry: &StorageRegistry,
) -> (u64, u64) {
    let now = Utc::now();
    let expired: Vec<ScreenshotRecord> = state
        .screenshots
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|shot| {
            let resolved = state.resolve_settings_for_user(&shot.user_id);
            let cutoff = now - Duration::days(resolved.retention_days.max(1));
            shot.captured_at < cutoff
        })
        .collect();
    let mut removed = 0u64;
    let mut failed = 0u64;
    for shot in expired {
        // Delete the stored object first. Only when that succeeds
        // is the metadata removed: on failure (or with no provider
        // registered at all) the metadata is kept, marked failed,
        // so a later purge can retry the delete instead of
        // orphaning bytes with no record left to retry them.
        let mut object_deleted = false;
        if let Some(provider) = registry.get(&shot.storage_target) {
            if provider.delete(&shot.storage_key).await.is_ok() {
                object_deleted = true;
            }
        }
        let metadata_removed = if object_deleted {
            state.screenshots.remove(&shot.id);
            removed += 1;
            true
        } else {
            failed += 1;
            false
        };
        state.emit(
            events::SCREENSHOT_DELETED,
            json!({
                "screenshot_id": shot.id.to_string(),
                "user_id": shot.user_id,
                "storage_target": shot.storage_target,
                "storage_key": shot.storage_key,
                "reason": "retention_purge",
                "object_deleted": object_deleted,
                "metadata_removed": metadata_removed,
            }),
        );
    }
    state.emit(
        events::SCREENSHOTS_PURGED,
        json!({
            "removed": removed,
            "failed": failed,
            "purged_at": now.to_rfc3339(),
        }),
    );
    (removed, failed)
}

// ---------------------------------------------------------------------------
// Handlers: activity, app usage, screenshots
// ---------------------------------------------------------------------------

/// Record one activity sample for `user_id`. Shared by the
/// dashboard endpoint and the agent endpoint. The buyer's activity
/// level gates it: `Off` rejects every report, `Minimal` and
/// `Full` accept scores. Idle seconds at or above the resolved
/// idle threshold auto-pause the user's running timer (the idle
/// period is recorded on the sample and on the timer).
pub(crate) fn record_activity(
    state: &TimeTrackerState,
    user_id: &str,
    input: ReportActivityRequest,
) -> UnifiedResult<ActivitySample> {
    let resolved = state.resolve_settings_for_user(user_id);
    if resolved.activity_level == ActivityLevel::Off {
        return Err(UnifiedError::unprocessable(
            "Activity tracking is disabled for this company (activity_level off)",
        ));
    }
    if !(0..=100).contains(&input.activity_score) {
        return Err(UnifiedError::bad_request(
            "activity_score must be between 0 and 100",
        ));
    }
    if input.idle_secs < 0 {
        return Err(UnifiedError::bad_request("idle_secs must not be negative"));
    }
    validate_interval(input.interval_start, input.interval_end, MAX_INTERVAL_SECONDS)?;
    if let Some(timer_id) = input.timer_id {
        let timer = state
            .timers
            .get(&timer_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Timer not found: {timer_id}")))?;
        if timer.user_id != user_id {
            return Err(UnifiedError::forbidden(
                "Activity cannot be reported against another user's timer",
            ));
        }
    }
    let now = Utc::now();
    let triggered_auto_pause = input.idle_secs >= resolved.idle_threshold_secs
        && auto_pause_on_idle(state, user_id, input.idle_secs);
    let sample = ActivitySample {
        id: Uuid::now_v7(),
        user_id: user_id.to_string(),
        timer_id: input.timer_id,
        project_id: input.project_id,
        task_id: input.task_id,
        activity_score: input.activity_score,
        idle_secs: input.idle_secs,
        interval_start: input.interval_start,
        interval_end: input.interval_end,
        triggered_auto_pause,
        created_at: now,
    };
    state.activity.insert(sample.id, sample.clone());
    state.emit(
        events::ACTIVITY_REPORTED,
        json!({
            "sample_id": sample.id.to_string(),
            "user_id": user_id,
            "activity_score": sample.activity_score,
            "idle_secs": sample.idle_secs,
            "triggered_auto_pause": triggered_auto_pause,
        }),
    );
    Ok(sample)
}

/// Record one app / website usage interval for `user_id`. The
/// buyer's activity level gates it (`Full` only — `Minimal`
/// collects scores, not applications), then the buyer's
/// allowlist / blocklist is enforced on the application name and
/// the website domain.
pub(crate) fn record_usage(
    state: &TimeTrackerState,
    user_id: &str,
    input: ReportUsageRequest,
) -> UnifiedResult<AppUsageRecord> {
    let resolved = state.resolve_settings_for_user(user_id);
    match resolved.activity_level {
        ActivityLevel::Off => {
            return Err(UnifiedError::unprocessable(
                "Activity tracking is disabled for this company (activity_level off)",
            ));
        }
        ActivityLevel::Minimal => {
            return Err(UnifiedError::unprocessable(
                "App and website usage tracking is disabled for this company (activity_level minimal collects activity scores only)",
            ));
        }
        ActivityLevel::Full => {}
    }
    let application_name = require_text(
        &input.application_name,
        "application_name",
        MAX_APP_NAME_LEN,
    )?;
    let domain = match &input.domain {
        Some(raw) => Some(require_text(raw, "domain", MAX_APP_NAME_LEN)?.to_lowercase()),
        None => None,
    };
    let window_title_hash = match &input.window_title_hash {
        Some(raw) => Some(valid_title_hash(raw)?),
        None => None,
    };
    let category = input.category.unwrap_or_default();
    let duration_secs = validate_interval(
        input.interval_start,
        input.interval_end,
        MAX_INTERVAL_SECONDS,
    )?;
    check_usage_allowed(&resolved, &application_name, domain.as_deref())?;
    let now = Utc::now();
    let record = AppUsageRecord {
        id: Uuid::now_v7(),
        user_id: user_id.to_string(),
        application_name,
        domain,
        window_title_hash,
        category,
        duration_secs,
        interval_start: input.interval_start,
        interval_end: input.interval_end,
        created_at: now,
    };
    state.usage.insert(record.id, record.clone());
    state.emit(
        events::USAGE_REPORTED,
        json!({
            "usage_id": record.id.to_string(),
            "user_id": user_id,
            "application_name": record.application_name,
            "domain": record.domain,
            "category": format!("{:?}", record.category).to_lowercase(),
            "duration_secs": record.duration_secs,
        }),
    );
    Ok(record)
}

/// Parse a `from` / `to` window from query filters. Defaults to
/// the last 7 days. `from` must precede `to` and the window may
/// not exceed 366 days.
pub(crate) fn parse_window(
    params: &PaginationParams,
) -> UnifiedResult<(DateTime<Utc>, DateTime<Utc>)> {
    let now = Utc::now();
    let parse = |raw: &str| {
        DateTime::parse_from_rfc3339(raw)
            .map(|parsed| parsed.with_timezone(&Utc))
            .map_err(|_| UnifiedError::bad_request(format!("Invalid RFC 3339 timestamp: {raw}")))
    };
    let from = match params.filters.get("from") {
        Some(raw) => parse(raw)?,
        None => now - Duration::days(7),
    };
    let to = match params.filters.get("to") {
        Some(raw) => parse(raw)?,
        None => now,
    };
    if to <= from {
        return Err(UnifiedError::bad_request("to must be after from"));
    }
    // Compared on the real duration, not on truncated whole days:
    // a 366-day-plus-23-hour window truncates to 366 days but is
    // longer than the cap.
    if to - from > Duration::days(366) {
        return Err(UnifiedError::bad_request("Window must be at most 366 days"));
    }
    Ok((from, to))
}
