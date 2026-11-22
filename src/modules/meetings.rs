// Meetings module — Zoom-like video meetings over WebRTC.
//
// Routes live under `/api/v1/meetings`:
//
//   Rooms:
//     GET/POST /rooms — list rooms / create a room (scheduled or instant).
//     GET/PATCH/DELETE /rooms/{id}.
//     POST /rooms/{id}/start, /end, /cancel, /regenerate-token.
//     GET  /join/{token} — resolve a join link token to its room.
//     POST /rooms/{id}/join, /leave.
//   People:
//     GET  /rooms/{id}/participants — paginated, filter by status / role.
//     POST /rooms/{id}/participants/{user_id}/promote, /demote, /remove.
//     POST /rooms/{id}/waiting/{user_id}/admit, /deny.
//     GET/POST /rooms/{id}/presence — live presence + heartbeat.
//     POST /rooms/{id}/screen-share — start / stop sharing a screen.
//   In-meeting:
//     GET/POST /rooms/{id}/messages — chat inside the meeting.
//     POST /rooms/{id}/signal — WebRTC signalling relay (offer /
//       answer / ICE candidate) over the shared WebSocket hub.
//     GET  /ws?room={id} — WebSocket feed of this room's events.
//   Recordings:
//     POST /rooms/{id}/recordings/start.
//     POST /rooms/{id}/recordings/{recording_id}/stop, /attach.
//     GET  /rooms/{id}/recordings.
//   History:
//     GET  /rooms/{id}/attendance — who joined, when, for how long.
//     GET  /history — ended and cancelled rooms.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-app room management (create / schedule / join / leave / waiting
// room / roles / presence), WebRTC *signalling* (offer / answer / ICE
// exchange relayed through the existing WebSocket hub), in-meeting
// chat, screen-share state events, and recording bookkeeping whose
// bytes live in the existing storage module. What is NOT built here:
// the actual media transport is browser-to-browser WebRTC — there is
// NO media server and NO SFU (Selective Forwarding Unit) in this
// round, so there is no server-side mixing, transcoding, or recording
// of media streams. A group call therefore depends on every
// participant's browser keeping a peer connection to every other
// participant. Follow-up (not started here): add an SFU / media
// server (for example a Janus / LiveKit / mediasoup gateway) and a
// server-side recording pipeline; until then the recording endpoints
// below only track metadata and reference objects already stored
// through the storage module.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required), exactly
// like the chat module. Role checks use the optional `X-User-Roles`
// header (comma separated) as an interim source.
//
// Events emitted on the hook bus:
//   - `meeting.room.created`, `meeting.room.updated`,
//     `meeting.room.deleted`, `meeting.started`, `meeting.ended`,
//     `meeting.cancelled`
//   - `meeting.joined`, `meeting.left`
//   - `meeting.waiting.requested`, `meeting.waiting.admitted`,
//     `meeting.waiting.denied`
//   - `meeting.participant.promoted`, `meeting.participant.demoted`,
//     `meeting.participant.removed`
//   - `meeting.presence`, `meeting.screen_share.started`,
//     `meeting.screen_share.stopped`
//   - `meeting.chat.message`
//   - `meeting.signal` (one per relayed signalling message)
//   - `meeting.recording.started`, `meeting.recording.stopped`,
//     `meeting.recording.stored`
// Events this module listens to:
//   - `scheduler.enqueue` is *emitted* (not listened to) when a room
//     is scheduled: the scheduler module listens to it and queues a
//     job from `{"kind", "payload"}`. See `schedule_meeting`.
//   - `meeting.start_requested` is listened to: that is the event the
//     scheduler would emit when a `meeting.start` job fires, following
//     its own naming rule (`backup.run` -> `backup.run_requested`).
// Events this module emits to other modules:
//   - `product.entitlement.check` with `{"product_id", "user_id"}`,
//     expecting `{"entitled": bool}` in the first hook result. Paid
//     rooms fail closed (503) when no handler answers; see
//     `check_entitlement`.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::storage::StorageProvider;
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

/// Maximum lengths enforced on user supplied text.
const MAX_TITLE_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 2000;
const MAX_TIMEZONE_LEN: usize = 64;
const MAX_RECURRENCE_LEN: usize = 128;
const MAX_CHAT_BODY_LEN: usize = 2000;
const MAX_PARTICIPANTS: usize = 500;
const MIN_PARTICIPANTS: usize = 2;
const DEFAULT_MAX_PARTICIPANTS: usize = 100;
/// Presence entries older than this are stale and hidden.
const PRESENCE_STALE_SECS: i64 = 60;
/// Default signed-URL lifetime for a stored recording (1 hour).
const RECORDING_SIGNED_URL_SECS: u64 = 3600;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A participant's role inside one room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingRole {
    Host,
    CoHost,
    Participant,
}

impl Default for MeetingRole {
    fn default() -> Self {
        Self::Participant
    }
}

/// Lifecycle of a room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomStatus {
    Scheduled,
    Live,
    Ended,
    Cancelled,
}

/// How access to a room is sold through the store / product
/// management. `free` needs no product; the other models all require
/// a `product_id` on the room and an entitlement check at join time:
/// `room` sells the hosted room itself, `seat` sells one seat in it,
/// `minutes` sells meeting minutes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessModel {
    Free,
    Room,
    Seat,
    Minutes,
}

impl Default for AccessModel {
    fn default() -> Self {
        Self::Free
    }
}

