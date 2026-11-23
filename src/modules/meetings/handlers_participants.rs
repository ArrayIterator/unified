//! HTTP handlers: participants, waiting room and presence.

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

// ---------------------------------------------------------------------------
// Join / leave
// ---------------------------------------------------------------------------

pub(crate) async fn join_room(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<JoinRoomQuery>,
) -> HttpResponse {
    let result: UnifiedResult<JoinResult> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let role = caller_role(&req);
        let mut room = load_room(&state, &path.into_inner())?;
        // A host joining their own scheduled room starts it early
        // (room preparation); everyone else must wait for it to go
        // live.
        if room.status == RoomStatus::Scheduled {
            if !is_host(&room, &actor) {
                return Err(UnifiedError::unprocessable(
                    "This meeting has not started yet",
                ));
            }
            room.status = RoomStatus::Live;
            room.started_at = Some(Utc::now());
            room.updated_at = Utc::now();
            state.rooms.insert(room.id, room.clone());
            state.publish_room_event(
                "meeting.started",
                room.id,
                json!({ "host_id": actor.clone(), "started_at": room.started_at }),
            );
        }
        if room.status != RoomStatus::Live {
            return Err(UnifiedError::unprocessable(format!(
                "This meeting cannot be joined (current status: {:?})",
                room.status
            )));
        }
        // The caller's existing record, if any, decides the rejoin
        // path below.
        let existing = state
            .participants
            .get(&participant_key(room.id, &actor))
            .map(|entry| entry.value().clone());
        if let Some(existing) = &existing {
            // Removal and denial are final until the host re-admits
            // the user: a removed or denied participant cannot
            // simply rejoin and undo the host's decision.
            if existing.status == "removed" || existing.status == "denied" {
                return Err(UnifiedError::forbidden(
                    "You were removed from this meeting; the host must re-admit you",
                ));
            }
            if existing.status == "joined" {
                return Ok(JoinResult {
                    status: "joined".to_string(),
                    room,
                    participant: existing.clone(),
                });
            }
            if existing.status == "waiting" {
                return Ok(JoinResult {
                    status: "waiting".to_string(),
                    room,
                    participant: existing.clone(),
                });
            }
        }
        // The join token gates entry by room id: anyone who is not
        // the host (or a co-host rejoining on their standing) must
        // present the token from the join link. A room id alone is
        // not an invitation, and the token is what keeps private
        // and paid rooms closed. A removed or denied record never
        // reaches this check (rejected above), and a joined or
        // waiting record short-circuits above, so the co-host
        // exemption here only covers a co-host rejoining after
        // leaving.
        let is_co_host_rejoining = existing
            .as_ref()
            .is_some_and(|record| record.role == MeetingRole::CoHost);
        if !is_host(&room, &actor)
            && !is_co_host_rejoining
            && query.token.as_deref() != Some(room.join_token.as_str())
        {
            return Err(UnifiedError::forbidden(
                "A valid join token is required to join this meeting room",
            ));
        }
        // Paid rooms check the product entitlement before anyone is
        // admitted — including before they are parked in the waiting
        // room. The host and callers holding the verified
        // `meetings.host` permission skip the check; a role claimed
        // in a header never skips it on its own.
        check_entitlement(&state, &room, &actor, role.as_deref())?;
        let now = Utc::now();
        // Rejoining after leaving resumes the same attendance
        // record: the original joined_at (and role) are kept and
        // only left_at is cleared, so the stay history survives.
        let left_record = existing.filter(|record| record.status == "left");
        if room.waiting_room_enabled && !is_host(&room, &actor) {
            let waiting = match left_record {
                Some(mut record) => {
                    record.status = "waiting".to_string();
                    record.left_at = None;
                    record.last_seen = now;
                    record.updated_at = now;
                    record
                }
                None => Participant {
                    room_id: room.id,
                    user_id: actor.clone(),
                    role: MeetingRole::Participant,
                    status: "waiting".to_string(),
                    joined_at: None,
                    left_at: None,
                    last_seen: now,
                    audio_muted: false,
                    video_muted: false,
                    sharing_screen: false,
                    hand_raised: false,
                    created_at: now,
                    updated_at: now,
                },
            };
            state
                .participants
                .insert(participant_key(room.id, &actor), waiting.clone());
            state.emit(
                "meeting.waiting.requested",
                json!({
                    "room_id": room.id.to_string(),
                    "user_id": actor,
                    "host_id": room.host_id,
                }),
            );
            return Ok(JoinResult {
                status: "waiting".to_string(),
                room,
                participant: waiting,
            });
        }
        if joined_count(&state, room.id) >= room.max_participants {
            return Err(UnifiedError::conflict(
                "This meeting room is full",
            ));
        }
        let participant = match left_record {
            Some(mut record) => {
                record.status = "joined".to_string();
                record.left_at = None;
                record.last_seen = now;
                record.updated_at = now;
                record
            }
            None => Participant {
                room_id: room.id,
                user_id: actor.clone(),
                role: MeetingRole::Participant,
                status: "joined".to_string(),
                joined_at: Some(now),
                left_at: None,
                last_seen: now,
                audio_muted: false,
                video_muted: false,
                sharing_screen: false,
                hand_raised: false,
                created_at: now,
                updated_at: now,
            },
        };
        state
            .participants
            .insert(participant_key(room.id, &actor), participant.clone());
        state.publish_room_event(
            "meeting.joined",
            room.id,
            json!({
                "user_id": actor,
                "role": "participant",
                "joined_at": now.to_rfc3339(),
            }),
        );
        Ok(JoinResult {
            status: "joined".to_string(),
            room,
            participant,
        })
    })();
    match result {
        Ok(joined) => ok(joined),
        Err(error) => err(error),
    }
}

