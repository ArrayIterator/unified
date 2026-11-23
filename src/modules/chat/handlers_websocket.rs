//! chat - handlers websocket.

use unified_core::error::{UnifiedError};
use actix_web::{web, HttpRequest, HttpResponse};
use actix_ws::Message;
use futures_util::StreamExt;
use tokio::sync::broadcast;
use super::{limits::*, types::*, hub::*, state::*, helpers::*, module::*, handlers_channels::*, handlers_messages::*, handlers_files::*, handlers_direct::*, handlers_presence::*, support_config::*, support_sessions::*, support_ratings::*, support_agents::*, support_canned::*, support_offline::*};



// ---------------------------------------------------------------------------
// WebSocket handler
// ---------------------------------------------------------------------------
pub(crate) async fn chat_ws(
    state: web::Data<ChatState>,
    req: HttpRequest,
    stream: web::Payload,
    query: web::Query<WsQuery>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error),
    };
    let actor = match actor_id(&req) {
        Ok(actor) => actor,
        Err(error) => return err(error),
    };
    let roles = caller_roles(&req);
    let channel_filter = match &query.channel {
        Some(raw) => match parse_uuid(raw, "channel") {
            Ok(id) => Some(id),
            Err(error) => return err(error),
        },
        None => None,
    };
    if let Some(channel_id) = channel_filter {
        match state.channels.get(&channel_id) {
            Some(entry) => {
                if !can_view(entry.value(), &actor, &roles) {
                    return err(UnifiedError::forbidden(
                        "Channel is not visible to this user",
                    ));
                }
            }
            None => {
                return err(UnifiedError::not_found(format!(
                    "Channel not found: {channel_id}"
                )));
            }
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
    let mut receiver = state.hub.subscribe();
    // The broadcast loop needs the actor, their roles, and the
    // channel store: an unfiltered subscription must still be
    // checked per event (see `broadcast_visible_to`).
    let task_state = state.clone();
    let task_actor = actor;
    let task_roles = roles;
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
                        Some(Ok(_)) => {}
                    }
                }
                broadcast = receiver.recv() => {
                    match broadcast {
                        Ok(event) => {
                            if let Some(channel_id) = channel_filter {
                                if event.channel_id != channel_id {
                                    continue;
                                }
                            } else if !broadcast_visible_to(
                                &task_state,
                                &event,
                                &task_actor,
                                &task_roles,
                            ) {
                                continue;
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


/// Per-event visibility for an unfiltered WebSocket subscription:
/// the event's channel must exist and be visible to the
/// subscriber (`can_view` — membership, unrestricted public, or a
/// role-restricted public channel the caller qualifies for).
/// Without this, a subscriber with no `?channel=` filter received
/// broadcasts for every channel, including private and DM channels
/// they cannot view. Unknown channels are never forwarded.
pub(crate) fn broadcast_visible_to(
    state: &ChatState,
    event: &ChatBroadcast,
    actor: &str,
    roles: &[String],
) -> bool {
    match state.channels.get(&event.channel_id) {
        Some(entry) => can_view(entry.value(), actor, roles),
        None => false,
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn channel(id: Uuid, kind: ChannelKind, members: Vec<&str>, roles: Vec<&str>) -> Channel {
        let now = Utc::now();
        Channel {
            id,
            name: "test".to_string(),
            kind,
            company_id: None,
            owner_id: "owner".to_string(),
            members: members.into_iter().map(str::to_string).collect(),
            roles: roles.into_iter().map(str::to_string).collect(),
            created_at: now,
            updated_at: now,
        }
    }

    fn event(channel_id: Uuid) -> ChatBroadcast {
        let now = Utc::now();
        ChatBroadcast {
            event: "chat.message".to_string(),
            channel_id,
            message: ChatMessage {
                id: Uuid::now_v7(),
                channel_id,
                author_id: "author".to_string(),
                body: "hello".to_string(),
                edited: false,
                reactions: Vec::new(),
                created_at: now,
                updated_at: now,
            },
        }
    }

    #[test]
    fn unfiltered_subscriber_only_sees_channels_they_can_view() {
        let state = ChatState::default();
        let private_id = Uuid::now_v7();
        let public_id = Uuid::now_v7();
        let dm_id = Uuid::now_v7();
        state.channels.insert(
            private_id,
            channel(private_id, ChannelKind::Private, vec!["alice"], vec![]),
        );
        state.channels.insert(
            public_id,
            channel(public_id, ChannelKind::Public, vec![], vec![]),
        );
        state.channels.insert(
            dm_id,
            channel(dm_id, ChannelKind::Dm, vec!["alice", "carol"], vec![]),
        );
        // A stranger sees only the unrestricted public channel.
        assert!(!broadcast_visible_to(&state, &event(private_id), "bob", &[]));
        assert!(broadcast_visible_to(&state, &event(public_id), "bob", &[]));
        assert!(!broadcast_visible_to(&state, &event(dm_id), "bob", &[]));
        // A member sees the private channel and the DM.
        assert!(broadcast_visible_to(&state, &event(private_id), "alice", &[]));
        assert!(broadcast_visible_to(&state, &event(dm_id), "alice", &[]));
        // Events for unknown channels are never forwarded.
        assert!(!broadcast_visible_to(&state, &event(Uuid::now_v7()), "alice", &[]));
    }
}
