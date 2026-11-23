// Handoff handlers: conversation to support ticket.

use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Handoff handlers (conversation -> support ticket, via hook)
// ---------------------------------------------------------------------------

/// Shared handoff implementation: emit `ticket.create_requested` and mark
/// the conversation as handed off. The tickets module owns ticket creation;
/// when its hook handler contributes a ticket id in the results, it is
/// returned as `ticket_id`, otherwise `ticket_id` is null and the caller
/// knows only that the request was emitted.
pub(crate) fn do_handoff(
    state: &AiSupportState,
    actor: &str,
    conversation_id: Uuid,
    summary: String,
) -> UnifiedResult<Value> {
    let mut conversation = load_conversation(state, conversation_id, actor)?;
    // The lifecycle table is enforced before any ticket is
    // requested: only `open` -> `handoff` (and the same-status
    // re-request) may hand off. A closed conversation must be
    // explicitly reopened first.
    if !ConversationStatus::can_transition(conversation.status, ConversationStatus::Handoff) {
        return Err(UnifiedError::conflict(format!(
            "Cannot hand off a {} conversation; reopen it first",
            conversation.status.as_str()
        )));
    }
    let history: Vec<ConversationMessage> = state
        .messages
        .get(&conversation_id)
        .map(|entry| entry.value().clone())
        .unwrap_or_default();
    let mut transcript = String::new();
    for message in history.iter().rev().take(3).rev() {
        let role = match message.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
        };
        transcript.push_str(&format!("{role}: {}\n", message.content));
    }
    let title = conversation
        .title
        .clone()
        .unwrap_or_else(|| "AI Support handoff".to_string());
    let mut ticket_id: Option<String> = None;
    match state.emit(
        "ticket.create_requested",
        json!({
            "source": "ai-support",
            "conversation_id": conversation_id.to_string(),
            "user_id": actor,
            "title": title,
            "summary": summary,
            "transcript": transcript.trim(),
        }),
    ) {
        Some(ctx) => {
            for result in &ctx.results {
                if let Some(id) = result
                    .get("ticket_id")
                    .or_else(|| result.get("id"))
                    .and_then(|value| value.as_str())
                {
                    ticket_id = Some(id.to_string());
                    break;
                }
            }
        }
        None => {
            return Err(UnifiedError::internal(
                "Ticket service is unavailable: no handler answered ticket.create_requested",
            ));
        }
    }
    conversation.status = ConversationStatus::Handoff;
    conversation.updated_at = Utc::now();
    state
        .conversations
        .insert(conversation.id, conversation.clone());
    Ok(json!({
        "ticket_requested": true,
        "ticket_id": ticket_id,
        "conversation_id": conversation_id.to_string(),
        "status": "handoff",
    }))
}

pub(crate) async fn handoff(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    body: web::Json<HandoffRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let history: Vec<ConversationMessage> = state
            .messages
            .get(&body.conversation_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        let summary = history
            .iter()
            .rev()
            .take(3)
            .map(|message| message.content.clone())
            .collect::<Vec<String>>();
        let mut summary = summary;
        summary.reverse();
        let summary = summary.join(" ");
        let summary = if summary.trim().is_empty() {
            "AI Support handoff requested by the user.".to_string()
        } else {
            summary.chars().take(MAX_SUMMARY_LEN).collect()
        };
        do_handoff(&state, &actor, body.conversation_id, summary)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}

pub(crate) async fn create_ticket(
    state: web::Data<AiSupportState>,
    req: HttpRequest,
    body: web::Json<CreateTicketRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let summary = require_text(&body.summary, "summary", MAX_SUMMARY_LEN)?;
        do_handoff(&state, &actor, body.conversation_id, summary)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}