pub(crate) async fn leave_room(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Participant> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let room = load_room(&state, &path.into_inner())?;
        let mut participant = load_participant(&state, room.id, &actor)?;
        if participant.status != "joined" {
            return Err(UnifiedError::unprocessable(
                "Only a joined participant can leave the room",
            ));
        }
        let now = Utc::now();
        participant.status = "left".to_string();
        participant.left_at = Some(now);
        participant.sharing_screen = false;
        participant.updated_at = now;
        state
            .participants
            .insert(participant_key(room.id, &actor), participant.clone());
        state.publish_room_event(
            "meeting.left",
            room.id,
            json!({ "user_id": actor, "left_at": now.to_rfc3339() }),
        );
        Ok(participant)
    })();
    match result {
        Ok(participant) => ok(participant),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Participants, waiting room, presence, screen share
// ---------------------------------------------------------------------------

pub(crate) async fn list_participants(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<Participant>> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let room = load_room(&state, &path.into_inner())?;
        // Only people who have a record in this room (joined,
        // waiting, or already left) can list its participants.
        let _self_participant = load_participant(&state, room.id, &actor)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let role_filter = params.filters.get("role").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Participant> = state
            .participants
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|participant| participant.room_id == room.id)
            .filter(|participant| {
                if let Some(status) = &status_filter {
                    if &participant.status != status {
                        return false;
                    }
                }
                if let Some(role) = &role_filter {
                    let expected = match role.as_str() {
                        "host" => MeetingRole::Host,
                        "co_host" => MeetingRole::CoHost,
                        "participant" => MeetingRole::Participant,
                        _ => return false,
                    };
                    if participant.role != expected {
                        return false;
                    }
                }
                if !search.is_empty() && !participant.user_id.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

fn load_manager(
    state: &MeetingsState,
    room: &MeetingRoom,
    actor: &str,
) -> UnifiedResult<Option<Participant>> {
    if is_host(room, actor) {
        return Ok(None);
    }
    let participant = load_participant(state, room.id, actor)?;
    if !is_co_host(&participant) {
        return Err(UnifiedError::forbidden(
            "Only the room host or a co-host can do this",
        ));
    }
    Ok(Some(participant))
}

pub(crate) async fn promote_participant(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Participant> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let (raw_room, user_id) = path.into_inner();
        let room = load_room(&state, &raw_room)?;
        if !is_host(&room, &actor) {
            return Err(UnifiedError::forbidden(
                "Only the room host can promote a participant",
            ));
        }
        let mut participant = load_participant(&state, room.id, &user_id)?;
        if participant.status != "joined" {
            return Err(UnifiedError::unprocessable(
                "Only a joined participant can be promoted to co-host",
            ));
        }
        if participant.role == MeetingRole::Host {
            return Err(UnifiedError::bad_request(
                "The room host cannot be promoted",
            ));
        }
        participant.role = MeetingRole::CoHost;
        participant.updated_at = Utc::now();
        state
            .participants
            .insert(participant_key(room.id, &user_id), participant.clone());
        state.emit(
            "meeting.participant.promoted",
            json!({
                "room_id": room.id.to_string(),
                "user_id": user_id,
                "promoted_by": actor,
            }),
        );
        Ok(participant)
    })();
    match result {
        Ok(participant) => ok(participant),
        Err(error) => err(error),
    }
}

pub(crate) async fn demote_participant(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Participant> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let (raw_room, user_id) = path.into_inner();
        let room = load_room(&state, &raw_room)?;
        if !is_host(&room, &actor) {
            return Err(UnifiedError::forbidden(
                "Only the room host can demote a co-host",
            ));
        }
        let mut participant = load_participant(&state, room.id, &user_id)?;
        // Demote mirrors promote: only a joined co-host can be
        // demoted. A co-host who already left holds no live standing
        // to take away.
        if participant.status != "joined" {
            return Err(UnifiedError::unprocessable(
                "Only a joined co-host can be demoted",
            ));
        }
        if participant.role != MeetingRole::CoHost {
            return Err(UnifiedError::unprocessable(
                "Only a co-host can be demoted",
            ));
        }
        participant.role = MeetingRole::Participant;
        participant.updated_at = Utc::now();
        state
            .participants
            .insert(participant_key(room.id, &user_id), participant.clone());
        state.emit(
            "meeting.participant.demoted",
            json!({
                "room_id": room.id.to_string(),
                "user_id": user_id,
                "demoted_by": actor,
            }),
        );
        Ok(participant)
    })();
    match result {
        Ok(participant) => ok(participant),
        Err(error) => err(error),
    }
}

