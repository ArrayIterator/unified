// webinars -- Chat handlers.

use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use super::handlers_questions::*;
use super::helpers::*;
use super::state::*;
use super::types::*;

// ---------------------------------------------------------------------------
// Chat handlers
// ---------------------------------------------------------------------------

pub(crate) async fn get_chat_channel(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let webinar = load_webinar(&state, &path.into_inner())?;
        require_qa_access(&state, &webinar, &actor, &roles)?;
        let channel_id = webinar
            .chat_channel_id
            .clone()
            .unwrap_or_else(|| format!("webinar:{}", webinar.id));
        Ok(json!({
            "webinar_id": webinar.id.to_string(),
            "channel_id": channel_id,
            "chat_enabled": webinar.chat_enabled,
            "status": status_name(webinar.status),
            "module": "chat",
            "note": "Messages live in the chat module; this webinar only names the channel and emits webinar.chat.message events for it.",
        }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error),
    }
}

pub(crate) async fn post_chat_message(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PostChatRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        require_qa_access(&state, &webinar, &actor, &roles)?;
        if !webinar.chat_enabled {
            return Err(UnifiedError::unprocessable(
                "Chat is disabled for this webinar",
            ));
        }
        if webinar.status != WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "Chat messages can only be posted while the webinar is live",
            ));
        }
        let body_text = require_text(&input.body, "body", MAX_CHAT_BODY_LEN)?;
        let channel_id = webinar
            .chat_channel_id
            .clone()
            .unwrap_or_else(|| format!("webinar:{}", webinar.id));
        let now = Utc::now();
        let message_id = Uuid::now_v7();
        // The chat module owns storage and delivery: this module
        // emits the event and reports honestly whether any handler
        // is listening, instead of pretending the message landed.
        let delivered = state.bus_has("webinar.chat.message");
        state.emit(
            "webinar.chat.message",
            json!({
                "webinar_id": webinar.id.to_string(),
                "message_id": message_id.to_string(),
                "channel_id": channel_id,
                "author_id": actor,
                "body": body_text,
                "sent_at": now.to_rfc3339(),
            }),
        );
        Ok(json!({
            "message_id": message_id.to_string(),
            "webinar_id": webinar.id.to_string(),
            "channel_id": channel_id,
            "author_id": actor,
            "body": body_text,
            "sent_at": now.to_rfc3339(),
            "delivered": delivered,
            "delivered_via": "webinar.chat.message",
        }))
    })();
    match result {
        Ok(value) => created(value),
        Err(error) => err(error),
    }
}
