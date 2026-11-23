// webinars -- Configuration handlers.

use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::json;

use super::helpers::*;
use super::state::*;
use super::types::*;

// ---------------------------------------------------------------------------
// Config handlers
// ---------------------------------------------------------------------------

pub(crate) async fn get_config(state: web::Data<WebinarsState>, req: HttpRequest) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return err(error);
    }
    match actor_id(&req) {
        Ok(_) => ok(state.config()),
        Err(error) => err(error),
    }
}

pub(crate) async fn update_config(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    body: web::Json<UpdateConfigRequest>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarConfig> = (|| {
        let _tenant = require_tenant(&req)?;
        actor_id(&req)?;
        let roles = caller_roles(&req);
        if !is_admin(&roles) {
            return Err(UnifiedError::forbidden(
                "Only an admin can change the webinar defaults",
            ));
        }
        let input = body.into_inner();
        let mut config = state.config();
        if let Some(value) = input.default_capacity {
            config.default_capacity = value;
        }
        if let Some(value) = input.min_capacity {
            config.min_capacity = value;
        }
        if let Some(value) = input.max_capacity {
            config.max_capacity = value;
        }
        if config.min_capacity < 1 {
            return Err(UnifiedError::bad_request("min_capacity must be at least 1"));
        }
        if config.max_capacity < config.min_capacity {
            return Err(UnifiedError::bad_request(
                "max_capacity must not be below min_capacity",
            ));
        }
        if config.default_capacity < config.min_capacity
            || config.default_capacity > config.max_capacity
        {
            return Err(UnifiedError::bad_request(
                "default_capacity must be between min_capacity and max_capacity",
            ));
        }
        if let Some(value) = input.default_requires_approval {
            config.default_requires_approval = value;
        }
        if let Some(value) = input.default_allow_waitlist {
            config.default_allow_waitlist = value;
        }
        if let Some(value) = input.auto_promote_waitlist {
            config.auto_promote_waitlist = value;
        }
        if let Some(raw) = &input.default_timezone {
            let timezone = require_text(raw, "default_timezone", MAX_TIMEZONE_LEN)?;
            if !valid_timezone(&timezone) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid default_timezone: {timezone}. Use an IANA name such as \"Europe/London\" or \"UTC\""
                )));
            }
            config.default_timezone = timezone;
        }
        if let Some(value) = input.reminder_lead_mins {
            config.reminder_lead_mins = value;
        }
        if let Some(value) = input.drop_off_bucket_mins {
            if value == 0 {
                return Err(UnifiedError::bad_request(
                    "drop_off_bucket_mins must be at least 1",
                ));
            }
            config.drop_off_bucket_mins = value;
        }
        if let Some(value) = input.poll_max_options {
            if value < 2 {
                return Err(UnifiedError::bad_request(
                    "poll_max_options must be at least 2",
                ));
            }
            config.poll_max_options = value;
        }
        if let Some(value) = input.chat_enabled_default {
            config.chat_enabled_default = value;
        }
        if let Some(value) = input.qa_enabled_default {
            config.qa_enabled_default = value;
        }
        if let Some(value) = input.polls_enabled_default {
            config.polls_enabled_default = value;
        }
        if let Some(value) = input.recording_enabled_default {
            config.recording_enabled_default = value;
        }
        *state.config.write().expect("config lock poisoned") = config.clone();
        state.emit(
            "webinar.config.updated",
            json!({
                "default_capacity": config.default_capacity,
                "min_capacity": config.min_capacity,
                "max_capacity": config.max_capacity,
                "default_requires_approval": config.default_requires_approval,
                "default_allow_waitlist": config.default_allow_waitlist,
                "auto_promote_waitlist": config.auto_promote_waitlist,
                "default_timezone": config.default_timezone,
                "reminder_lead_mins": config.reminder_lead_mins,
                "drop_off_bucket_mins": config.drop_off_bucket_mins,
                "poll_max_options": config.poll_max_options,
            }),
        );
        Ok(config)
    })();
    match result {
        Ok(config) => ok(config),
        Err(error) => err(error),
    }
}
