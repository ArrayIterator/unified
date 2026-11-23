//! Public data types, request/response shapes and their impls.

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

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_TITLE_LEN: usize = 200;
pub(crate) const MAX_DESCRIPTION_LEN: usize = 2000;
pub(crate) const MAX_TIMEZONE_LEN: usize = 64;
pub(crate) const MAX_RECURRENCE_LEN: usize = 128;
pub(crate) const MAX_CHAT_BODY_LEN: usize = 2000;
pub(crate) const MAX_PARTICIPANTS: usize = 500;
pub(crate) const MIN_PARTICIPANTS: usize = 2;
pub(crate) const DEFAULT_MAX_PARTICIPANTS: usize = 100;
/// Presence entries older than this are stale and hidden.
pub(crate) const PRESENCE_STALE_SECS: i64 = 60;
/// Default signed-URL lifetime for a stored recording (1 hour).
pub(crate) const RECORDING_SIGNED_URL_SECS: u64 = 3600;

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
    /// invalidates every previously shared link. The token is
    /// never serialized with the room: list/get/update/start/end/
    /// join responses carry no token, so it cannot be harvested by
    /// enumerating rooms. Only the host's create and regenerate
    /// responses include it, via `RoomWithToken`.
    #[serde(skip_serializing)]
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

/// A room plus its join token, returned only to the host by the
/// create and regenerate-token endpoints. Every other room response
/// serializes `MeetingRoom` directly, whose `join_token` is skipped.
#[derive(Debug, Clone, Serialize)]
pub struct RoomWithToken {
    #[serde(flatten)]
    pub room: MeetingRoom,
    pub join_token: String,
}

impl RoomWithToken {
    pub(crate) fn for_host(room: MeetingRoom) -> Self {
        let join_token = room.join_token.clone();
        Self { room, join_token }
    }
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

/// Query for joining by room id: the room's join token, required
/// unless the caller is the host. The token is what a join link
/// carries, so joining by id without it is refused.
#[derive(Debug, Deserialize)]
pub struct JoinRoomQuery {
    #[serde(default)]
    pub token: Option<String>,
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
    // Visible crate-wide: the sibling handler/validation modules
    // call these; without `pub(crate)` they could not (private
    // methods are only callable inside this file's module).
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
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
    pub(crate) fn publish_room_event(&self, event: &str, room_id: Uuid, mut data: Value) {
        if let Value::Object(map) = &mut data {
            map.insert("event".to_string(), Value::String(event.to_string()));
            map.insert("room_id".to_string(), Value::String(room_id.to_string()));
        }
        global_hub().broadcast(data.clone());
        self.emit(event, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_room() -> MeetingRoom {
        MeetingRoom {
            id: Uuid::new_v4(),
            title: "Weekly sync".to_string(),
            description: None,
            host_id: "host-1".to_string(),
            status: RoomStatus::Scheduled,
            access_model: AccessModel::Free,
            product_id: None,
            max_participants: 10,
            waiting_room_enabled: false,
            join_token: "secret-join-token".to_string(),
            starts_at: None,
            ends_at: None,
            timezone: "UTC".to_string(),
            recurrence_rule: None,
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            started_at: None,
            ended_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn meeting_room_serialization_hides_join_token() {
        let room = test_room();
        let value = serde_json::to_value(&room).expect("room serializes");
        assert!(
            value.get("join_token").is_none(),
            "join_token must never leak in a plain room response (list/get/history/join)"
        );
        assert_eq!(value.get("title").and_then(Value::as_str), Some("Weekly sync"));
    }

    #[test]
    fn room_with_token_serialization_includes_join_token_once() {
        let room = test_room();
        let value = serde_json::to_value(&RoomWithToken::for_host(room))
            .expect("host response serializes");
        assert_eq!(
            value.get("join_token").and_then(Value::as_str),
            Some("secret-join-token"),
            "only the host's create/regenerate response carries the token"
        );
        assert_eq!(value.get("title").and_then(Value::as_str), Some("Weekly sync"));
    }
}