pub(crate) async fn remove_participant(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Participant> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let (raw_room, user_id) = path.into_inner();
        let room = load_room(&state, &raw_room)?;
        let manager = load_manager(&state, &room, &actor)?;
        if is_host(&room, &user_id) {
            return Err(UnifiedError::bad_request(
                "The room host cannot be removed; end or delete the room instead",
            ));
        }
        // A co-host may only remove plain participants, never another
        // co-host; the host may remove anyone except themselves.
        if manager.is_some() {
            let target = load_participant(&state, room.id, &user_id)?;
            if target.role == MeetingRole::CoHost {
                return Err(UnifiedError::forbidden(
                    "A co-host cannot remove another co-host",
                ));
            }
        }
        let mut participant = load_participant(&state, room.id, &user_id)?;
        if participant.status != "joined" && participant.status != "waiting" {
            return Err(UnifiedError::unprocessable(
                "Only a joined or waiting participant can be removed",
            ));
        }
        let now = Utc::now();
        participant.status = "removed".to_string();
        participant.left_at = Some(now);
        participant.sharing_screen = false;
        participant.updated_at = now;
        state
            .participants
            .insert(participant_key(room.id, &user_id), participant.clone());
        state.publish_room_event(
            "meeting.participant.removed",
            room.id,
            json!({ "user_id": user_id, "removed_by": actor }),
        );
        Ok(participant)
    })();
    match result {
        Ok(participant) => ok(participant),
        Err(error) => err(error),
    }
}

