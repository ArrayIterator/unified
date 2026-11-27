//! HTTP handlers: rooms.

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

// Room handlers

pub(crate) async fn list_rooms(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    // Listing rooms is authenticated: without an actor the room
    // list (and the join tokens it used to carry) is enumerable by
    // anyone. The token itself is never serialized; see types.rs.
    if let Err(error) = require_tenant(&req) {
        return err(error);
    }
    if let Err(error) = actor_id(&req) {
        return err(error);
    }
    let params = query.into_inner();
    let status_filter = params.filters.get("status").cloned();
    let host_filter = params.filters.get("host_id").cloned();
    let model_filter = params.filters.get("access_model").cloned();
    let product_filter = params.filters.get("product_id").cloned();
    let search = params.search.clone().unwrap_or_default().to_lowercase();
    let mut items: Vec<MeetingRoom> = state
        .rooms
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|room| {
            if let Some(status) = &status_filter {
                let expected = match status.as_str() {
                    "scheduled" => RoomStatus::Scheduled,
                    "live" => RoomStatus::Live,
                    "ended" => RoomStatus::Ended,
                    "cancelled" => RoomStatus::Cancelled,
                    _ => return false,
                };
                if room.status != expected {
                    return false;
                }
            }
            if let Some(host_id) = &host_filter {
                if &room.host_id != host_id {
                    return false;
                }
            }
            if let Some(model) = &model_filter {
                let expected = match model.as_str() {
                    "free" => AccessModel::Free,
                    "room" => AccessModel::Room,
                    "seat" => AccessModel::Seat,
                    "minutes" => AccessModel::Minutes,
                    _ => return false,
                };
                if room.access_model != expected {
                    return false;
                }
            }
            if let Some(product_id) = &product_filter {
                if room.product_id.as_deref() != Some(product_id.as_str()) {
                    return false;
                }
            }
            if !search.is_empty() {
                let haystack = format!(
                    "{} {}",
                    room.title,
                    room.description.clone().unwrap_or_default()
                )
                .to_lowercase();
                if !haystack.contains(&search) {
                    return false;
                }
            }
            true
        })
        .collect();
    let sort = params.allowed_sort(&["title", "starts_at", "created_at"], "created_at");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "title" => a.title.cmp(&b.title),
            "starts_at" => a.starts_at.cmp(&b.starts_at),
            _ => a.created_at.cmp(&b.created_at),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    ok(paginate(items, &params))
}

pub(crate) async fn create_room(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    body: web::Json<CreateRoomRequest>,
) -> HttpResponse {
    let result: UnifiedResult<RoomWithToken> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let access_model = input.access_model.unwrap_or_default();
        let product_id = match &input.product_id {
            Some(raw) => Some(require_text(raw, "product_id", 128)?),
            None => None,
        };
        if access_model != AccessModel::Free && product_id.is_none() {
            return Err(UnifiedError::bad_request(
                "A paid access_model (room, seat, minutes) requires product_id",
            ));
        }
        if access_model == AccessModel::Free && product_id.is_some() {
            return Err(UnifiedError::bad_request(
                "A free room must not carry product_id; use access_model room, seat, or minutes for a paid room",
            ));
        }
        let max_participants = input.max_participants.unwrap_or(DEFAULT_MAX_PARTICIPANTS);
        if max_participants < MIN_PARTICIPANTS || max_participants > MAX_PARTICIPANTS {
            return Err(UnifiedError::bad_request(format!(
                "max_participants must be between {MIN_PARTICIPANTS} and {MAX_PARTICIPANTS}"
            )));
        }
        let timezone = match &input.timezone {
            Some(raw) => require_text(raw, "timezone", MAX_TIMEZONE_LEN)?,
            None => "UTC".to_string(),
        };
        let recurrence_rule = match &input.recurrence_rule {
            Some(raw) => Some(require_text(raw, "recurrence_rule", MAX_RECURRENCE_LEN)?),
            None => None,
        };
        validate_calendar(
            input.starts_at,
            input.ends_at,
            &timezone,
            recurrence_rule.as_deref(),
        )?;
        let now = Utc::now();
        // An instant room (no future start) is live immediately; a
        // future start stays scheduled until its start time.
        let (status, started_at) = match input.starts_at {
            Some(start) if start > now => (RoomStatus::Scheduled, None),
            _ => (RoomStatus::Live, Some(now)),
        };
        let mut room = MeetingRoom {
            id: Uuid::now_v7(),
            title,
            description,
            host_id: actor.clone(),
            status,
            access_model,
            product_id,
            max_participants,
            waiting_room_enabled: input.waiting_room_enabled.unwrap_or(false),
            join_token: Uuid::new_v4().to_string(),
            starts_at: input.starts_at,
            ends_at: input.ends_at,
            timezone,
            recurrence_rule,
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            started_at,
            ended_at: None,
            created_at: now,
            updated_at: now,
        };
        schedule_meeting(&state, &mut room);
        state.rooms.insert(room.id, room.clone());
        // The host is a joined participant from the start, so they
        // can signal and chat without a separate join call.
        let host_participant = Participant {
            room_id: room.id,
            user_id: actor.clone(),
            role: MeetingRole::Host,
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
        };
        state
            .participants
            .insert(participant_key(room.id, &actor), host_participant);
        state.emit(
            "meeting.room.created",
            json!({
                "room_id": room.id.to_string(),
                "meeting_id": room.id.to_string(),
                "host_id": actor,
                "status": format!("{:?}", room.status).to_lowercase(),
                "access_model": format!("{:?}", room.access_model).to_lowercase(),
                "product_id": room.product_id.clone(),
                "starts_at": room.starts_at,
                "ends_at": room.ends_at,
                "timezone": room.timezone.clone(),
                "recurrence_rule": room.recurrence_rule.clone(),
                "scheduler_job_id": room.scheduler_job_id.clone(),
                "scheduler_status": room.scheduler_status.clone(),
            }),
        );
        // Only the host's own create response carries the join
        // token; every other room response omits it.
        Ok(RoomWithToken::for_host(room))
    })();
    match result {
        Ok(room) => created(room),
        Err(error) => err(error),
    }
}

