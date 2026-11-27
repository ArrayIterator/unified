//! Hook-bus handlers: answering requests from other modules.

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

/// Answer `meeting.start_requested`: the event the scheduler emits
/// when a `meeting.start` job fires. A scheduled room whose start
/// time has arrived goes live; any other state is left alone, and an
/// unknown room id is ignored so a stale job can never resurrect a
/// deleted room.
pub(crate) fn on_meeting_start_requested(state: MeetingsState, ctx: &mut HookContext) {
    let room_id = ctx
        .data
        .get("room_id")
        .or_else(|| ctx.data.get("meeting_id"))
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(room_id) = room_id else {
        ctx.add_result(json!({ "started": false, "error": "meeting.start_requested requires 'room_id'" }));
        return;
    };
    let Some(mut entry) = state.rooms.get_mut(&room_id) else {
        ctx.add_result(json!({ "started": false, "room_id": room_id.to_string(), "error": "Room not found" }));
        return;
    };
    if entry.status != RoomStatus::Scheduled {
        ctx.add_result(json!({
            "started": false,
            "room_id": room_id.to_string(),
            "reason": "Room is not scheduled",
        }));
        return;
    }
    entry.status = RoomStatus::Live;
    entry.started_at = Some(Utc::now());
    entry.updated_at = Utc::now();
    let room = entry.clone();
    drop(entry);
    state.publish_room_event(
        "meeting.started",
        room_id,
        json!({ "host_id": room.host_id, "started_at": room.started_at }),
    );
    ctx.add_result(json!({ "started": true, "room_id": room_id.to_string() }));
}

// Signalling relay (WebRTC over the shared WebSocket hub)

/// Validate and relay one signalling message.
///
/// The shared hub (`crate::http::websocket::global_hub`) is a single
/// global broadcast channel: `broadcast` delivers to every connected
/// `GET /ws` client, and `subscribe` is how a session receives. There
/// is no per-room channel in the hub, so routing is done with payload
/// fields - every relayed event carries `room_id`, `sender_id`, and,
/// for directed messages, `recipient_id` - and receivers filter on
/// them (the meetings WebSocket endpoint does this server-side).
/// Validation is enforced here, before anything is broadcast, so a
/// participant can only signal inside rooms they have joined, and
/// only to recipients who are in the same room.
pub(crate) fn relay_signal(
    state: &MeetingsState,
    room: &MeetingRoom,
    sender_id: &str,
    kind: SignalKind,
    recipient_id: Option<String>,
    payload: Value,
) -> UnifiedResult<Value> {
    let sender = load_participant(state, room.id, sender_id)?;
    if sender.status != "joined" {
        return Err(UnifiedError::forbidden(
            "Only a joined participant can send signalling messages in this room",
        ));
    }
    if !payload.is_object() {
        return Err(UnifiedError::bad_request(
            "Signal payload must be a JSON object (SDP or ICE candidate)",
        ));
    }
    if let Some(recipient) = &recipient_id {
        if recipient == sender_id {
            return Err(UnifiedError::bad_request(
                "A signalling message cannot be addressed to its own sender",
            ));
        }
        let target = load_participant(state, room.id, recipient)?;
        if target.status != "joined" {
            return Err(UnifiedError::not_found(format!(
                "Recipient {recipient} is not currently in this room"
            )));
        }
    }
    let kind_name = match kind {
        SignalKind::Offer => "offer",
        SignalKind::Answer => "answer",
        SignalKind::IceCandidate => "ice-candidate",
    };
    let event = json!({
        "event": "meeting.signal",
        "room_id": room.id.to_string(),
        "sender_id": sender_id,
        "recipient_id": recipient_id,
        "kind": kind_name,
        "payload": payload,
        "sent_at": Utc::now().to_rfc3339(),
    });
    global_hub().broadcast(event.clone());
    state.emit("meeting.signal", event);
    Ok(json!({
        "delivered": true,
        "room_id": room.id.to_string(),
        "sender_id": sender_id,
        "recipient_id": recipient_id,
        "kind": kind_name,
    }))
}
