//! WebRTC signalling: REST store-and-forward plus a
//! WebSocket feed over the shared hub.
//!
//! The design reuses the meetings module's approach exactly:
//! signalling messages (offer / answer / ICE candidate) are
//! validated, stored, and broadcast on the process-wide
//! `global_hub()` (`crate::http::websocket`), and receivers
//! filter on `call_id` plus `recipient_id`. Media itself is
//! peer-to-peer WebRTC between the callers' browsers - the
//! server NEVER relays media in this version, and there is no
//! SFU: a group call depends on every participant's browser
//! keeping a peer connection to every other participant.
//! Follow-up (not started here): add an SFU / media server
//! and a server-side recording pipeline.

use actix_web::{web, HttpRequest, HttpResponse};
use actix_ws::Message;
use chrono::Utc;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, err, load_participant, load_visible_call, ok, paginate, parse_uuid, resolve_tenant,
};
use super::state::PhoneState;
use super::types::{SignalKind, SignalMessage, SignalRequest, EVENT_SIGNAL_RELAYED};
use crate::http::websocket::global_hub;

/// Path carrying a call id.
#[derive(Debug, Deserialize)]
struct CallPath {
    call_id: String,
}

/// Query for the call WebSocket feed.
#[derive(Debug, Deserialize)]
struct PhoneWsQuery {
    call: Option<String>,
}

// Signalling relay (shared by the REST endpoint and the
// WebSocket endpoint, exactly like meetings `relay_signal`)