pub(crate) async fn admit_waiting(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Participant> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let (raw_room, user_id) = path.into_inner();
        let room = load_room(&state, &raw_room)?;
        let _manager = load_manager(&state, &room, &actor)?;
        let mut participant = load_participant(&state, room.id, &user_id)?;
        // Waiting users are admitted, and removed or denied users
        // are re-admitted by the host: that is the only way back in
        // after a removal or denial, since rejoining is refused for
        // those statuses (see join_room).
        if participant.status != "waiting"
            && participant.status != "removed"
            && participant.status != "denied"
        {
            return Err(UnifiedError::unprocessable(
                "Only a waiting, removed, or denied user can be admitted",
            ));
        }
        // The entitlement is re-checked at admission time: a purchase
        // may have lapsed while the user waited. The admitted user's
        // own standing is checked, not the admitter's roles.
        check_entitlement(&state, &room, &user_id, None)?;
        if joined_count(&state, room.id) >= room.max_participants {
            return Err(UnifiedError::conflict(
                "This meeting room is full",
            ));
        }
        let now = Utc::now();
        participant.status = "joined".to_string();
        // Re-admission resumes the same attendance record: a
        // removed or denied user who had joined before keeps their
        // original joined_at, exactly like a rejoin after leaving
        // in join_room. Only a user who never joined (waiting)
        // gets a fresh joined_at.
        if participant.joined_at.is_none() {
            participant.joined_at = Some(now);
        }
        participant.left_at = None;
        participant.last_seen = now;
        participant.updated_at = now;
        state
            .participants
            .insert(participant_key(room.id, &user_id), participant.clone());
        state.publish_room_event(
            "meeting.joined",
            room.id,
            json!({
                "user_id": user_id.clone(),
                "role": "participant",
                "admitted_by": actor.clone(),
                "joined_at": now.to_rfc3339(),
            }),
        );
        state.emit(
            "meeting.waiting.admitted",
            json!({
                "room_id": room.id.to_string(),
                "user_id": user_id,
                "admitted_by": actor,
            }),
        );
        Ok(participant)
    })();
    match result {
        Ok(participant) => ok(participant),
        Err(error) => err(error),
    }
}

pub(crate) async fn deny_waiting(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Participant> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let (raw_room, user_id) = path.into_inner();
        let room = load_room(&state, &raw_room)?;
        let _manager = load_manager(&state, &room, &actor)?;
        let mut participant = load_participant(&state, room.id, &user_id)?;
        if participant.status != "waiting" {
            return Err(UnifiedError::unprocessable(
                "Only a waiting user can be denied",
            ));
        }
        participant.status = "denied".to_string();
        participant.updated_at = Utc::now();
        state
            .participants
            .insert(participant_key(room.id, &user_id), participant.clone());
        state.emit(
            "meeting.waiting.denied",
            json!({
                "room_id": room.id.to_string(),
                "user_id": user_id,
                "denied_by": actor,
            }),
        );
        Ok(participant)
    })();
    match result {
        Ok(participant) => ok(participant),
        Err(error) => err(error),
    }
}

pub(crate) async fn list_presence(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<PresenceView>> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let room = load_room(&state, &path.into_inner())?;
        let _self_participant = load_participant(&state, room.id, &actor)?;
        let params = query.into_inner();
        let now = Utc::now();
        let mut items: Vec<PresenceView> = state
            .participants
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|participant| {
                participant.room_id == room.id
                    && participant.status == "joined"
                    && now.signed_duration_since(participant.last_seen).num_seconds()
                        <= PRESENCE_STALE_SECS
            })
            .map(|participant| PresenceView {
                user_id: participant.user_id,
                role: participant.role,
                last_seen: participant.last_seen,
                audio_muted: participant.audio_muted,
                video_muted: participant.video_muted,
                sharing_screen: participant.sharing_screen,
                hand_raised: participant.hand_raised,
            })
            .collect();
        items.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

