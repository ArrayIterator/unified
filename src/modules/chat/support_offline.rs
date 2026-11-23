//! chat - support offline.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use serde_json::{json, Value};
use uuid::Uuid;
use super::{limits::*, types::*, hub::*, state::*, helpers::*, module::*, handlers_channels::*, handlers_messages::*, handlers_files::*, handlers_direct::*, handlers_presence::*, handlers_websocket::*, support_config::*, support_sessions::*, support_ratings::*, support_agents::*, support_canned::*};



// ---------------------------------------------------------------------------
// Live support: offline messages
// ---------------------------------------------------------------------------
pub(crate) async fn list_offline_messages(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<OfflineMessage>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let _actor = actor_id(&req)?;
        let params = query.into_inner();
        let department_filter = params.filters.get("department_id").cloned();
        let ticket_filter = params.filters.get("ticket_requested").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<OfflineMessage> = state
            .offline_messages
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|message| {
                if let Some(department) = &department_filter {
                    if message.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if let Some(ticket_requested) = &ticket_filter {
                    let expected = ticket_requested == "true";
                    if message.ticket_requested != expected {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !message.subject.to_lowercase().contains(&search)
                    && !message.message.to_lowercase().contains(&search)
                    && !message.visitor_email.to_lowercase().contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        // Newest first unless the caller asks for ascending order.
        let descending = params.normalized().order != "asc";
        items.sort_by(|a, b| {
            if descending {
                b.created_at.cmp(&a.created_at)
            } else {
                a.created_at.cmp(&b.created_at)
            }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}


/// A visitor leaves a message when no agent is available. When agents
/// are free the widget should start a live chat instead, so this
/// endpoint refuses with a conflict in that case. Otherwise the message
/// becomes a ticket through `ticket.create_requested` and is kept here
/// for staff follow-up.
pub(crate) async fn create_offline_message(
    state: web::Data<ChatState>,
    body: web::Json<CreateOfflineMessageRequest>,
) -> HttpResponse {
    let result: UnifiedResult<OfflineMessage> = (|| {
        let body = body.into_inner();
        let visitor_name = require_text(&body.visitor_name, "visitor_name", MAX_VISITOR_NAME_LEN)?;
        let visitor_email = require_email(&body.visitor_email)?;
        let department_id =
            clean_optional_text(&body.department_id, "department_id", MAX_DEPARTMENT_LEN)?;
        let subject = match &body.subject {
            Some(raw) => require_text(raw, "subject", MAX_SUBJECT_LEN)?,
            None => format!("Offline message from {visitor_name}"),
        };
        let message_text = require_text(&body.message, "message", MAX_OFFLINE_MESSAGE_LEN)?;
        if agents_available(&state, department_id.as_deref()) {
            return Err(UnifiedError::conflict(
                "Live agents are available for this department; start a live chat instead",
            ));
        }
        let ticket_id = request_ticket(
            &state,
            json!({
                "source": "chat",
                "channel_id": Value::Null,
                "user_id": visitor_email.clone(),
                "visitor_name": visitor_name.clone(),
                "visitor_email": visitor_email.clone(),
                "title": subject.clone(),
                "body": message_text.clone(),
                "description": message_text.clone(),
                "company_id": Value::Null,
                "department_id": department_id.clone(),
                "priority": "normal",
                "category": "chat",
            }),
        )?;
        let offline = OfflineMessage {
            id: Uuid::now_v7(),
            visitor_name: visitor_name.clone(),
            visitor_email: visitor_email.clone(),
            department_id: department_id.clone(),
            subject: subject.clone(),
            message: message_text,
            ticket_requested: ticket_id.is_some(),
            ticket_id: ticket_id.clone(),
            created_at: Utc::now(),
        };
        state.offline_messages.insert(offline.id, offline.clone());
        state.emit(
            "chat.offline_message.created",
            json!({
                "offline_message_id": offline.id.to_string(),
                "visitor_email": visitor_email,
                "department_id": department_id,
                "ticket_id": ticket_id,
            }),
        );
        Ok(offline)
    })();
    match result {
        Ok(offline) => created(offline),
        Err(error) => err(error),
    }
}