pub(crate) async fn get_room(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    // Authenticated like the list: an unauthenticated caller must
    // not be able to fetch (and previously harvest the join token
    // of) any room by id. The token is not serialized in the
    // response; see types.rs.
    if let Err(error) = require_tenant(&req) {
        return err(error);
    }
    if let Err(error) = actor_id(&req) {
        return err(error);
    }
    match load_room(&state, &path.into_inner()) {
        Ok(room) => ok(room),
        Err(error) => err(error),
    }
}

pub(crate) async fn update_room(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRoomRequest>,
) -> HttpResponse {
    let result: UnifiedResult<MeetingRoom> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let input = body.into_inner();
        let mut room = load_room(&state, &path.into_inner())?;
        if !is_host(&room, &actor) {
            return Err(UnifiedError::forbidden(
                "Only the room host can update the room",
            ));
        }
        if room.status == RoomStatus::Ended || room.status == RoomStatus::Cancelled {
            return Err(UnifiedError::unprocessable(
                "An ended or cancelled room cannot be updated",
            ));
        }
        let mut starts_changed = false;
        let mut ends_changed = false;
        let mut timezone_changed = false;
        let mut recurrence_changed = false;
        if let Some(title) = &input.title {
            room.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            room.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(access_model) = input.access_model {
            room.access_model = access_model;
        }
        if let Some(product_id) = &input.product_id {
            room.product_id = match product_id {
                Some(raw) => Some(require_text(raw, "product_id", 128)?),
                None => None,
            };
        }
        if room.access_model != AccessModel::Free && room.product_id.is_none() {
            return Err(UnifiedError::bad_request(
                "A paid access_model (room, seat, minutes) requires product_id",
            ));
        }
        if room.access_model == AccessModel::Free && room.product_id.is_some() {
            return Err(UnifiedError::bad_request(
                "A free room must not carry product_id; use access_model room, seat, or minutes for a paid room",
            ));
        }
        if let Some(max_participants) = input.max_participants {
            if max_participants < MIN_PARTICIPANTS || max_participants > MAX_PARTICIPANTS {
                return Err(UnifiedError::bad_request(format!(
                    "max_participants must be between {MIN_PARTICIPANTS} and {MAX_PARTICIPANTS}"
                )));
            }
            if max_participants < joined_count(&state, room.id) {
                return Err(UnifiedError::bad_request(
                    "max_participants cannot be lowered below the number of participants currently in the room",
                ));
            }
            room.max_participants = max_participants;
        }
        if let Some(waiting_room_enabled) = input.waiting_room_enabled {
            room.waiting_room_enabled = waiting_room_enabled;
        }
        if let Some(starts_at) = input.starts_at {
            if Some(starts_at) != room.starts_at {
                starts_changed = true;
            }
            room.starts_at = Some(starts_at);
        }
        if let Some(ends_at) = input.ends_at {
            if Some(ends_at) != room.ends_at {
                ends_changed = true;
            }
            room.ends_at = Some(ends_at);
        }
        if let Some(timezone) = &input.timezone {
            let timezone = require_text(timezone, "timezone", MAX_TIMEZONE_LEN)?;
            if timezone != room.timezone {
                timezone_changed = true;
            }
            room.timezone = timezone;
        }
        if let Some(recurrence_rule) = &input.recurrence_rule {
            let recurrence_rule = match recurrence_rule {
                Some(raw) => Some(require_text(raw, "recurrence_rule", MAX_RECURRENCE_LEN)?),
                None => None,
            };
            if recurrence_rule != room.recurrence_rule {
                recurrence_changed = true;
            }
            room.recurrence_rule = recurrence_rule;
        }
        // Only validate the calendar when the caller actually
        // changed a calendar field: a title-only edit to a live (or
        // overdue scheduled) room must not fail on its stored start
        // time, which is already in the past. When a calendar field
        // did change, the past-start rule is still skipped for rooms
        // that already started, and for an unchanged stored start.
        if starts_changed || ends_changed || timezone_changed || recurrence_changed {
            let already_started =
                room.status == RoomStatus::Live || room.started_at.is_some();
            validate_calendar_with(
                room.starts_at,
                room.ends_at,
                &room.timezone,
                room.recurrence_rule.as_deref(),
                already_started || !starts_changed,
            )?;
        }
        room.updated_at = Utc::now();
        if starts_changed {
            schedule_meeting(&state, &mut room);
        }
        state.rooms.insert(room.id, room.clone());
        state.emit(
            "meeting.room.updated",
            json!({
                "room_id": room.id.to_string(),
                "meeting_id": room.id.to_string(),
                "updated_by": actor,
                "scheduler_job_id": room.scheduler_job_id.clone(),
                "scheduler_status": room.scheduler_status.clone(),
            }),
        );
        Ok(room)
    })();
    match result {
        Ok(room) => ok(room),
        Err(error) => err(error),
    }
}

