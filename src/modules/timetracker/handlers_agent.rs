// timetracker — HTTP handlers: desktop-agent endpoints.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Handlers: desktop-agent API (API-key authenticated)
//
// These endpoints are the server side the future desktop / mobile
// agent app will call. No agent binary is built in this round: any
// HTTP client holding a key scoped to `timetracker:write` (or
// `timetracker:read` for status) can exercise them today, which is
// exactly the contract the future agent will use.
// ---------------------------------------------------------------------------

pub(crate) async fn agent_heartbeat(
    state: web::Data<TimeTrackerState>,
    req: HttpRequest,
    body: web::Json<AgentHeartbeatRequest>,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<Value> = (|| {
        let (_key, user_id) =
            authenticate_agent(&state, &req, "write", AGENT_RATE_PER_MINUTE)?;
        let input = body.into_inner();
        if input.idle_secs < 0 {
            return Err(UnifiedError::bad_request("idle_secs must not be negative"));
        }
        let resolved = state.resolve_settings_for_user(&user_id);
        if resolved.activity_level == ActivityLevel::Off {
            return Err(UnifiedError::unprocessable(
                "Activity tracking is disabled for this company (activity_level off)",
            ));
        }
        // Idle past the resolved threshold auto-pauses the running
        // timer; the idle period is recorded on the timer, never
        // discarded. A heartbeat without idle seconds changes
        // nothing.
        let auto_paused = if input.idle_secs > 0 {
            auto_pause_on_idle(&state, &user_id, input.idle_secs)
        } else {
            false
        };
        let timer = active_timer_for(&state, &user_id);
        // Effective screenshot value: the staff / buyer switch AND
        // the user's own consent. Reporting the switch alone would
        // tell an agent to capture while every upload is rejected
        // with 403 for missing consent, so the consent flag is
        // reported alongside it, as `agent_status` does.
        let screenshot_consent = state.user_settings_for(&user_id).screenshots_enabled;
        Ok(json!({
            "user_id": user_id,
            "online": true,
            "idle_secs": input.idle_secs,
            "auto_paused": auto_paused,
            "active_timer": timer,
            "company_id": resolved.company_id.map(|id| id.to_string()),
            "screenshots_enabled": resolved.screenshots_enabled && screenshot_consent,
            "screenshot_consent": screenshot_consent,
            "screenshot_interval_secs": resolved.screenshot_interval_secs,
            "blur_screenshots": resolved.blur_screenshots,
            "activity_level": format!("{:?}", resolved.activity_level).to_lowercase(),
            "idle_threshold_secs": resolved.idle_threshold_secs,
        }))
    })();
    match result {
        Ok(view) => json::success_response(view, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn agent_activity(
    state: web::Data<TimeTrackerState>,
    req: HttpRequest,
    body: web::Json<ReportActivityRequest>,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<ActivitySample> = (|| {
        let (_key, user_id) =
            authenticate_agent(&state, &req, "write", AGENT_RATE_PER_MINUTE)?;
        record_activity(&state, &user_id, body.into_inner())
    })();
    match result {
        Ok(sample) => json::success_response(sample, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn agent_usage(
    state: web::Data<TimeTrackerState>,
    req: HttpRequest,
    body: web::Json<ReportUsageRequest>,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<AppUsageRecord> = (|| {
        let (_key, user_id) =
            authenticate_agent(&state, &req, "write", AGENT_RATE_PER_MINUTE)?;
        record_usage(&state, &user_id, body.into_inner())
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn agent_upload_screenshot(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<ScreenshotUploadQuery>,
    body: web::Bytes,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let (_key, user_id) =
            authenticate_agent(&state, &req, "write", SCREENSHOT_RATE_PER_MINUTE)?;
        let resolved = state.resolve_settings_for_user(&user_id);
        // Three gates, in order: the buyer's activity level
        // (screenshots need Full), the buyer's master switch, and
        // the user's own consent flag. Any closed gate is a 403
        // with the reason named, so the agent can surface it.
        if resolved.activity_level != ActivityLevel::Full {
            return Err(UnifiedError::forbidden(
                "Screenshots are disabled for this company: activity_level is not full",
            ));
        }
        if !resolved.screenshots_enabled {
            return Err(UnifiedError::forbidden(
                "Screenshots are disabled for this company by the buyer's settings",
            ));
        }
        if !state.user_settings_for(&user_id).screenshots_enabled {
            return Err(UnifiedError::forbidden(
                "Screenshots are disabled for this user: no screenshot consent (enable screenshots in /settings/me)",
            ));
        }
        let content_type = req
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let extension = match content_type.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            _ => {
                return Err(UnifiedError::bad_request(
                    "Screenshot content-type must be image/png, image/jpeg, or image/webp",
                ));
            }
        };
        if body.len() > MAX_SCREENSHOT_BYTES {
            return Err(UnifiedError::new(
                format!(
                    "Screenshot exceeds the maximum size of {MAX_SCREENSHOT_BYTES} bytes"
                ),
                413,
            ));
        }
        if body.is_empty() {
            return Err(UnifiedError::bad_request("Screenshot body must not be empty"));
        }
        let input = query.into_inner();
        if let Some(timer_id) = input.timer_id {
            let timer = state
                .timers
                .get(&timer_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found(format!("Timer not found: {timer_id}")))?;
            if timer.user_id != user_id {
                return Err(UnifiedError::forbidden(
                    "Screenshot cannot be attached to another user's timer",
                ));
            }
        }
        // Bytes go to the storage module only. The target must
        // exist; a missing target fails the upload (503) instead
        // of storing bytes somewhere else.
        let target = state.global_settings().storage_target;
        let provider = ctx.storage_registry.get(&target).ok_or_else(|| {
            UnifiedError::new(
                format!("Storage target \"{target}\" is not registered; configure it before uploading screenshots"),
                503,
            )
        })?;
        let id = Uuid::now_v7();
        let key = format!("timetracker/{user_id}/{id}.{extension}");
        let stored_key = provider.upload(&key, body.to_vec()).await?;
        let now = Utc::now();
        let record = ScreenshotRecord {
            id,
            user_id: user_id.clone(),
            timer_id: input.timer_id,
            project_id: input.project_id,
            task_id: input.task_id,
            storage_target: target.clone(),
            storage_key: stored_key.clone(),
            content_sha256: sha256_hex(&body),
            content_type: content_type.clone(),
            size_bytes: body.len() as u64,
            blurred: input.blurred || resolved.blur_screenshots,
            captured_at: input.captured_at.unwrap_or(now),
            created_at: now,
        };
        state.screenshots.insert(record.id, record.clone());
        state.emit(
            events::SCREENSHOT_UPLOADED,
            json!({
                "screenshot_id": record.id.to_string(),
                "user_id": user_id,
                "timer_id": record.timer_id.map(|id| id.to_string()),
                "project_id": record.project_id.map(|id| id.to_string()),
                "task_id": record.task_id.map(|id| id.to_string()),
                "storage_target": target,
                "storage_key": stored_key,
                "content_type": content_type,
                "size_bytes": record.size_bytes,
                "blurred": record.blurred,
                "captured_at": record.captured_at.to_rfc3339(),
            }),
        );
        Ok(json!({ "screenshot": record }))
    })()
    .await;
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn agent_status(
    state: web::Data<TimeTrackerState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<Value> = (|| {
        let (_key, user_id) =
            authenticate_agent(&state, &req, "read", AGENT_RATE_PER_MINUTE)?;
        let resolved = state.resolve_settings_for_user(&user_id);
        let mine = state.user_settings_for(&user_id);
        let timer = active_timer_for(&state, &user_id);
        Ok(json!({
            "user_id": user_id,
            "company_id": resolved.company_id.map(|id| id.to_string()),
            "screenshots_enabled": resolved.screenshots_enabled,
            "screenshot_consent": mine.screenshots_enabled,
            "screenshot_interval_secs": resolved.screenshot_interval_secs,
            "blur_screenshots": resolved.blur_screenshots,
            "activity_level": format!("{:?}", resolved.activity_level).to_lowercase(),
            "idle_threshold_secs": resolved.idle_threshold_secs,
            "timezone": resolved.timezone,
            "retention_days": resolved.retention_days,
            "active_timer": timer,
        }))
    })();
    match result {
        Ok(view) => json::success_response(view, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn agent_start_timer(
    state: web::Data<TimeTrackerState>,
    req: HttpRequest,
    body: web::Json<StartTimerRequest>,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<TimeTrackerTimer> = (|| {
        let (_key, user_id) =
            authenticate_agent(&state, &req, "write", AGENT_RATE_PER_MINUTE)?;
        let input = body.into_inner();
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let now = Utc::now();
        let timer = TimeTrackerTimer {
            id: Uuid::now_v7(),
            user_id: user_id.clone(),
            project_id: input.project_id,
            task_id: input.task_id,
            description,
            billable: input.billable,
            status: TimerStatus::Running,
            started_at: now,
            paused_total_secs: 0,
            paused_at: None,
            idle_total_secs: 0,
            stopped_at: None,
            entry_id: None,
            created_at: now,
            updated_at: now,
        };
        // Atomic check-and-insert: two concurrent starts for this
        // user cannot both pass (see `insert_timer_if_none_active`).
        if !state.insert_timer_if_none_active(timer.clone()) {
            return Err(UnifiedError::conflict(
                "An active timer already exists for this user: stop or pause it before starting another",
            ));
        }
        state.emit(
            events::TIMER_STARTED,
            json!({
                "timer_id": timer.id.to_string(),
                "user_id": user_id,
                "project_id": timer.project_id.map(|id| id.to_string()),
                "task_id": timer.task_id.map(|id| id.to_string()),
                "billable": timer.billable,
                "started_at": timer.started_at.to_rfc3339(),
                "source": "agent",
            }),
        );
        Ok(timer)
    })();
    match result {
        Ok(timer) => json::success_response(timer, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn agent_stop_timer(
    state: web::Data<TimeTrackerState>,
    req: HttpRequest,
    body: web::Json<Value>,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<TimeEntry> = (|| {
        let (_key, user_id) =
            authenticate_agent(&state, &req, "write", AGENT_RATE_PER_MINUTE)?;
        let timer_id = body
            .get("timer_id")
            .and_then(|value| value.as_str())
            .map(|raw| parse_uuid(raw, "timer_id"))
            .transpose()?;
        let timer = match timer_id {
            Some(id) => state
                .timers
                .get(&id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found(format!("Timer not found: {id}")))?,
            None => active_timer_for(&state, &user_id).ok_or_else(|| {
                UnifiedError::not_found("No active timer for this user")
            })?,
        };
        if timer.user_id != user_id {
            return Err(UnifiedError::forbidden(
                "An agent can only stop its own user's timer",
            ));
        }
        stop_timer_inner(&state, timer.id, "agent_stop")
    })();
    json::respond(result, debug)
}
