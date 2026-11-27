//! WebSocket session handling.

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

// Meetings WebSocket endpoint

/// `GET /api/v1/meetings/ws?room={id}`: a room-scoped view over the
/// shared hub. The session subscribes with `global_hub().subscribe()`
/// and only forwards events for its room whose `recipient_id` is
/// absent (room broadcast) or matches the caller, never echoing the
/// caller's own messages back. Inbound text frames must be JSON
/// signalling messages (`{"type": "signal", "kind", "recipient_id"?,
/// "payload"}`) or `{"type": "ping"}`; they go through the same
/// `relay_signal` validation as the REST endpoint, so a client
/// cannot signal in a room it has not joined, or to a user who is
/// not in the room.
pub(crate) async fn meetings_ws(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    stream: web::Payload,
    query: web::Query<MeetingsWsQuery>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return err(error);
    }
    let actor = match actor_id(&req) {
        Ok(actor) => actor,
        Err(error) => return err(error),
    };
    let room_id = match &query.room {
        Some(raw) => match parse_uuid(raw, "room") {
            Ok(id) => id,
            Err(error) => return err(error),
        },
        None => return err(UnifiedError::bad_request("Query parameter room={id} is required")),
    };
    let room = match state.rooms.get(&room_id).map(|entry| entry.value().clone()) {
        Some(room) => room,
        None => {
            return err(UnifiedError::not_found(format!(
                "Meeting room not found: {room_id}"
            )))
        }
    };
    match state
        .participants
        .get(&participant_key(room_id, &actor))
        .map(|entry| entry.value().clone())
    {
        Some(participant) if participant.status == "joined" => {}
        _ => {
            return err(UnifiedError::forbidden(
                "Only a joined participant can open this room's WebSocket feed",
            ));
        }
    }
    let (response, mut session, mut msg_stream) = match actix_ws::handle(&req, stream) {
        Ok(parts) => parts,
        Err(error) => {
            return err(
                UnifiedError::bad_request(format!("WebSocket handshake failed: {error}"))
                    .with_trace(format!("{error:?}")),
            );
        }
    };
    let state = state.into_inner();
    let mut receiver = global_hub().subscribe();
    actix_web::rt::spawn(async move {
        loop {
            tokio::select! {
                incoming = msg_stream.next() => {
                    match incoming {
                        Some(Ok(Message::Ping(bytes))) => {
                            if session.pong(&bytes).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                        Some(Ok(Message::Text(text))) => {
                            // `from_slice` on the frame bytes: this
                            // parses the text frame regardless of
                            // whether the actix-ws version carries the
                            // payload as bytes or as a string.
                            let parsed: Result<Value, _> =
                                serde_json::from_slice(text.as_ref());
                            let reply = match parsed {
                                Ok(value) => handle_ws_message(&state, &room, &actor, value).await,
                                Err(_) => Some(json!({
                                    "error": { "message": "WebSocket message must be a JSON object", "code": 400, "metadata": {} },
                                })),
                            };
                            if let Some(reply) = reply {
                                let text = serde_json::to_string(&reply).unwrap_or_default();
                                if session.text(text).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Some(Ok(_)) => {}
                    }
                }
                broadcast = receiver.recv() => {
                    match broadcast {
                        Ok(event) => {
                            let Some(event_room) = event.get("room_id").and_then(|v| v.as_str()) else {
                                continue;
                            };
                            if event_room != room_id.to_string() {
                                continue;
                            }
                            if !event
                                .get("event")
                                .and_then(|v| v.as_str())
                                .is_some_and(|name| name.starts_with("meeting."))
                            {
                                continue;
                            }
                            if let Some(sender) = event.get("sender_id").and_then(|v| v.as_str()) {
                                if sender == actor {
                                    continue;
                                }
                            }
                            if let Some(recipient) = event.get("recipient_id").and_then(|v| v.as_str()) {
                                if recipient != actor {
                                    continue;
                                }
                            }
                            let text = serde_json::to_string(&event).unwrap_or_default();
                            if session.text(text).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
        let _ = session.close(None).await;
    });
    response
}

/// Handle one inbound JSON message on a meetings WebSocket session.
/// Returns a reply frame, or `None` when the message was relayed
/// silently (a valid signal) or was a heartbeat that needs no reply
/// beyond the pong below.
async fn handle_ws_message(
    state: &MeetingsState,
    room: &MeetingRoom,
    actor: &str,
    value: Value,
) -> Option<Value> {
    let message_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match message_type {
        "ping" => Some(json!({ "event": "meeting.pong", "room_id": room.id.to_string() })),
        "signal" => {
            let kind = value.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            let kind = match kind {
                "offer" => SignalKind::Offer,
                "answer" => SignalKind::Answer,
                "ice-candidate" | "ice_candidate" => SignalKind::IceCandidate,
                _ => {
                    return Some(json!({
                        "error": { "message": "Signal kind must be offer, answer, or ice-candidate", "code": 400, "metadata": {} },
                    }));
                }
            };
            let recipient_id = value
                .get("recipient_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let Some(payload) = value.get("payload").cloned() else {
                return Some(json!({
                    "error": { "message": "Signal message requires a payload object", "code": 400, "metadata": {} },
                }));
            };
            match relay_signal(state, room, actor, kind, recipient_id, payload) {
                Ok(_) => None,
                Err(error) => Some(json::error_json(&error, false)),
            }
        }
        _ => Some(json!({
            "error": { "message": "Unknown WebSocket message type: use \"signal\" or \"ping\"", "code": 400, "metadata": {} },
        })),
    }
}