pub(crate) async fn delete_room(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let room = load_room(&state, &path.into_inner())?;
        if !is_host(&room, &actor) {
            return Err(UnifiedError::forbidden(
                "Only the room host can delete the room",
            ));
        }
        if room.status == RoomStatus::Live {
            return Err(UnifiedError::unprocessable(
                "A live room cannot be deleted; end it first",
            ));
        }
        let room_id = room.id;
        state.rooms.remove(&room_id);
        state
            .participants
            .retain(|_, participant| participant.room_id != room_id);
        state
            .messages
            .retain(|_, message| message.room_id != room_id);
        state
            .recordings
            .retain(|_, recording| recording.room_id != room_id);
        state.emit(
            "meeting.room.deleted",
            json!({ "room_id": room_id.to_string(), "deleted_by": actor }),
        );
        Ok(json!({ "deleted": true, "room_id": room_id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}

pub(crate) async fn start_room(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<MeetingRoom> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let mut room = load_room(&state, &path.into_inner())?;
        if !is_host(&room, &actor) {
            return Err(UnifiedError::forbidden(
                "Only the room host can start the room",
            ));
        }
        if room.status != RoomStatus::Scheduled {
            return Err(UnifiedError::unprocessable(format!(
                "Only a scheduled room can be started (current status: {:?})",
                room.status
            )));
        }
        room.status = RoomStatus::Live;
        room.started_at = Some(Utc::now());
        room.updated_at = Utc::now();
        state.rooms.insert(room.id, room.clone());
        state.publish_room_event(
            "meeting.started",
            room.id,
            json!({ "host_id": actor, "started_at": room.started_at }),
        );
        Ok(room)
    })();
    match result {
        Ok(room) => ok(room),
        Err(error) => err(error),
    }
}

/// Close every joined participant's stay and end the room.
fn close_room_participants(state: &MeetingsState, room_id: Uuid, now: DateTime<Utc>) {
    for mut entry in state.participants.iter_mut() {
        let participant = entry.value_mut();
        if participant.room_id == room_id && participant.status == "joined" {
            participant.status = "left".to_string();
            participant.left_at = Some(now);
            participant.sharing_screen = false;
            participant.updated_at = now;
        }
    }
}

pub(crate) async fn end_room(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<MeetingRoom> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let mut room = load_room(&state, &path.into_inner())?;
        if !is_host(&room, &actor) {
            return Err(UnifiedError::forbidden(
                "Only the room host can end the room",
            ));
        }
        if room.status != RoomStatus::Live {
            return Err(UnifiedError::unprocessable(format!(
                "Only a live room can be ended (current status: {:?})",
                room.status
            )));
        }
        let now = Utc::now();
        room.status = RoomStatus::Ended;
        room.ended_at = Some(now);
        room.updated_at = now;
        state.rooms.insert(room.id, room.clone());
        close_room_participants(&state, room.id, now);
        let attendance = state
            .participants
            .iter()
            .filter(|entry| {
                entry.value().room_id == room.id
                    && (entry.value().status == "left" || entry.value().status == "joined")
            })
            .count();
        state.publish_room_event(
            "meeting.ended",
            room.id,
            json!({
                "host_id": actor,
                "ended_at": room.ended_at,
                "attendance_count": attendance,
            }),
        );
        Ok(room)
    })();
    match result {
        Ok(room) => ok(room),
        Err(error) => err(error),
    }
}