/// Validate and relay one signalling message inside a call:
/// the sender must be a participant of the call, the call must
/// still be live (ringing, active, or held), the payload must
/// be a JSON object (SDP or ICE candidate), and a directed
/// message must name another participant of the same call.
/// The message is stored (store-and-forward for clients that
/// reconnect), broadcast on the shared hub, and the
/// `phone.signal` hook event is emitted.
pub(crate) fn relay_signal(
    state: &PhoneState,
    tenant_id: &str,
    call_id: Uuid,
    sender_id: &str,
    kind: SignalKind,
    recipient_id: Option<String>,
    payload: Value,
) -> UnifiedResult<Value> {
    let call = state
        .calls
        .get(&call_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Call not found: {call_id}")))?;
    if call.tenant_id != tenant_id {
        return Err(UnifiedError::not_found(format!(
            "Call not found: {call_id}"
        )));
    }
    if call.status.is_terminal() {
        return Err(UnifiedError::conflict(format!(
            "Cannot signal in a {} call",
            call.status.as_str()
        )));
    }
    let sender = load_participant(state, call_id, sender_id)?;
    if sender.status == super::types::ParticipantStatus::Declined
        || sender.status == super::types::ParticipantStatus::Left
        || sender.status == super::types::ParticipantStatus::Missed
    {
        return Err(UnifiedError::forbidden(
            "Only a current participant can send signalling messages in this call",
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
        let target = load_participant(state, call_id, recipient)?;
        if target.status == super::types::ParticipantStatus::Declined
            || target.status == super::types::ParticipantStatus::Left
            || target.status == super::types::ParticipantStatus::Missed
        {
            return Err(UnifiedError::not_found(format!(
                "Recipient {recipient} is not currently in this call"
            )));
        }
    }
    let message = SignalMessage {
        id: Uuid::now_v7(),
        tenant_id: tenant_id.to_string(),
        call_id,
        sender_id: sender_id.to_string(),
        recipient_id: recipient_id.clone(),
        kind,
        payload: payload.clone(),
        delivered: false,
        created_at: Utc::now(),
    };
    state.signals.insert(message.id, message.clone());
    let event = json!({
        "event": "phone.signal",
        "call_id": call_id.to_string(),
        "sender_id": sender_id,
        "recipient_id": recipient_id,
        "kind": kind.as_str(),
        "payload": payload,
        "sent_at": message.created_at.to_rfc3339(),
    });
    global_hub().broadcast(event.clone());
    state.emit(EVENT_SIGNAL_RELAYED, event);
    Ok(json!({
        "signal_id": message.id.to_string(),
        "delivered": true,
        "call_id": call_id.to_string(),
        "sender_id": sender_id,
        "recipient_id": recipient_id,
        "kind": kind.as_str(),
    }))
}

/// Handle one inbound WebSocket JSON message: `{"type":
/// "signal", "kind", "recipient_id"?, "payload"}` or
/// `{"type": "ping"}`. Returns a reply frame, if any.
async fn handle_ws_message(
    state: &PhoneState,
    tenant_id: &str,
    call_id: Uuid,
    actor: &str,
    value: Value,
) -> Option<Value> {
    let message_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match message_type {
        "ping" => Some(json!({ "type": "pong" })),
        "signal" => {
            let kind = value
                .get("kind")
                .and_then(|v| v.as_str())
                .and_then(SignalKind::from_str);
            let Some(kind) = kind else {
                return Some(json!({
                    "error": { "message": "Signal kind must be offer, answer, or ice_candidate", "code": 400, "metadata": {} },
                }));
            };
            let recipient_id = value
                .get("recipient_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let payload = match value.get("payload").cloned() {
                Some(payload) => payload,
                None => {
                    return Some(json!({
                        "error": { "message": "Signal payload is required", "code": 400, "metadata": {} },
                    }));
                }
            };
            match relay_signal(
                state,
                tenant_id,
                call_id,
                actor,
                kind,
                recipient_id,
                payload,
            ) {
                Ok(data) => Some(json!({ "type": "signal", "result": data })),
                Err(error) => Some(json!({
                    "error": { "message": error.to_string(), "code": 400, "metadata": {} },
                })),
            }
        }
        _ => Some(json!({
            "error": { "message": "Unknown WebSocket message type (expected signal or ping)", "code": 400, "metadata": {} },
        })),
    }
}

// REST handlers (authenticated)

pub(crate) async fn post_signal(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CallPath>,
    body: web::Json<SignalRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let call = load_visible_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        let input = body.into_inner();
        let kind = SignalKind::from_str(&input.kind).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown signal kind: {} (expected offer, answer, or ice_candidate)",
                input.kind
            ))
        })?;
        let recipient = match &input.recipient_id {
            Some(raw) if !raw.trim().is_empty() => Some(raw.trim().to_string()),
            _ => None,
        };
        relay_signal(
            &state,
            &tenant_id,
            call.id,
            &caller.actor,
            kind,
            recipient,
            input.payload,
        )
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// The signals inbox: stored signalling messages of one call,
/// paginated. A caller only sees messages addressed to them or
/// broadcast to the whole call. Reading the inbox marks the
/// messages it returned as delivered, so a reconnecting client
/// does not receive the same offer twice.
pub(crate) async fn list_signals(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CallPath>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let params = query.into_inner();
        let call = load_visible_call(&state, &ctx, &tenant_id, &caller, &path.call_id)?;
        let kind_filter = params
            .filters
            .get("kind")
            .and_then(|raw| SignalKind::from_str(raw));
        let undelivered_only = params
            .filters
            .get("undelivered")
            .map(|raw| raw == "true")
            .unwrap_or(false);
        let mut items = state.call_signals(call.id);
        items.retain(|signal| {
            if let Some(recipient) = &signal.recipient_id {
                if recipient != &caller.actor {
                    return false;
                }
            }
            if let Some(kind) = kind_filter {
                if signal.kind != kind {
                    return false;
                }
            }
            if undelivered_only && signal.delivered {
                return false;
            }
            true
        });
        let views: Vec<Value> = items
            .iter()
            .map(|signal| serde_json::to_value(signal).unwrap_or(Value::Null))
            .collect();
        let page = paginate(views, &params);
        for signal in &items {
            if let Some(mut entry) = state.signals.get_mut(&signal.id) {
                entry.value_mut().delivered = true;
            }
        }
        Ok(page.to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// WebSocket endpoint (authenticated)

/// `GET /api/v1/phone/ws?call={id}`: a call-scoped view over
/// the shared hub, exactly like the meetings room feed. The
/// session subscribes with `global_hub().subscribe()` and only
/// forwards events for its call whose `recipient_id` is absent
/// (call broadcast) or matches the caller, never echoing the
/// caller's own messages back. Inbound text frames must be
/// JSON signalling messages (`{"type": "signal", "kind",
/// "recipient_id"?, "payload"}`) or `{"type": "ping"}`; they
/// go through the same `relay_signal` validation as the REST
/// endpoint, so a client cannot signal in a call it has not
/// joined, or to a user who is not in the call.
pub(crate) async fn phone_ws(
    state: web::Data<PhoneState>,
    req: HttpRequest,
    stream: web::Payload,
    query: web::Query<PhoneWsQuery>,
) -> HttpResponse {
    let tenant_id = match resolve_tenant(&req) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return err(error, false),
    };
    let actor = match caller(&req) {
        Ok(caller) => caller.actor,
        Err(error) => return err(error, false),
    };
    let call_id = match &query.call {
        Some(raw) => match parse_uuid(raw, "call") {
            Ok(id) => id,
            Err(error) => return err(error, false),
        },
        None => {
            return err(
                UnifiedError::bad_request("Query parameter call={id} is required"),
                false,
            )
        }
    };
    let call = match state.calls.get(&call_id).map(|entry| entry.value().clone()) {
        Some(call) => call,
        None => {
            return err(
                UnifiedError::not_found(format!("Call not found: {call_id}")),
                false,
            )
        }
    };
    if call.tenant_id != tenant_id {
        return err(
            UnifiedError::not_found(format!("Call not found: {call_id}")),
            false,
        );
    }
    match state
        .call_participants(call_id)
        .iter()
        .find(|participant| participant.user_id == actor)
    {
        Some(participant)
            if matches!(
                participant.status,
                super::types::ParticipantStatus::Joined
                    | super::types::ParticipantStatus::Ringing
                    | super::types::ParticipantStatus::Invited
            ) => {}
        _ => {
            return err(
                UnifiedError::forbidden(
                    "Only a participant of this call can open its WebSocket feed",
                ),
                false,
            );
        }
    }
    let (response, mut session, mut msg_stream) = match actix_ws::handle(&req, stream) {
        Ok(parts) => parts,
        Err(error) => {
            return err(
                UnifiedError::bad_request(format!("WebSocket handshake failed: {error}"))
                    .with_trace(format!("{error:?}")),
                false,
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
                            let parsed: Result<Value, _> =
                                serde_json::from_slice(text.as_ref());
                            let reply = match parsed {
                                Ok(value) => handle_ws_message(&state, &tenant_id, call_id, &actor, value).await,
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
                    let Ok(event) = broadcast else {
                        break;
                    };
                    // Only this call's events, only broadcasts or
                    // messages addressed to this caller, never the
                    // caller's own messages back.
                    let is_call_event = event
                        .get("call_id")
                        .and_then(|value| value.as_str())
                        == Some(call_id.to_string().as_str());
                    if !is_call_event {
                        continue;
                    }
                    let sender_is_self = event
                        .get("sender_id")
                        .and_then(|value| value.as_str())
                        == Some(actor.as_str());
                    if sender_is_self {
                        continue;
                    }
                    let addressed = match event.get("recipient_id") {
                        Some(Value::Null) | None => true,
                        Some(value) => value.as_str() == Some(actor.as_str()),
                    };
                    if !addressed {
                        continue;
                    }
                    let text = serde_json::to_string(&event).unwrap_or_default();
                    if session.text(text).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    response
}