/// One meeting room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingRoom {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub host_id: String,
    pub status: RoomStatus,
    pub access_model: AccessModel,
    /// Store product id that grants access, or `None` for a free room.
    /// The product itself (name, price, billing) lives in the store
    /// module; only the reference is kept here.
    pub product_id: Option<String>,
    pub max_participants: usize,
    pub waiting_room_enabled: bool,
    /// Opaque token for the join link
    /// (`/api/v1/meetings/join/{join_token}`). Regenerating it
    /// invalidates every previously shared link.
    pub join_token: String,
    /// Calendar data. Times are stored as UTC; `timezone` is the IANA
    /// name the host scheduled in (format-checked only, never used to
    /// shift the stored UTC times).
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub timezone: String,
    /// Recurrence in the scheduler's own schedule format: an interval
    /// in seconds (`"604800"`) or a cron-like expression
    /// (`"0 9 * * 1"`). Only meaningful together with `starts_at`.
    pub recurrence_rule: Option<String>,
    /// Scheduler linkage, filled by `schedule_meeting`: the queue item
    /// id when the scheduler accepted the job, and one of
    /// `not_scheduled`, `queued`, `declined`.
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One person's stay in one room. The same record is both the live
/// participant list and the attendance history: `left_at` is `None`
/// while the person is still in the room, and `duration_secs` is
/// derived from `joined_at` / `left_at` when the room is read back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    pub room_id: Uuid,
    pub user_id: String,
    pub role: MeetingRole,
    /// `waiting`, `joined`, `left`, `denied`, `removed`.
    pub status: String,
    pub joined_at: Option<DateTime<Utc>>,
    pub left_at: Option<DateTime<Utc>>,
    pub last_seen: DateTime<Utc>,
    pub audio_muted: bool,
    pub video_muted: bool,
    pub sharing_screen: bool,
    pub hand_raised: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One in-meeting chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingMessage {
    pub id: Uuid,
    pub room_id: Uuid,
    pub author_id: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

/// One recording. Only metadata lives here: the media bytes are
/// uploaded by the recording client through the storage module, and
/// this record references the stored object by target + key (+ an
/// optional signed URL minted at attach time). No second storage
/// system is built here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recording {
    pub id: Uuid,
    pub room_id: Uuid,
    pub started_by: String,
    /// `recording`, `stopped`, `stored`.
    pub status: String,
    pub storage_target: Option<String>,
    pub storage_key: Option<String>,
    pub storage_url: Option<String>,
    pub size_bytes: Option<u64>,
    pub duration_secs: Option<u64>,
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Attendance view: one participant plus the derived stay length.
#[derive(Debug, Clone, Serialize)]
pub struct AttendanceView {
    pub user_id: String,
    pub role: MeetingRole,
    pub status: String,
    pub joined_at: Option<DateTime<Utc>>,
    pub left_at: Option<DateTime<Utc>>,
    /// Seconds between join and leave (or join and now, while the
    /// person is still in the room); `None` for waiting / denied.
    pub duration_secs: Option<i64>,
}

/// Live presence view for one joined participant.
#[derive(Debug, Clone, Serialize)]
pub struct PresenceView {
    pub user_id: String,
    pub role: MeetingRole,
    pub last_seen: DateTime<Utc>,
    pub audio_muted: bool,
    pub video_muted: bool,
    pub sharing_screen: bool,
    pub hand_raised: bool,
}

/// Result of a join attempt: `joined`, or `waiting` when the room's
/// waiting room held the caller for admission.
#[derive(Debug, Clone, Serialize)]
pub struct JoinResult {
    pub status: String,
    pub room: MeetingRoom,
    pub participant: Participant,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateRoomRequest {
    pub title: String,
    pub description: Option<String>,
    pub access_model: Option<AccessModel>,
    pub product_id: Option<String>,
    pub max_participants: Option<usize>,
    pub waiting_room_enabled: Option<bool>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub timezone: Option<String>,
    pub recurrence_rule: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRoomRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub access_model: Option<AccessModel>,
    pub product_id: Option<Option<String>>,
    pub max_participants: Option<usize>,
    pub waiting_room_enabled: Option<bool>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub timezone: Option<String>,
    pub recurrence_rule: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct PresenceHeartbeatRequest {
    pub audio_muted: Option<bool>,
    pub video_muted: Option<bool>,
    pub hand_raised: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ScreenShareRequest {
    pub sharing: bool,
}

#[derive(Debug, Deserialize)]
pub struct PostMeetingMessageRequest {
    pub body: String,
}

/// One WebRTC signalling message. `payload` is the SDP (offer /
/// answer) or ICE candidate object, passed through untouched — the
/// server never parses media descriptions, it only routes them.
#[derive(Debug, Deserialize)]
pub struct SignalRequest {
    pub kind: SignalKind,
    pub recipient_id: Option<String>,
    pub payload: Value,
}

/// Signalling message kinds. `ice_candidate` carries one ICE
/// candidate; trickle ICE sends one request per candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    Offer,
    Answer,
    IceCandidate,
}

#[derive(Debug, Deserialize)]
pub struct AttachRecordingRequest {
    pub storage_target: String,
    pub storage_key: String,
    pub url: Option<String>,
    pub size_bytes: Option<u64>,
    pub duration_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct MeetingsWsQuery {
    pub room: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for meetings. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); handlers tolerate a missing bus
/// by skipping event emission, except the entitlement check, which
/// fails closed when there is no bus to ask. Durable rows for these
/// types are defined by the migration sketch in the module report;
/// until that migration lands, rooms do not survive a restart.
#[derive(Clone)]
pub struct MeetingsState {
    pub rooms: Arc<DashMap<Uuid, MeetingRoom>>,
    /// Keyed by `"{room_id}:{user_id}"`.
    pub participants: Arc<DashMap<String, Participant>>,
    pub messages: Arc<DashMap<Uuid, MeetingMessage>>,
    pub recordings: Arc<DashMap<Uuid, Recording>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for MeetingsState {
    fn default() -> Self {
        Self {
            rooms: Arc::new(DashMap::new()),
            participants: Arc::new(DashMap::new()),
            messages: Arc::new(DashMap::new()),
            recordings: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl MeetingsState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Publish a room event on the shared WebSocket hub (so every
    /// connected `GET /ws` client and every `GET /api/v1/meetings/ws`
    /// session can see it) and on the hook bus (so other modules can
    /// react to it). The hub is a global broadcast channel: events
    /// carry `room_id` and, for directed events, `recipient_id`, and
    /// receivers filter on those fields.
    fn publish_room_event(&self, event: &str, room_id: Uuid, mut data: Value) {
        if let Value::Object(map) = &mut data {
            map.insert("event".to_string(), Value::String(event.to_string()));
            map.insert("room_id".to_string(), Value::String(room_id.to_string()));
        }
        global_hub().broadcast(data.clone());
        self.emit(event, data);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn actor_id(req: &HttpRequest) -> UnifiedResult<String> {
    req.headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))
}

/// Roles claimed by the caller via the interim `X-User-Roles` header.
fn caller_roles(req: &HttpRequest) -> Vec<String> {
    req.headers()
        .get("x-user-roles")
        .and_then(|value| value.to_str().ok())
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|role| !role.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Host override capability: the room host, and callers holding the
/// `admin` or `meetings.host` role, bypass the entitlement check for
/// paid rooms. Everyone else must hold the product entitlement.
fn has_host_override(room: &MeetingRoom, user_id: &str, roles: &[String]) -> bool {
    room.host_id == user_id
        || roles.iter().any(|role| role == "admin" || role == "meetings.host")
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError) -> HttpResponse {
    json::error_response(&error, false)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn participant_key(room_id: Uuid, user_id: &str) -> String {
    format!("{room_id}:{user_id}")
}

fn load_room(state: &MeetingsState, raw_id: &str) -> UnifiedResult<MeetingRoom> {
    let id = parse_uuid(raw_id, "room")?;
    state
        .rooms
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Meeting room not found: {id}")))
}

fn load_participant(
    state: &MeetingsState,
    room_id: Uuid,
    user_id: &str,
) -> UnifiedResult<Participant> {
    state
        .participants
        .get(&participant_key(room_id, user_id))
        .map(|entry| entry.value().clone())
        .ok_or_else(|| {
            UnifiedError::not_found(format!("User {user_id} is not a participant of this room"))
        })
}

fn is_host(room: &MeetingRoom, user_id: &str) -> bool {
    room.host_id == user_id
}

fn is_co_host(participant: &Participant) -> bool {
    participant.role == MeetingRole::CoHost && participant.status == "joined"
}

/// The wire name of a role, matching its serde spelling.
fn role_name(role: MeetingRole) -> &'static str {
    match role {
        MeetingRole::Host => "host",
        MeetingRole::CoHost => "co_host",
        MeetingRole::Participant => "participant",
    }
}

fn can_manage_room(room: &MeetingRoom, participant: Option<&Participant>, user_id: &str) -> bool {
    if is_host(room, user_id) {
        return true;
    }
    participant.is_some_and(is_co_host)
}

fn joined_count(state: &MeetingsState, room_id: Uuid) -> usize {
    state
        .participants
        .iter()
        .filter(|entry| entry.value().room_id == room_id && entry.value().status == "joined")
        .count()
}

/// A timezone is an IANA name (`"Asia/Jakarta"`, `"UTC"`): letters,
/// digits, `_`, `-`, `+`, and `/` only, no empty segments. This
/// checks the shape only; no timezone conversion is done anywhere
/// (all stored times are UTC).
fn valid_timezone(timezone: &str) -> bool {
    if timezone.is_empty() || timezone.len() > MAX_TIMEZONE_LEN {
        return false;
    }
    if timezone == "UTC" {
        return true;
    }
    timezone
        .split('/')
        .all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+')
        })
        && timezone.contains('/')
}

/// A recurrence / schedule string, using the scheduler's own rules
/// (`scheduler.rs` `valid_schedule`): an interval in seconds, or five
/// cron-like fields where each field is `*`, `*/N`, or a plain
/// number. Kept in sync with the scheduler by hand; if the scheduler
/// accepts a new schedule shape, this must accept it too.
fn valid_schedule(schedule: &str) -> bool {
    let schedule = schedule.trim();
    if schedule.is_empty() {
        return false;
    }
    if schedule.parse::<u64>().is_ok() {
        return true;
    }
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    fields.iter().all(|field| {
        if *field == "*" {
            return true;
        }
        if let Some(step) = field.strip_prefix("*/") {
            return step.parse::<u32>().is_ok_and(|n| n > 0);
        }
        field.parse::<u32>().is_ok()
    })
}

fn validate_calendar(
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
    timezone: &str,
    recurrence_rule: Option<&str>,
) -> UnifiedResult<()> {
    if !valid_timezone(timezone) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid timezone: {timezone}. Use an IANA name such as \"Asia/Jakarta\" or \"UTC\""
        )));
    }
    if let Some(rule) = recurrence_rule {
        if !valid_schedule(rule) {
            return Err(UnifiedError::bad_request(
                "recurrence_rule must be an interval in seconds or a cron-like expression such as \"0 9 * * 1\"",
            ));
        }
        if starts_at.is_none() {
            return Err(UnifiedError::bad_request(
                "recurrence_rule requires starts_at: a recurring meeting must have a first occurrence",
            ));
        }
    }
    match (starts_at, ends_at) {
        (Some(start), Some(end)) if end <= start => Err(UnifiedError::bad_request(
            "ends_at must be after starts_at",
        )),
        (None, Some(end)) if end <= Utc::now() => Err(UnifiedError::bad_request(
            "ends_at must be in the future for an instant meeting",
        )),
        (Some(start), None) if start <= Utc::now() => Err(UnifiedError::bad_request(
            "starts_at must be in the future for a scheduled meeting",
        )),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Entitlement (meetings as a product)
// ---------------------------------------------------------------------------

/// Check, at join time, whether `user_id` may enter a paid room.
///
/// Free rooms (`product_id` is `None`) skip the check entirely. The
/// room host and callers with the host override capability skip it
/// too. Otherwise the store / product module is asked through the
/// hook event `product.entitlement.check` with payload
/// `{"product_id": ..., "user_id": ...}`, and the first hook result
/// must carry `{"entitled": true}`.
///
/// Fail-closed on purpose: when no handler answers the event (no
/// product module registered yet, or the store has not wired the
/// check), joining a paid room answers 503 instead of letting
/// someone in for free. An explicit `{"entitled": false}` (or an
/// `error` in the result) answers 403.
fn check_entitlement(
    state: &MeetingsState,
    room: &MeetingRoom,
    user_id: &str,
    roles: &[String],
) -> UnifiedResult<()> {
    let Some(product_id) = &room.product_id else {
        return Ok(());
    };
    if has_host_override(room, user_id, roles) {
        return Ok(());
    }
    let Some(bus) = state.bus() else {
        return Err(UnifiedError::new(
            "Entitlement service is unavailable: no hook bus is attached",
            503,
        ));
    };
    let answer = bus.emit(
        "product.entitlement.check",
        json!({ "product_id": product_id, "user_id": user_id }),
    );
    match answer.results.first() {
        Some(result) if result.get("entitled").and_then(Value::as_bool) == Some(true) => Ok(()),
        Some(result) if result.get("error").is_some() => {
            let reason = result
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("entitlement check failed");
            Err(UnifiedError::new(
                format!("Entitlement check failed: {reason}"),
                502,
            ))
        }
        Some(_) => Err(UnifiedError::forbidden(
            "A paid product entitlement is required to join this meeting room",
        )),
        None => Err(UnifiedError::new(
            "Entitlement service did not answer product.entitlement.check: no handler is registered",
            503,
        )),
    }
}

// ---------------------------------------------------------------------------
// Scheduler linkage
// ---------------------------------------------------------------------------

/// Link a scheduled room to the scheduler module.
///
/// The scheduler listens to the `scheduler.enqueue` event with the
/// shape `{"kind", "payload"}` (see `scheduler.rs`
/// `on_scheduler_enqueue`). A meeting start is queued as kind
/// `meeting.start` with the room id, meeting id, start time, and
/// host id in the payload. The result is read back from the first
/// hook result: `{"queued": true, "job_id": ...}` means the scheduler
/// accepted the job.
///
/// CURRENT LIMIT, stated plainly: the scheduler's `KNOWN_KINDS` list
/// does not contain a meeting kind yet, so today the scheduler
/// answers `{"queued": false, "error": "Unknown job kind: ..."}`
/// and this function records `scheduler_status: "declined"` instead
/// of pretending the reminder was scheduled. The room is still
/// created and stays usable; the follow-up is to add a
/// `meeting.start` kind to the scheduler (which would then emit
/// `meeting.start_requested`, the event this module already listens
/// to below). Rooms without `starts_at` are never queued and keep
/// `scheduler_status: "not_scheduled"`.
fn schedule_meeting(state: &MeetingsState, room: &mut MeetingRoom) {
    let Some(starts_at) = room.starts_at else {
        room.scheduler_job_id = None;
        room.scheduler_status = "not_scheduled".to_string();
        return;
    };
    let Some(bus) = state.bus() else {
        room.scheduler_job_id = None;
        room.scheduler_status = "declined".to_string();
        return;
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "meeting.start",
            "payload": {
                "meeting_id": room.id.to_string(),
                "room_id": room.id.to_string(),
                "starts_at": starts_at.to_rfc3339(),
                "host_id": room.host_id,
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            room.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            room.scheduler_status = "queued".to_string();
        }
        _ => {
            room.scheduler_job_id = None;
            room.scheduler_status = "declined".to_string();
        }
    }
}

/// Answer `meeting.start_requested`: the event the scheduler emits
/// when a `meeting.start` job fires. A scheduled room whose start
/// time has arrived goes live; any other state is left alone, and an
/// unknown room id is ignored so a stale job can never resurrect a
/// deleted room.
fn on_meeting_start_requested(state: MeetingsState, ctx: &mut HookContext) {
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

// ---------------------------------------------------------------------------
// Signalling relay (WebRTC over the shared WebSocket hub)
// ---------------------------------------------------------------------------

/// Validate and relay one signalling message.
///
/// The shared hub (`crate::http::websocket::global_hub`) is a single
/// global broadcast channel: `broadcast` delivers to every connected
/// `GET /ws` client, and `subscribe` is how a session receives. There
/// is no per-room channel in the hub, so routing is done with payload
/// fields — every relayed event carries `room_id`, `sender_id`, and,
/// for directed messages, `recipient_id` — and receivers filter on
/// them (the meetings WebSocket endpoint does this server-side).
/// Validation is enforced here, before anything is broadcast, so a
/// participant can only signal inside rooms they have joined, and
/// only to recipients who are in the same room.
fn relay_signal(
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

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct MeetingsModule {
    state: MeetingsState,
}

impl MeetingsModule {
    pub fn new() -> Self {
        Self {
            state: MeetingsState::default(),
        }
    }
}

impl Default for MeetingsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for MeetingsModule {
    fn id(&self) -> &'static str {
        "meetings"
    }

    fn name(&self) -> &'static str {
        "Meetings"
    }

    fn description(&self) -> &'static str {
        "Video meetings over WebRTC: rooms, waiting room, roles, signalling over the shared WebSocket hub, in-meeting chat, screen-share events, recording references, attendance, and paid rooms via product entitlements. Media transport is browser-to-browser WebRTC; no media server or SFU is built in."
    }

    /// Meetings is sold as a product: a customer buys a module
    /// entitlement for it (see `crate::modules::licensing`).
    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
    }

    fn requires(&self) -> &[&'static str] {
        &["scheduler", "storage", "store"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "scheduler", "storage", "store"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "meeting.start_requested",
            "meetings",
            Arc::new(move |ctx: &mut HookContext| {
                on_meeting_start_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/meetings")
                .app_data(state)
                .route("/rooms", web::get().to(list_rooms))
                .route("/rooms", web::post().to(create_room))
                .route("/rooms/{id}", web::get().to(get_room))
                .route("/rooms/{id}", web::patch().to(update_room))
                .route("/rooms/{id}", web::delete().to(delete_room))
                .route("/rooms/{id}/start", web::post().to(start_room))
                .route("/rooms/{id}/end", web::post().to(end_room))
                .route("/rooms/{id}/cancel", web::post().to(cancel_room))
                .route(
                    "/rooms/{id}/regenerate-token",
                    web::post().to(regenerate_token),
                )
                .route("/join/{token}", web::get().to(join_by_token))
                .route("/rooms/{id}/join", web::post().to(join_room))
                .route("/rooms/{id}/leave", web::post().to(leave_room))
                .route("/rooms/{id}/participants", web::get().to(list_participants))
                .route(
                    "/rooms/{id}/participants/{user_id}/promote",
                    web::post().to(promote_participant),
                )
                .route(
                    "/rooms/{id}/participants/{user_id}/demote",
                    web::post().to(demote_participant),
                )
                .route(
                    "/rooms/{id}/participants/{user_id}/remove",
                    web::post().to(remove_participant),
                )
                .route(
                    "/rooms/{id}/waiting/{user_id}/admit",
                    web::post().to(admit_waiting),
                )
                .route(
                    "/rooms/{id}/waiting/{user_id}/deny",
                    web::post().to(deny_waiting),
                )
                .route("/rooms/{id}/presence", web::get().to(list_presence))
                .route("/rooms/{id}/presence", web::post().to(heartbeat_presence))
                .route("/rooms/{id}/screen-share", web::post().to(set_screen_share))
                .route("/rooms/{id}/messages", web::get().to(list_messages))
                .route("/rooms/{id}/messages", web::post().to(post_message))
                .route("/rooms/{id}/signal", web::post().to(post_signal))
                .route("/rooms/{id}/recordings", web::get().to(list_recordings))
                .route(
                    "/rooms/{id}/recordings/start",
                    web::post().to(start_recording),
                )
                .route(
                    "/rooms/{id}/recordings/{recording_id}/stop",
                    web::post().to(stop_recording),
                )
                .route(
                    "/rooms/{id}/recordings/{recording_id}/attach",
                    web::post().to(attach_recording),
                )
                .route("/rooms/{id}/attendance", web::get().to(list_attendance))
                .route("/history", web::get().to(list_history))
                .route("/ws", web::get().to(meetings_ws)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/meetings/rooms", owner: "meetings", description: "List meeting rooms (paginated, filter by status, host_id, access_model, product_id)." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms", owner: "meetings", description: "Create a meeting room (scheduled or instant)." },
            RouteInfo { method: "GET", path: "/api/v1/meetings/rooms/{id}", owner: "meetings", description: "Get one meeting room." },
            RouteInfo { method: "PATCH", path: "/api/v1/meetings/rooms/{id}", owner: "meetings", description: "Update a meeting room (host only)." },
            RouteInfo { method: "DELETE", path: "/api/v1/meetings/rooms/{id}", owner: "meetings", description: "Delete a scheduled, ended, or cancelled room (host only)." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/start", owner: "meetings", description: "Start a scheduled room (host only)." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/end", owner: "meetings", description: "End a live room (host only)." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/cancel", owner: "meetings", description: "Cancel a scheduled room (host only)." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/regenerate-token", owner: "meetings", description: "Regenerate a room join token, invalidating old links (host only)." },
            RouteInfo { method: "GET", path: "/api/v1/meetings/join/{token}", owner: "meetings", description: "Resolve a join link token to its room." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/join", owner: "meetings", description: "Join a room (waiting room, seat limit, and paid entitlement enforced)." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/leave", owner: "meetings", description: "Leave a room (attendance is recorded)." },
            RouteInfo { method: "GET", path: "/api/v1/meetings/rooms/{id}/participants", owner: "meetings", description: "List room participants (paginated, filter by status, role)." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/participants/{user_id}/promote", owner: "meetings", description: "Promote a participant to co-host (host only)." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/participants/{user_id}/demote", owner: "meetings", description: "Demote a co-host to participant (host only)." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/participants/{user_id}/remove", owner: "meetings", description: "Remove a participant from a room (host or co-host)." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/waiting/{user_id}/admit", owner: "meetings", description: "Admit a waiting-room user into the room (host or co-host)." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/waiting/{user_id}/deny", owner: "meetings", description: "Deny a waiting-room user (host or co-host)." },
            RouteInfo { method: "GET", path: "/api/v1/meetings/rooms/{id}/presence", owner: "meetings", description: "Live presence of joined participants (stale entries hidden)." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/presence", owner: "meetings", description: "Presence heartbeat with media state (joined participants)." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/screen-share", owner: "meetings", description: "Start or stop sharing a screen (event only; media stays peer-to-peer)." },
            RouteInfo { method: "GET", path: "/api/v1/meetings/rooms/{id}/messages", owner: "meetings", description: "List in-meeting chat messages (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/messages", owner: "meetings", description: "Post an in-meeting chat message." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/signal", owner: "meetings", description: "Relay one WebRTC signalling message (offer, answer, ICE candidate) in a room." },
            RouteInfo { method: "GET", path: "/api/v1/meetings/rooms/{id}/recordings", owner: "meetings", description: "List recordings of a room (paginated, filter by status)." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/recordings/start", owner: "meetings", description: "Start a recording (metadata only; bytes go through the storage module)." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/recordings/{recording_id}/stop", owner: "meetings", description: "Stop a recording." },
            RouteInfo { method: "POST", path: "/api/v1/meetings/rooms/{id}/recordings/{recording_id}/attach", owner: "meetings", description: "Attach a stored recording object (storage target + key) to a stopped recording." },
            RouteInfo { method: "GET", path: "/api/v1/meetings/rooms/{id}/attendance", owner: "meetings", description: "Attendance of a room: who joined, when, for how long." },
            RouteInfo { method: "GET", path: "/api/v1/meetings/history", owner: "meetings", description: "Ended and cancelled rooms (paginated, filter by host_id, from, to)." },
            RouteInfo { method: "GET", path: "/api/v1/meetings/ws", owner: "meetings", description: "WebSocket feed of one room's events (?room={id}); also accepts signalling messages." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(MeetingsModule::new()));
}

// ---------------------------------------------------------------------------
// Room handlers
// ---------------------------------------------------------------------------

async fn list_rooms(
    state: web::Data<MeetingsState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
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

async fn create_room(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    body: web::Json<CreateRoomRequest>,
) -> HttpResponse {
    let result: UnifiedResult<MeetingRoom> = (|| {
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
                "product_id": room.product_id,
                "starts_at": room.starts_at,
                "ends_at": room.ends_at,
                "timezone": room.timezone,
                "recurrence_rule": room.recurrence_rule,
                "scheduler_job_id": room.scheduler_job_id,
                "scheduler_status": room.scheduler_status,
            }),
        );
        Ok(room)
    })();
    match result {
        Ok(room) => created(room),
        Err(error) => err(error),
    }
}

async fn get_room(
    state: web::Data<MeetingsState>,
    path: web::Path<String>,
) -> HttpResponse {
    match load_room(&state, &path.into_inner()) {
        Ok(room) => ok(room),
        Err(error) => err(error),
    }
}

async fn update_room(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRoomRequest>,
) -> HttpResponse {
    let result: UnifiedResult<MeetingRoom> = (|| {
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
            room.ends_at = Some(ends_at);
        }
        if let Some(timezone) = &input.timezone {
            room.timezone = require_text(timezone, "timezone", MAX_TIMEZONE_LEN)?;
        }
        if let Some(recurrence_rule) = &input.recurrence_rule {
            room.recurrence_rule = match recurrence_rule {
                Some(raw) => Some(require_text(raw, "recurrence_rule", MAX_RECURRENCE_LEN)?),
                None => None,
            };
        }
        validate_calendar(
            room.starts_at,
            room.ends_at,
            &room.timezone,
            room.recurrence_rule.as_deref(),
        )?;
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
                "scheduler_job_id": room.scheduler_job_id,
                "scheduler_status": room.scheduler_status,
            }),
        );
        Ok(room)
    })();
    match result {
        Ok(room) => ok(room),
        Err(error) => err(error),
    }
}

async fn delete_room(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
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

async fn start_room(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<MeetingRoom> = (|| {
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

async fn end_room(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<MeetingRoom> = (|| {
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

async fn cancel_room(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<MeetingRoom> = (|| {
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

async fn regenerate_token(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<MeetingRoom> = (|| {
        let actor = actor_id(&req)?;
        let mut room = load_room(&state, &path.into_inner())?;
        if !is_host(&room, &actor) {
            return Err(UnifiedError::forbidden(
                "Only the room host can regenerate the join token",
            ));
        }
        room.join_token = Uuid::new_v4().to_string();
        room.updated_at = Utc::now();
        state.rooms.insert(room.id, room.clone());
        Ok(room)
    })();
    match result {
        Ok(room) => ok(room),
        Err(error) => err(error),
    }
}

async fn join_by_token(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<MeetingRoom> = (|| {
        let _actor = actor_id(&req)?;
        let token = path.into_inner();
        state
            .rooms
            .iter()
            .find(|entry| entry.value().join_token == token)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Join link is invalid or has been revoked"))
    })();
    match result {
        Ok(room) => ok(room),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Join / leave
// ---------------------------------------------------------------------------

async fn join_room(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<JoinResult> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
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
                json!({ "host_id": actor, "started_at": room.started_at }),
            );
        }
        if room.status != RoomStatus::Live {
            return Err(UnifiedError::unprocessable(format!(
                "This meeting cannot be joined (current status: {:?})",
                room.status
            )));
        }
        // Rejoining after leaving resumes the same attendance record.
        if let Some(existing) = state
            .participants
            .get(&participant_key(room.id, &actor))
            .map(|entry| entry.value().clone())
        {
            if existing.status == "joined" {
                return Ok(JoinResult {
                    status: "joined".to_string(),
                    room,
                    participant: existing,
                });
            }
            if existing.status == "waiting" {
                return Ok(JoinResult {
                    status: "waiting".to_string(),
                    room,
                    participant: existing,
                });
            }
        }
        // Paid rooms check the product entitlement before anyone is
        // admitted — including before they are parked in the waiting
        // room. The host and host-override roles skip the check.
        check_entitlement(&state, &room, &actor, &roles)?;
        let now = Utc::now();
        if room.waiting_room_enabled && !is_host(&room, &actor) {
            let waiting = Participant {
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
        let participant = Participant {
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

async fn leave_room(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Participant> = (|| {
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

async fn list_participants(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<Participant>> = (|| {
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

async fn promote_participant(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Participant> = (|| {
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

async fn demote_participant(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Participant> = (|| {
        let actor = actor_id(&req)?;
        let (raw_room, user_id) = path.into_inner();
        let room = load_room(&state, &raw_room)?;
        if !is_host(&room, &actor) {
            return Err(UnifiedError::forbidden(
                "Only the room host can demote a co-host",
            ));
        }
        let mut participant = load_participant(&state, room.id, &user_id)?;
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

async fn remove_participant(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Participant> = (|| {
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

async fn admit_waiting(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Participant> = (|| {
        let actor = actor_id(&req)?;
        let (raw_room, user_id) = path.into_inner();
        let room = load_room(&state, &raw_room)?;
        let _manager = load_manager(&state, &room, &actor)?;
        let mut participant = load_participant(&state, room.id, &user_id)?;
        if participant.status != "waiting" {
            return Err(UnifiedError::unprocessable(
                "Only a waiting user can be admitted",
            ));
        }
        // The entitlement is re-checked at admission time: a purchase
        // may have lapsed while the user waited. The admitted user's
        // own standing is checked, not the admitter's roles.
        check_entitlement(&state, &room, &user_id, &[])?;
        if joined_count(&state, room.id) >= room.max_participants {
            return Err(UnifiedError::conflict(
                "This meeting room is full",
            ));
        }
        let now = Utc::now();
        participant.status = "joined".to_string();
        participant.joined_at = Some(now);
        participant.last_seen = now;
        participant.updated_at = now;
        state
            .participants
            .insert(participant_key(room.id, &user_id), participant.clone());
        state.publish_room_event(
            "meeting.joined",
            room.id,
            json!({
                "user_id": user_id,
                "role": "participant",
                "admitted_by": actor,
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

async fn deny_waiting(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Participant> = (|| {
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

async fn list_presence(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<PresenceView>> = (|| {
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

async fn heartbeat_presence(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PresenceHeartbeatRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PresenceView> = (|| {
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

async fn set_screen_share(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ScreenShareRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Participant> = (|| {
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

// ---------------------------------------------------------------------------
// In-meeting chat and signalling
// ---------------------------------------------------------------------------

async fn list_messages(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<MeetingMessage>> = (|| {
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

async fn post_message(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PostMeetingMessageRequest>,
) -> HttpResponse {
    let result: UnifiedResult<MeetingMessage> = (|| {
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

async fn post_signal(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SignalRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
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

// ---------------------------------------------------------------------------
// Recordings
// ---------------------------------------------------------------------------

async fn list_recordings(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<Recording>> = (|| {
        let actor = actor_id(&req)?;
        let room = load_room(&state, &path.into_inner())?;
        let participant = load_participant(&state, room.id, &actor)?;
        if participant.status != "joined" && !is_host(&room, &actor) {
            return Err(UnifiedError::forbidden(
                "Only a joined participant can list this room's recordings",
            ));
        }
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<Recording> = state
            .recordings
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|recording| recording.room_id == room.id)
            .filter(|recording| {
                status_filter
                    .as_ref()
                    .is_none_or(|status| &recording.status == status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

async fn start_recording(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Recording> = (|| {
        let actor = actor_id(&req)?;
        let room = load_room(&state, &path.into_inner())?;
        let participant = load_participant(&state, room.id, &actor)?;
        if !can_manage_room(&room, Some(&participant), &actor) {
            return Err(UnifiedError::forbidden(
                "Only the room host or a co-host can start a recording",
            ));
        }
        if room.status != RoomStatus::Live {
            return Err(UnifiedError::unprocessable(
                "A recording can only start in a live room",
            ));
        }
        let already_recording = state.recordings.iter().any(|entry| {
            entry.value().room_id == room.id && entry.value().status == "recording"
        });
        if already_recording {
            return Err(UnifiedError::conflict(
                "This room already has a recording in progress",
            ));
        }
        let now = Utc::now();
        let id = Uuid::now_v7();
        let recording = Recording {
            id,
            room_id: room.id,
            started_by: actor.clone(),
            status: "recording".to_string(),
            storage_target: None,
            // The default object key the recording client uploads to
            // through the storage module; the attach call confirms
            // the real target and key afterwards.
            storage_key: Some(format!("meetings/{}/{id}.webm", room.id)),
            storage_url: None,
            size_bytes: None,
            duration_secs: None,
            started_at: now,
            stopped_at: None,
            created_at: now,
            updated_at: now,
        };
        state.recordings.insert(id, recording.clone());
        state.publish_room_event(
            "meeting.recording.started",
            room.id,
            json!({
                "recording_id": id.to_string(),
                "started_by": actor,
                "storage_key": recording.storage_key,
            }),
        );
        Ok(recording)
    })();
    match result {
        Ok(recording) => created(recording),
        Err(error) => err(error),
    }
}

async fn stop_recording(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Recording> = (|| {
        let actor = actor_id(&req)?;
        let (raw_room, raw_recording) = path.into_inner();
        let room = load_room(&state, &raw_room)?;
        let recording_id = parse_uuid(&raw_recording, "recording")?;
        let mut recording = state
            .recordings
            .get(&recording_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Recording not found: {recording_id}")))?;
        if recording.room_id != room.id {
            return Err(UnifiedError::not_found(format!(
                "Recording not found: {recording_id}"
            )));
        }
        let participant = load_participant(&state, room.id, &actor)?;
        // The host, a co-host, or the user who started the recording
        // may stop it.
        let started_by_actor = recording.started_by == actor;
        if !started_by_actor && !can_manage_room(&room, Some(&participant), &actor) {
            return Err(UnifiedError::forbidden(
                "Only the room host, a co-host, or the user who started the recording can stop it",
            ));
        }
        if recording.status != "recording" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a recording in progress can be stopped (current status: {})",
                recording.status
            )));
        }
        let now = Utc::now();
        recording.status = "stopped".to_string();
        recording.stopped_at = Some(now);
        recording.updated_at = now;
        state.recordings.insert(recording.id, recording.clone());
        state.publish_room_event(
            "meeting.recording.stopped",
            room.id,
            json!({
                "recording_id": recording.id.to_string(),
                "stopped_by": actor,
                "stopped_at": now.to_rfc3339(),
            }),
        );
        Ok(recording)
    })();
    match result {
        Ok(recording) => ok(recording),
        Err(error) => err(error),
    }
}

async fn attach_recording(
    state: web::Data<MeetingsState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<AttachRecordingRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Recording> = (|| async {
        let actor = actor_id(&req)?;
        let (raw_room, raw_recording) = path.into_inner();
        let room = load_room(&state, &raw_room)?;
        let participant = load_participant(&state, room.id, &actor)?;
        if !can_manage_room(&room, Some(&participant), &actor) {
            return Err(UnifiedError::forbidden(
                "Only the room host or a co-host can attach a recording",
            ));
        }
        let recording_id = parse_uuid(&raw_recording, "recording")?;
        let mut recording = state
            .recordings
            .get(&recording_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Recording not found: {recording_id}"))
            })?;
        if recording.room_id != room.id {
            return Err(UnifiedError::not_found(format!(
                "Recording not found: {recording_id}"
            )));
        }
        if recording.status != "stopped" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a stopped recording can be attached to a stored object (current status: {})",
                recording.status
            )));
        }
        let input = body.into_inner();
        let storage_target = require_text(&input.storage_target, "storage_target", 128)?;
        let storage_key = require_text(&input.storage_key, "storage_key", MAX_KEY_LEN_STORAGE)?;
        // The URL is a reference, not a second copy of the bytes: use
        // the caller's URL when given, otherwise mint a signed URL
        // from the existing storage registry for that target.
        let storage_url = match input.url {
            Some(raw) => Some(require_text(&raw, "url", 2048)?),
            None => {
                let provider = ctx
                    .storage_registry
                    .get(&storage_target)
                    .ok_or_else(|| {
                        UnifiedError::not_found(format!(
                            "Storage target \"{storage_target}\" not found"
                        ))
                    })?;
                Some(
                    provider
                        .signed_url(&storage_key, RECORDING_SIGNED_URL_SECS)
                        .await?,
                )
            }
        };
        recording.status = "stored".to_string();
        recording.storage_target = Some(storage_target);
        recording.storage_key = Some(storage_key);
        recording.storage_url = storage_url;
        recording.size_bytes = input.size_bytes;
        recording.duration_secs = input.duration_secs;
        recording.updated_at = Utc::now();
        state.recordings.insert(recording.id, recording.clone());
        state.publish_room_event(
            "meeting.recording.stored",
            room.id,
            json!({
                "recording_id": recording.id.to_string(),
                "storage_target": recording.storage_target,
                "storage_key": recording.storage_key,
                "storage_url": recording.storage_url,
                "size_bytes": recording.size_bytes,
                "duration_secs": recording.duration_secs,
            }),
        );
        Ok(recording)
    })()
    .await;
    match result {
        Ok(recording) => ok(recording),
        Err(error) => err(error),
    }
}

/// Maximum storage key length for a recording attach, matching the
/// storage module's key limit.
const MAX_KEY_LEN_STORAGE: usize = 1024;

// ---------------------------------------------------------------------------
// Attendance and history
// ---------------------------------------------------------------------------

fn attendance_view(participant: &Participant) -> AttendanceView {
    let duration_secs = match (participant.joined_at, participant.left_at) {
        (Some(joined), Some(left)) => Some(left.signed_duration_since(joined).num_seconds()),
        (Some(joined), None) if participant.status == "joined" => {
            Some(Utc::now().signed_duration_since(joined).num_seconds())
        }
        _ => None,
    };
    AttendanceView {
        user_id: participant.user_id.clone(),
        role: participant.role,
        status: participant.status.clone(),
        joined_at: participant.joined_at,
        left_at: participant.left_at,
        duration_secs,
    }
}

async fn list_attendance(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<AttendanceView>> = (|| {
        let actor = actor_id(&req)?;
        let room = load_room(&state, &path.into_inner())?;
        let participant = load_participant(&state, room.id, &actor)?;
        if participant.status != "joined" && !is_host(&room, &actor) {
            return Err(UnifiedError::forbidden(
                "Only a joined participant can read this room's attendance",
            ));
        }
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<AttendanceView> = state
            .participants
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.room_id == room.id)
            .filter(|entry| {
                if let Some(user_id) = &user_filter {
                    if &entry.user_id != user_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &entry.status != status {
                        return false;
                    }
                }
                true
            })
            .map(|entry| attendance_view(&entry))
            .collect();
        items.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

async fn list_history(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
    range: web::Query<HistoryQuery>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<MeetingRoom>> = (|| {
        let _actor = actor_id(&req)?;
        let params = query.into_inner();
        let range = range.into_inner();
        let host_filter = params.filters.get("host_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<MeetingRoom> = state
            .rooms
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|room| {
                room.status == RoomStatus::Ended || room.status == RoomStatus::Cancelled
            })
            .filter(|room| {
                if let Some(status) = &status_filter {
                    let expected = match status.as_str() {
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
                if let Some(from) = range.from {
                    if room.created_at < from {
                        return false;
                    }
                }
                if let Some(to) = range.to {
                    if room.created_at > to {
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
        items.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Meetings WebSocket endpoint
// ---------------------------------------------------------------------------

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
async fn meetings_ws(
    state: web::Data<MeetingsState>,
    req: HttpRequest,
    stream: web::Payload,
    query: web::Query<MeetingsWsQuery>,
) -> HttpResponse {
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