pub(crate) async fn cancel_room(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<MeetingRoom> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let mut room = load_room(&state, &path.into_inner())?;
        if !is_host(&room, &actor) {
            return Err(UnifiedError::forbidden(
                "Only the room host can cancel the room",
            ));
        }
        if room.status == RoomStatus::Live {
            return Err(UnifiedError::unprocessable(
                "A live room cannot be cancelled; end it instead",
            ));
        }
        if room.status == RoomStatus::Ended || room.status == RoomStatus::Cancelled {
            return Err(UnifiedError::unprocessable(format!(
                "Room is already {:?}",
                room.status
            )));
        }
        room.status = RoomStatus::Cancelled;
        room.updated_at = Utc::now();
        state.rooms.insert(room.id, room.clone());
        state.emit(
            "meeting.cancelled",
            json!({ "room_id": room.id.to_string(), "cancelled_by": actor }),
        );
        Ok(room)
    })();
    match result {
        Ok(room) => ok(room),
        Err(error) => err(error),
    }
}

pub(crate) async fn regenerate_token(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<RoomWithToken> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let mut room = load_room(&state, &path.into_inner())?;
        if !is_host(&room, &actor) {
            return Err(UnifiedError::forbidden(
                "Only the room host can regenerate the join token",
            ));
        }
        // A token for an ended or cancelled room is meaningless:
        // the room can never be joined again, so regenerating it
        // only creates a live-looking link to a dead room.
        if room.status != RoomStatus::Scheduled && room.status != RoomStatus::Live {
            return Err(UnifiedError::unprocessable(
                "Join tokens can only be regenerated for a scheduled or live room",
            ));
        }
        room.join_token = Uuid::new_v4().to_string();
        room.updated_at = Utc::now();
        state.rooms.insert(room.id, room.clone());
        // Only the host's own regenerate response carries the new
        // token; every other room response omits it.
        Ok(RoomWithToken::for_host(room))
    })();
    match result {
        Ok(room) => ok(room),
        Err(error) => err(error),
    }
}

