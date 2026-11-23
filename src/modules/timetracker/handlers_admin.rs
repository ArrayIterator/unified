// timetracker — HTTP handlers: settings, domains, and hook handlers.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) async fn team_dashboard(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW_TEAM)?;
        let company_id = claimed_company_id(&req)?;
        require_company_member(&ctx, &caller.actor, company_id)?;
        let global = state.global_settings();
        let now = Utc::now();
        let today_start = now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .map(|naive| naive.and_utc())
            .unwrap_or(now);
        let online_cutoff = now - Duration::seconds(ONLINE_WINDOW_SECS);
        let members: Vec<StaffSettings> = state
            .staff
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|staff| staff.company_id == company_id)
            .collect();
        let mut statuses: Vec<TeamMemberStatus> = Vec::new();
        for staff in &members {
            let today_secs: i64 = state
                .entries
                .iter()
                .filter(|entry| {
                    entry.value().user_id == staff.user_id
                        && entry.value().started_at >= today_start
                })
                .map(|entry| entry.value().duration_secs)
                .sum();
            let today_samples: Vec<ActivitySample> = state
                .activity
                .iter()
                .filter(|entry| {
                    entry.value().user_id == staff.user_id
                        && entry.value().interval_start >= today_start
                })
                .map(|entry| entry.value().clone())
                .collect();
            let avg_activity_today = if today_samples.is_empty() {
                None
            } else {
                Some(
                    today_samples.iter().map(|sample| sample.activity_score).sum::<i64>() as f64
                        / today_samples.len() as f64,
                )
            };
            let last_activity_at = today_samples
                .iter()
                .map(|sample| sample.interval_end)
                .max();
            let active_timer = active_timer_for(&state, &staff.user_id);
            let online_now = active_timer.is_some()
                || last_activity_at.map(|at| at >= online_cutoff).unwrap_or(false);
            let low_activity = avg_activity_today
                .map(|avg| avg < global.low_activity_threshold as f64)
                .unwrap_or(false);
            statuses.push(TeamMemberStatus {
                user_id: staff.user_id.clone(),
                online_now,
                active_timer_id: active_timer.map(|timer| timer.id),
                today_secs,
                avg_activity_today,
                low_activity,
                last_activity_at,
            });
        }
        let online_count = statuses.iter().filter(|status| status.online_now).count();
        let low_count = statuses.iter().filter(|status| status.low_activity).count();
        Ok(json!({
            "company_id": company_id.to_string(),
            "members": statuses,
            "online_now": online_count,
            "low_activity_count": low_count,
            "total_members": members.len(),
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_settings(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TrackerSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        Ok(state.global_settings())
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_settings(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TrackerSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_SETTINGS)?;
        let input = body.into_inner();
        let mut settings = state.global_settings();
        if let Some(value) = input.idle_threshold_secs {
            if !(30..=86_400).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "idle_threshold_secs must be between 30 and 86400",
                ));
            }
            settings.idle_threshold_secs = value;
        }
        if let Some(value) = input.retention_days {
            if !(1..=3650).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "retention_days must be between 1 and 3650",
                ));
            }
            settings.retention_days = value;
        }
        if let Some(value) = input.overtime_hours_per_week {
            if !(0.0..=168.0).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "overtime_hours_per_week must be between 0 and 168",
                ));
            }
            settings.overtime_hours_per_week = value;
        }
        if let Some(value) = input.overtime_multiplier {
            if !(1.0..=4.0).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "overtime_multiplier must be between 1 and 4",
                ));
            }
            settings.overtime_multiplier = value;
        }
        if let Some(value) = input.low_activity_threshold {
            if !(0..=100).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "low_activity_threshold must be between 0 and 100",
                ));
            }
            settings.low_activity_threshold = value;
        }
        if let Some(raw) = &input.storage_target {
            settings.storage_target = require_text(raw, "storage_target", MAX_TARGET_LEN)?;
        }
        if let Some(raw) = &input.default_currency {
            settings.default_currency = valid_currency(raw)?;
        }
        if let Some(value) = input.auto_approve_timer_entries {
            settings.auto_approve_timer_entries = value;
        }
        settings.updated_at = Utc::now();
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        state.emit(
            events::SETTINGS_UPDATED,
            json!({
                "updated_by": caller.actor,
                "idle_threshold_secs": settings.idle_threshold_secs,
                "retention_days": settings.retention_days,
            }),
        );
        Ok(settings)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_my_settings(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let mine = state.user_settings_for(&caller.actor);
        let resolved = state.resolve_settings_for_user(&caller.actor);
        Ok(json!({
            "user_id": caller.actor,
            "screenshots_enabled": mine.screenshots_enabled,
            "blur_screenshots": mine.blur_screenshots,
            "idle_threshold_secs": mine.idle_threshold_secs,
            "resolved": {
                "company_id": resolved.company_id.map(|id| id.to_string()),
                "screenshots_enabled": resolved.screenshots_enabled,
                "screenshot_interval_secs": resolved.screenshot_interval_secs,
                "activity_level": format!("{:?}", resolved.activity_level).to_lowercase(),
                "idle_threshold_secs": resolved.idle_threshold_secs,
                "blur_screenshots": resolved.blur_screenshots,
                "timezone": resolved.timezone,
                "retention_days": resolved.retention_days,
            },
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_my_settings(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateMySettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<UserTrackerSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let input = body.into_inner();
        let mut mine = state.user_settings_for(&caller.actor);
        if let Some(value) = input.screenshots_enabled {
            mine.screenshots_enabled = value;
        }
        if let Some(value) = input.blur_screenshots {
            mine.blur_screenshots = value;
        }
        if let Some(value) = input.idle_threshold_secs {
            if !(30..=86_400).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "idle_threshold_secs must be between 30 and 86400",
                ));
            }
            mine.idle_threshold_secs = Some(value);
        }
        mine.updated_at = Utc::now();
        state
            .user_settings
            .insert(caller.actor.clone(), mine.clone());
        Ok(mine)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_buyer_settings(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let company_id = claimed_company_id(&req)?;
        require_company_member(&ctx, &caller.actor, company_id)?;
        let stored = state.buyer_settings_for(company_id);
        let resolved = state.resolve_settings_for_company(company_id);
        Ok(json!({
            "company_id": company_id.to_string(),
            "overrides": stored,
            "resolved": {
                "screenshots_enabled": resolved.screenshots_enabled,
                "screenshot_interval_secs": resolved.screenshot_interval_secs,
                "activity_level": format!("{:?}", resolved.activity_level).to_lowercase(),
                "idle_threshold_secs": resolved.idle_threshold_secs,
                "blur_screenshots": resolved.blur_screenshots,
                "allowed_apps": resolved.allowed_apps,
                "blocked_apps": resolved.blocked_apps,
                "allowed_websites": resolved.allowed_websites,
                "blocked_websites": resolved.blocked_websites,
                "timezone": resolved.timezone,
                "working_hours_start": resolved.working_hours_start,
                "working_hours_end": resolved.working_hours_end,
                "retention_days": resolved.retention_days,
            },
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_buyer_settings(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateBuyerSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BuyerSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_BUYER_SETTINGS)?;
        let company_id = claimed_company_id(&req)?;
        require_company_member(&ctx, &caller.actor, company_id)?;
        let input = body.into_inner();
        let mut settings = state.buyer_settings_for(company_id);
        if let Some(value) = input.screenshots_enabled {
            settings.screenshots_enabled = Some(value);
        }
        if let Some(value) = input.screenshot_interval_secs {
            if !(60..=3600).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "screenshot_interval_secs must be between 60 and 3600",
                ));
            }
            settings.screenshot_interval_secs = Some(value);
        }
        if let Some(value) = input.activity_level {
            settings.activity_level = Some(value);
        }
        if let Some(value) = input.idle_timeout_secs {
            if !(30..=86_400).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "idle_timeout_secs must be between 30 and 86400",
                ));
            }
            settings.idle_timeout_secs = Some(value);
        }
        if let Some(value) = input.blur_screenshots {
            settings.blur_screenshots = Some(value);
        }
        if let Some(values) = &input.allowed_apps {
            settings.allowed_apps = validate_list(values, "allowed_apps")?;
        }
        if let Some(values) = &input.blocked_apps {
            settings.blocked_apps = validate_list(values, "blocked_apps")?;
        }
        if let Some(values) = &input.allowed_websites {
            settings.allowed_websites = validate_list(values, "allowed_websites")?;
        }
        if let Some(values) = &input.blocked_websites {
            settings.blocked_websites = validate_list(values, "blocked_websites")?;
        }
        if let Some(value) = input.default_hourly_rate {
            if value < 0.0 || !value.is_finite() {
                return Err(UnifiedError::bad_request(
                    "default_hourly_rate must be a finite number >= 0",
                ));
            }
            settings.default_hourly_rate = Some(round4(value));
        }
        if let Some(raw) = &input.timezone {
            settings.timezone = Some(valid_timezone(raw)?);
        }
        if let Some(raw) = &input.working_hours_start {
            settings.working_hours_start = Some(valid_hhmm(raw, "working_hours_start")?);
        }
        if let Some(raw) = &input.working_hours_end {
            settings.working_hours_end = Some(valid_hhmm(raw, "working_hours_end")?);
        }
        if let Some(days) = &input.working_days {
            for day in days {
                if !(1..=7).contains(day) {
                    return Err(UnifiedError::bad_request(
                        "working_days entries must be ISO weekday numbers 1 (Monday) to 7 (Sunday)",
                    ));
                }
            }
            settings.working_days = days.clone();
        }
        if let Some(value) = input.retention_days {
            if !(1..=3650).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "retention_days must be between 1 and 3650",
                ));
            }
            settings.retention_days = Some(value);
        }
        settings.updated_at = Utc::now();
        state
            .buyer_settings
            .insert(company_id, settings.clone());
        state.emit(
            events::SETTINGS_UPDATED,
            json!({
                "company_id": company_id.to_string(),
                "updated_by": caller.actor,
                "scope": "buyer",
            }),
        );
        Ok(settings)
    })();
    json::respond(result, debug)
}