pub(crate) async fn heartbeat_presence(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PresenceHeartbeatRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PresenceView> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let room = load_room(&state, &path.into_inner())?;
        let mut participant = load_participant(&state, room.id, &actor)?;
        if participant.status != "joined" {
            return Err(UnifiedError::unprocessable(
                "Only a joined participant can send a presence heartbeat",
            ));
        }
        participant.last_seen = Utc::now();
        if let Some(audio_muted) = body.audio_muted {
            participant.audio_muted = audio_muted;
        }
        if let Some(video_muted) = body.video_muted {
            participant.video_muted = video_muted;
        }
        if let Some(hand_raised) = body.hand_raised {
            participant.hand_raised = hand_raised;
        }
        participant.updated_at = Utc::now();
        state
            .participants
            .insert(participant_key(room.id, &actor), participant.clone());
        state.publish_room_event(
            "meeting.presence",
            room.id,
            json!({
                "user_id": actor,
                "role": role_name(participant.role),
                "audio_muted": participant.audio_muted,
                "video_muted": participant.video_muted,
                "hand_raised": participant.hand_raised,
                "sharing_screen": participant.sharing_screen,
                "last_seen": participant.last_seen.to_rfc3339(),
            }),
        );
        Ok(PresenceView {
            user_id: participant.user_id,
            role: participant.role,
            last_seen: participant.last_seen,
            audio_muted: participant.audio_muted,
            video_muted: participant.video_muted,
            sharing_screen: participant.sharing_screen,
            hand_raised: participant.hand_raised,
        })
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error),
    }
}