pub(crate) async fn join_by_token(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<MeetingRoom> = (|| {
        let _tenant = require_tenant(&req)?;
        let _actor = actor_id(&req)?;
        let token = path.into_inner();
        let room = state
            .rooms
            .iter()
            .find(|entry| entry.value().join_token == token)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Join link is invalid or has been revoked"))?;
        // Ended and cancelled rooms do not resolve: a join link to
        // a dead room is invalid, and its token stays unusable.
        if room.status == RoomStatus::Ended || room.status == RoomStatus::Cancelled {
            return Err(UnifiedError::not_found(
                "Join link is invalid or has been revoked",
            ));
        }
        Ok(room)
    })();
    match result {
        Ok(room) => ok(room),
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

    fn seed_room(
        state: &MeetingsState,
        status: RoomStatus,
        starts_at: Option<DateTime<Utc>>,
    ) -> MeetingRoom {
        let now = Utc::now();
        let room = MeetingRoom {
            id: Uuid::now_v7(),
            title: "Room".to_string(),
            description: None,
            host_id: "host-1".to_string(),
            status,
            access_model: AccessModel::Free,
            product_id: None,
            max_participants: 10,
            waiting_room_enabled: false,
            // Unique per room: two rooms must never share a token,
            // or a token lookup could resolve to the wrong room.
            join_token: format!("join-token-{}", Uuid::new_v4()),
            starts_at,
            ends_at: None,
            timezone: "UTC".to_string(),
            recurrence_rule: None,
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            started_at: if status == RoomStatus::Live { Some(now) } else { None },
            ended_at: None,
            created_at: now,
            updated_at: now,
        };
        state.rooms.insert(room.id, room.clone());
        room
    }

    fn actor_request(user: &str) -> HttpRequest {
        verified_request_without_roles(user)
    }

    fn title_only_update(title: &str) -> web::Json<UpdateRoomRequest> {
        web::Json(UpdateRoomRequest {
            title: Some(title.to_string()),
            description: None,
            access_model: None,
            product_id: None,
            max_participants: None,
            waiting_room_enabled: None,
            starts_at: None,
            ends_at: None,
            timezone: None,
            recurrence_rule: None,
        })
    }

    #[tokio::test]
    async fn list_rooms_requires_actor() {
        let state = web::Data::new(MeetingsState::default());
        let denied = list_rooms(
            state.clone(),
            TestRequest::default().to_http_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let allowed = list_rooms(
            state.clone(),
            actor_request("someone"),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(allowed.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_room_requires_actor() {
        let state = web::Data::new(MeetingsState::default());
        let room = seed_room(&state, RoomStatus::Scheduled, None);
        let denied = get_room(
            state.clone(),
            TestRequest::default().to_http_request(),
            web::Path::from(room.id.to_string()),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let allowed = get_room(
            state.clone(),
            actor_request("someone"),
            web::Path::from(room.id.to_string()),
        )
        .await;
        assert_eq!(allowed.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn update_room_title_only_edit_on_live_room_succeeds() {
        let state = web::Data::new(MeetingsState::default());
        let past = Utc::now() - chrono::Duration::hours(1);
        let room = seed_room(&state, RoomStatus::Live, Some(past));
        let resp = update_room(
            state.clone(),
            actor_request("host-1"),
            web::Path::from(room.id.to_string()),
            title_only_update("Renamed live room"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            state.rooms.get(&room.id).unwrap().title,
            "Renamed live room"
        );
    }

    #[tokio::test]
    async fn update_room_title_only_edit_on_overdue_scheduled_room_succeeds() {
        let state = web::Data::new(MeetingsState::default());
        let past = Utc::now() - chrono::Duration::hours(1);
        let room = seed_room(&state, RoomStatus::Scheduled, Some(past));
        let resp = update_room(
            state.clone(),
            actor_request("host-1"),
            web::Path::from(room.id.to_string()),
            title_only_update("Renamed overdue room"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn update_room_rejects_new_past_start_on_scheduled_room() {
        let state = web::Data::new(MeetingsState::default());
        let future = Utc::now() + chrono::Duration::hours(2);
        let room = seed_room(&state, RoomStatus::Scheduled, Some(future));
        let past = Utc::now() - chrono::Duration::hours(1);
        let resp = update_room(
            state.clone(),
            actor_request("host-1"),
            web::Path::from(room.id.to_string()),
            web::Json(UpdateRoomRequest {
                title: None,
                description: None,
                access_model: None,
                product_id: None,
                max_participants: None,
                waiting_room_enabled: None,
                starts_at: Some(past),
                ends_at: None,
                timezone: None,
                recurrence_rule: None,
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn regenerate_token_rejects_ended_room() {
        let state = web::Data::new(MeetingsState::default());
        let ended = seed_room(&state, RoomStatus::Ended, None);
        let denied = regenerate_token(
            state.clone(),
            actor_request("host-1"),
            web::Path::from(ended.id.to_string()),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let scheduled = seed_room(&state, RoomStatus::Scheduled, None);
        let allowed = regenerate_token(
            state.clone(),
            actor_request("host-1"),
            web::Path::from(scheduled.id.to_string()),
        )
        .await;
        assert_eq!(allowed.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn join_by_token_rejects_ended_room() {
        let state = web::Data::new(MeetingsState::default());
        let ended = seed_room(&state, RoomStatus::Ended, None);
        let denied = join_by_token(
            state.clone(),
            actor_request("someone"),
            web::Path::from(ended.join_token.clone()),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::NOT_FOUND);
        let live = seed_room(&state, RoomStatus::Live, None);
        let allowed = join_by_token(
            state.clone(),
            actor_request("someone"),
            web::Path::from(live.join_token.clone()),
        )
        .await;
        assert_eq!(allowed.status(), StatusCode::OK);
    }
}
