// webinars -- Follow-up handler.

use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};

use super::helpers::*;
use super::state::*;
use super::types::*;

// Follow-up handler

pub(crate) async fn send_follow_up(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FollowUpRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let mut webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can send the follow-up",
            ));
        }
        if webinar.status != WebinarStatus::Ended {
            return Err(UnifiedError::unprocessable(
                "Follow-up can only be sent after the webinar has ended",
            ));
        }
        let message = match &input.message {
            Some(raw) => Some(require_text(raw, "message", MAX_FOLLOW_UP_MESSAGE_LEN)?),
            None => None,
        };
        let distribution_id = input
            .survey_distribution_id
            .as_ref()
            .map(|raw| raw.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| webinar.survey_distribution_id.clone());
        let include_no_shows = input.include_no_shows.unwrap_or(false);
        let replay_link = format!("/api/v1/webinars/{}/replay", webinar.id);
        let ready_recording_url = state
            .recordings
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.webinar_id == webinar.id && row.status == "ready")
            .min_by(|a, b| a.created_at.cmp(&b.created_at))
            .and_then(|row| row.storage_url.clone());
        let mut recipients: Vec<String> = Vec::new();
        for entry in state.attendance.iter() {
            let row = entry.value();
            if row.webinar_id == webinar.id && !recipients.contains(&row.user_id) {
                recipients.push(row.user_id.clone());
            }
        }
        if include_no_shows {
            for entry in state.registrations.iter() {
                let row = entry.value();
                if row.webinar_id == webinar.id
                    && row.status == "registered"
                    && !recipients.contains(&row.user_id)
                {
                    recipients.push(row.user_id.clone());
                }
            }
        }
        let body_text = message.unwrap_or_else(|| {
            "Thank you for joining. The recording and a short feedback survey are ready for you.".to_string()
        });
        let mut notified = 0u64;
        for user_id in &recipients {
            state.notify(
                user_id,
                &format!("Follow-up: {}", webinar.title),
                &body_text,
                Some(replay_link.clone()),
            );
            notified += 1;
        }
        // Ask the surveys module to send the follow-up survey to
        // its distribution. The surveys module owns the
        // distribution, the recipients, and the delivery; this
        // module only requests it.
        let survey_requested = match &distribution_id {
            Some(id) => {
                state.emit(
                    "surveys.send_requested",
                    json!({
                        "distribution_id": id,
                        "survey_id": webinar.survey_id,
                        "webinar_id": webinar.id.to_string(),
                    }),
                );
                true
            }
            None => false,
        };
        let now = Utc::now();
        webinar.follow_up_sent_at = Some(now);
        webinar.follow_up_count = notified;
        webinar.updated_at = now;
        state.webinars.insert(webinar.id, webinar.clone());
        state.emit(
            "webinar.follow_up.sent",
            json!({
                "webinar_id": webinar.id.to_string(),
                "sent_by": actor,
                "notified": notified,
                "include_no_shows": include_no_shows,
                "survey_distribution_id": distribution_id,
                "survey_requested": survey_requested,
                "recording_url": ready_recording_url,
                "replay_link": replay_link,
                "sent_at": now.to_rfc3339(),
            }),
        );
        Ok(json!({
            "webinar_id": webinar.id.to_string(),
            "notified": notified,
            "survey_requested": survey_requested,
            "survey_distribution_id": distribution_id,
            "recording_url": ready_recording_url,
            "replay_link": replay_link,
            "sent_at": now.to_rfc3339(),
        }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error),
    }
}