pub(crate) async fn set_screen_share(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ScreenShareRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Participant> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let room = load_room(&state, &path.into_inner())?;
        let mut participant = load_participant(&state, room.id, &actor)?;
        if participant.status != "joined" {
            return Err(UnifiedError::unprocessable(
                "Only a joined participant can share a screen",
            ));
        }
        if body.sharing && !participant.sharing_screen {
            // One sharer at a time: the clients render a single
            // shared screen, so a second share is refused instead of
            // silently replacing the first.
            let other_sharer = state.participants.iter().any(|entry| {
                let other = entry.value();
                other.room_id == room.id
                    && other.status == "joined"
                    && other.user_id != actor
                    && other.sharing_screen
            });
            if other_sharer {
                return Err(UnifiedError::conflict(
                    "Another participant is already sharing their screen in this room",
                ));
            }
        }
        participant.sharing_screen = body.sharing;
        participant.last_seen = Utc::now();
        participant.updated_at = Utc::now();
        state
            .participants
            .insert(participant_key(room.id, &actor), participant.clone());
        let event = if body.sharing {
            "meeting.screen_share.started"
        } else {
            "meeting.screen_share.stopped"
        };
        state.publish_room_event(
            event,
            room.id,
            json!({ "user_id": actor, "sharing": body.sharing }),
        );
        Ok(participant)
    })();
    match result {
        Ok(participant) => ok(participant),
        Err(error) => err(error),
    }
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    fn seed_live_room(state: &MeetingsState) -> MeetingRoom {
        let now = Utc::now();
        let room = MeetingRoom {
            id: Uuid::now_v7(),
            title: "Room".to_string(),
            description: None,
            host_id: "host-1".to_string(),
            status: RoomStatus::Live,
            access_model: AccessModel::Free,
            product_id: None,
            max_participants: 10,
            waiting_room_enabled: false,
            join_token: "token-xyz".to_string(),
            starts_at: Some(now - chrono::Duration::hours(1)),
            ends_at: None,
            timezone: "UTC".to_string(),
            recurrence_rule: None,
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            started_at: Some(now - chrono::Duration::hours(1)),
            ended_at: None,
            created_at: now,
            updated_at: now,
        };
        state.rooms.insert(room.id, room.clone());
        room
    }

    fn seed_participant(
        state: &MeetingsState,
        room_id: Uuid,
        user_id: &str,
        role: MeetingRole,
        status: &str,
        joined_at: Option<DateTime<Utc>>,
        left_at: Option<DateTime<Utc>>,
    ) -> Participant {
        let now = Utc::now();
        let participant = Participant {
            room_id,
            user_id: user_id.to_string(),
            role,
            status: status.to_string(),
            joined_at,
            left_at,
            last_seen: now,
            audio_muted: false,
            video_muted: false,
            sharing_screen: false,
            hand_raised: false,
            created_at: now,
            updated_at: now,
        };
        state
            .participants
            .insert(participant_key(room_id, user_id), participant.clone());
        participant
    }

    fn actor_request(user: &str) -> HttpRequest {
        verified_request_without_roles(user)
    }

    fn join_query(token: Option<&str>) -> web::Query<JoinRoomQuery> {
        web::Query(JoinRoomQuery {
            token: token.map(str::to_string),
        })
    }

    fn join_path(room: &MeetingRoom) -> web::Path<String> {
        web::Path::from(room.id.to_string())
    }

    #[tokio::test]
    async fn join_room_requires_join_token() {
        let state = web::Data::new(MeetingsState::default());
        let room = seed_live_room(&state);
        let no_token = join_room(
            state.clone(),
            actor_request("user-1"),
            join_path(&room),
            join_query(None),
        )
        .await;
        assert_eq!(no_token.status(), StatusCode::FORBIDDEN);
        let wrong_token = join_room(
            state.clone(),
            actor_request("user-1"),
            join_path(&room),
            join_query(Some("not-the-token")),
        )
        .await;
        assert_eq!(wrong_token.status(), StatusCode::FORBIDDEN);
        let with_token = join_room(
            state.clone(),
            actor_request("user-1"),
            join_path(&room),
            join_query(Some("token-xyz")),
        )
        .await;
        assert_eq!(with_token.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn join_room_host_needs_no_token() {
        let state = web::Data::new(MeetingsState::default());
        let room = seed_live_room(&state);
        let resp = join_room(
            state.clone(),
            actor_request("host-1"),
            join_path(&room),
            join_query(None),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn join_room_rejects_removed_and_denied() {
        let state = web::Data::new(MeetingsState::default());
        let room = seed_live_room(&state);
        seed_participant(
            &state,
            room.id,
            "removed-1",
            MeetingRole::Participant,
            "removed",
            Some(Utc::now() - chrono::Duration::hours(1)),
            Some(Utc::now()),
        );
        seed_participant(
            &state,
            room.id,
            "denied-1",
            MeetingRole::Participant,
            "denied",
            None,
            None,
        );
        for user in ["removed-1", "denied-1"] {
            let resp = join_room(
                state.clone(),
                actor_request(user),
                join_path(&room),
                join_query(Some("token-xyz")),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{user} must stay out");
        }
    }

    #[tokio::test]
    async fn join_room_rejoin_after_left_restores_record() {
        let state = web::Data::new(MeetingsState::default());
        let room = seed_live_room(&state);
        let first_joined = Utc::now() - chrono::Duration::hours(1);
        let left_at = Utc::now() - chrono::Duration::minutes(30);
        seed_participant(
            &state,
            room.id,
            "user-1",
            MeetingRole::Participant,
            "left",
            Some(first_joined),
            Some(left_at),
        );
        let resp = join_room(
            state.clone(),
            actor_request("user-1"),
            join_path(&room),
            join_query(Some("token-xyz")),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let restored = state
            .participants
            .get(&participant_key(room.id, "user-1"))
            .expect("participant record still exists")
            .value()
            .clone();
        assert_eq!(restored.status, "joined");
        assert_eq!(
            restored.joined_at,
            Some(first_joined),
            "rejoin keeps the original joined_at instead of starting a fresh record"
        );
        assert_eq!(restored.left_at, None);
    }

    #[tokio::test]
    async fn join_room_co_host_rejoin_needs_no_token() {
        // A co-host rejoining after leaving stands on their
        // host-granted standing, like the host: no token needed,
        // and the restored record keeps the co-host role and the
        // original joined_at.
        let state = web::Data::new(MeetingsState::default());
        let room = seed_live_room(&state);
        let first_joined = Utc::now() - chrono::Duration::hours(1);
        seed_participant(
            &state,
            room.id,
            "cohost-1",
            MeetingRole::CoHost,
            "left",
            Some(first_joined),
            Some(Utc::now() - chrono::Duration::minutes(30)),
        );
        let resp = join_room(
            state.clone(),
            actor_request("cohost-1"),
            join_path(&room),
            join_query(None),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let restored = state
            .participants
            .get(&participant_key(room.id, "cohost-1"))
            .expect("participant record still exists")
            .value()
            .clone();
        assert_eq!(restored.status, "joined");
        assert_eq!(restored.role, MeetingRole::CoHost);
        assert_eq!(restored.joined_at, Some(first_joined));
        assert_eq!(restored.left_at, None);
    }

    #[tokio::test]
    async fn join_room_left_participant_still_needs_token() {
        // The co-host exemption is standing-based, not a general
        // rejoin exemption: a plain participant who left still
        // needs the token.
        let state = web::Data::new(MeetingsState::default());
        let room = seed_live_room(&state);
        seed_participant(
            &state,
            room.id,
            "user-1",
            MeetingRole::Participant,
            "left",
            Some(Utc::now() - chrono::Duration::hours(1)),
            Some(Utc::now() - chrono::Duration::minutes(30)),
        );
        let denied = join_room(
            state.clone(),
            actor_request("user-1"),
            join_path(&room),
            join_query(None),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn admit_waiting_re_admits_removed_keeps_original_joined_at() {
        // Host re-admission resumes the same attendance record:
        // the original joined_at survives, only left_at clears.
        let state = web::Data::new(MeetingsState::default());
        let room = seed_live_room(&state);
        let first_joined = Utc::now() - chrono::Duration::hours(1);
        seed_participant(
            &state,
            room.id,
            "removed-1",
            MeetingRole::Participant,
            "removed",
            Some(first_joined),
            Some(Utc::now()),
        );
        let resp = admit_waiting(
            state.clone(),
            actor_request("host-1"),
            web::Path::from((room.id.to_string(), "removed-1".to_string())),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let admitted = state
            .participants
            .get(&participant_key(room.id, "removed-1"))
            .expect("participant record still exists")
            .value()
            .clone();
        assert_eq!(admitted.status, "joined");
        assert_eq!(
            admitted.joined_at,
            Some(first_joined),
            "re-admission keeps the original joined_at instead of starting a fresh stay"
        );
        assert_eq!(admitted.left_at, None);
    }

    #[tokio::test]
    async fn demote_participant_requires_joined() {
        let state = web::Data::new(MeetingsState::default());
        let room = seed_live_room(&state);
        seed_participant(
            &state,
            room.id,
            "left-cohost",
            MeetingRole::CoHost,
            "left",
            Some(Utc::now() - chrono::Duration::hours(1)),
            Some(Utc::now()),
        );
        seed_participant(
            &state,
            room.id,
            "joined-cohost",
            MeetingRole::CoHost,
            "joined",
            Some(Utc::now()),
            None,
        );
        let demote_path = |user: &str| {
            web::Path::from((room.id.to_string(), user.to_string()))
        };
        let denied = demote_participant(
            state.clone(),
            actor_request("host-1"),
            demote_path("left-cohost"),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let allowed = demote_participant(
            state.clone(),
            actor_request("host-1"),
            demote_path("joined-cohost"),
        )
        .await;
        assert_eq!(allowed.status(), StatusCode::OK);
        assert_eq!(
            state
                .participants
                .get(&participant_key(room.id, "joined-cohost"))
                .unwrap()
                .role,
            MeetingRole::Participant
        );
    }

    #[tokio::test]
    async fn admit_waiting_re_admits_removed_participant() {
        let state = web::Data::new(MeetingsState::default());
        let room = seed_live_room(&state);
        seed_participant(
            &state,
            room.id,
            "removed-1",
            MeetingRole::Participant,
            "removed",
            Some(Utc::now() - chrono::Duration::hours(1)),
            Some(Utc::now()),
        );
        let resp = admit_waiting(
            state.clone(),
            actor_request("host-1"),
            web::Path::from((room.id.to_string(), "removed-1".to_string())),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let admitted = state
            .participants
            .get(&participant_key(room.id, "removed-1"))
            .expect("participant record still exists")
            .value()
            .clone();
        assert_eq!(admitted.status, "joined");
        assert_eq!(admitted.left_at, None);
    }
}
