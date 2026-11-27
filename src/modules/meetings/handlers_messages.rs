//! HTTP handlers: in-meeting messages and signalling.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::storage::StorageProvider;
use crate::http::websocket::global_hub;
use actix_web::{web, HttpRequest, HttpResponse};
use actix_ws::Message;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;
use uuid::Uuid;

use super::*;

// In-meeting chat and signalling

pub(crate) async fn list_messages(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<MeetingMessage>> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let room = load_room(&state, &path.into_inner())?;
        let participant = load_participant(&state, room.id, &actor)?;
        if participant.status != "joined" && !is_host(&room, &actor) {
            return Err(UnifiedError::forbidden(
                "Only a joined participant can read this room's chat",
            ));
        }
        let params = query.into_inner();
        let author_filter = params.filters.get("author_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<MeetingMessage> = state
            .messages
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|message| message.room_id == room.id)
            .filter(|message| {
                if let Some(author) = &author_filter {
                    if &message.author_id != author {
                        return false;
                    }
                }
                if !search.is_empty() && !message.body.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id)));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

pub(crate) async fn post_message(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PostMeetingMessageRequest>,
) -> HttpResponse {
    let result: UnifiedResult<MeetingMessage> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let room = load_room(&state, &path.into_inner())?;
        let participant = load_participant(&state, room.id, &actor)?;
        if participant.status != "joined" {
            return Err(UnifiedError::forbidden(
                "Only a joined participant can post in this room's chat",
            ));
        }
        let text = require_text(&body.body, "body", MAX_CHAT_BODY_LEN)?;
        let now = Utc::now();
        let message = MeetingMessage {
            id: Uuid::now_v7(),
            room_id: room.id,
            author_id: actor.clone(),
            body: text.clone(),
            created_at: now,
        };
        state.messages.insert(message.id, message.clone());
        state.publish_room_event(
            "meeting.chat.message",
            room.id,
            json!({
                "message_id": message.id.to_string(),
                "author_id": actor,
                "body": text,
                "created_at": now.to_rfc3339(),
            }),
        );
        Ok(message)
    })();
    match result {
        Ok(message) => created(message),
        Err(error) => err(error),
    }
}

pub(crate) async fn post_signal(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SignalRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let room = load_room(&state, &path.into_inner())?;
        let input = body.into_inner();
        relay_signal(
            &state,
            &room,
            &actor,
            input.kind,
            input.recipient_id,
            input.payload,
        )
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}
