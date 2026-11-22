// Webinars module — scheduled and recurring live webinars built on
// top of the meetings / video-stream module.
//
// Routes live under `/api/v1/webinars`:
//
//   Webinars:
//     GET/POST / — list webinars / create a webinar.
//     GET/PATCH/DELETE /{id}.
//     POST /{id}/publish, /cancel, /start, /end, /link-meeting, /mute.
//     GET  /{id}/live — live session view (status, attendees, mutes).
//     GET/PATCH /config — configurable defaults (PATCH is admin only).
//   Registration:
//     POST /{id}/register — register (free, paid, waitlist, approval).
//     GET  /{id}/registrations — paginated, filter by status.
//     GET  /{id}/registrations/mine — the caller's own registration.
//     POST /{id}/registrations/{user_id}/approve, /deny, /cancel,
//       /promote.
//   Attendance:
//     POST /{id}/join, /leave.
//     GET  /{id}/attendance — join/leave history (paginated).
//   Live polls:
//     GET/POST /{id}/polls.
//     GET  /{id}/polls/{poll_id} — poll with live results.
//     POST /{id}/polls/{poll_id}/vote, /close.
//     DELETE /{id}/polls/{poll_id}.
//   Live Q&A:
//     GET/POST /{id}/questions.
//     POST /{id}/questions/{question_id}/upvote, /answer.
//     DELETE /{id}/questions/{question_id}.
//   Live chat:
//     GET  /{id}/chat — channel descriptor (messages live in chat).
//     POST /{id}/chat — post a message (emitted as an event).
//   Recordings and replay:
//     GET/POST /{id}/recordings.
//     POST /{id}/recordings/{recording_id}/publish.
//     DELETE /{id}/recordings/{recording_id}.
//     GET  /{id}/replay — replay page data.
//   Follow-up and analytics:
//     POST /{id}/follow-up — recording link + survey invitation.
//     GET  /{id}/analytics — registrations, attendance, drop-off,
//       poll and Q&A participation.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// webinar scheduling (one-off + recurring), host / co-host /
// panelist roles, capacity, registration with approval and waitlist
// (including automatic waitlist promotion), live start/end tied to
// a meetings room, polls with votes and results, Q&A with upvotes
// and answered flags, attendance join/leave tracking, recording
// references, replay page data, post-webinar follow-up events, and
// analytics computed from the stored rows. What is NOT built here:
//   - Video transport. A webinar has NO media pipeline of its own:
//     `link-meeting` attaches an existing meetings room, and `start`
//     asks that room to go live through the `meeting.start_requested`
//     event the meetings module already listens to. Until a media
//     server / SFU exists, the same browser-to-browser WebRTC limit
//     the meetings module documents applies here too.
//   - Payments. A paid webinar only keeps a `product_id` reference
//     and emits `webinar.registration.payment_required` for the
//     billing / store modules; entitlement is checked at join time
//     through `product.entitlement.check`, exactly like meetings.
//     No order, charge, or refund is created here.
//   - Chat persistence. Chat messages are emitted as
//     `webinar.chat.message` for the chat module to store and
//     deliver; this module keeps no second message store. When no
//     chat handler is registered the post endpoint says so
//     (`delivered: false`) instead of pretending the message landed.
//   - Survey delivery. Follow-up emits `surveys.send_requested`
//     (the event the surveys module listens to, with a distribution
//     id) and `notification.requested` per recipient; the surveys
//     and notifications modules own delivery.
//   - Recording bytes. Recordings are metadata references
//     (`storage_target` + `storage_key`) to objects stored through
//     the storage module, exactly like meeting recordings.
//   - Durable rows. The in-memory DashMap store is authoritative
//     until the migration in `migrations/2026-11-22-000077_webinars`
//     is applied and a persistence writer lands; until then
//     webinars do not survive a restart (same standing limit as the
//     meetings module).
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required), exactly
// like the meetings module. Role checks use the optional
// `X-User-Roles` header (comma separated) as an interim source.
//
// Config-driven behaviour (standing architecture rule): nothing
// about capacities, approval, waitlists, feature flags, poll
// limits, or drop-off buckets is hard-coded. Per-webinar fields
// carry the choice; `WebinarConfig` holds the configurable
// defaults, readable at `GET /config` and editable by an admin at
// `PATCH /config`. The defaults below are only the initial seed
// values for that config, clearly marked as such.
//
// Events emitted on the hook bus:
//   - `webinar.created`, `webinar.updated`, `webinar.deleted`,
//     `webinar.published`, `webinar.cancelled`, `webinar.started`,
//     `webinar.ended`
//   - `webinar.meeting.linked`, `webinar.mute_changed`
//   - `webinar.registration.created`, `webinar.registration.approved`,
//     `webinar.registration.denied`, `webinar.registration.cancelled`,
//     `webinar.registration.waitlisted`, `webinar.registration.promoted`,
//     `webinar.registration.payment_required`
//   - `webinar.attendee.joined`, `webinar.attendee.left`
//   - `webinar.poll.created`, `webinar.poll.voted`,
//     `webinar.poll.closed`, `webinar.poll.deleted`
//   - `webinar.question.asked`, `webinar.question.upvoted`,
//     `webinar.question.answered`, `webinar.question.deleted`
//   - `webinar.chat.message`
//   - `webinar.recording.attached`, `webinar.recording.ready`,
//     `webinar.recording.deleted`
//   - `webinar.follow_up.sent`
//   - `notification.requested` (registration, promotion, live,
//     cancellation, reminder, follow-up notices, in the
//     notifications module's payload shape)
//   - `scheduler.enqueue` (start + reminder jobs; see
//     `schedule_webinar`)
//   - `surveys.send_requested` (follow-up survey distribution)
//   - `product.entitlement.check` (paid join, fail-closed)
// Events this module listens to:
//   - `webinar.start_requested` — the scheduler firing a
//     `webinar.start` job takes a scheduled webinar live.
//   - `webinar.reminder_requested` — the scheduler firing a
//     `webinar.reminder` job notifies registered attendees.
//   - `order.paid`, `store.order.completed` — a paid order for the
//     webinar's product marks the matching registration paid.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_TITLE_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 2000;
const MAX_TIMEZONE_LEN: usize = 64;
const MAX_RECURRENCE_LEN: usize = 128;
const MAX_QUESTION_LEN: usize = 500;
const MAX_POLL_OPTION_LEN: usize = 200;
const MAX_CHAT_BODY_LEN: usize = 2000;
const MAX_QA_BODY_LEN: usize = 1000;
const MAX_FOLLOW_UP_MESSAGE_LEN: usize = 2000;
const MAX_PRODUCT_ID_LEN: usize = 128;
const MAX_CHANNEL_ID_LEN: usize = 128;

/// Configurable-default seed values. These are ONLY the initial
/// values loaded into `WebinarConfig`; an admin can change every one
/// of them at `PATCH /api/v1/webinars/config`, and per-webinar
/// fields override them. Nothing below is a hard-coded rule.
const SEED_DEFAULT_CAPACITY: usize = 100;
const SEED_MAX_CAPACITY: usize = 10_000;
const SEED_MIN_CAPACITY: usize = 1;
const SEED_DEFAULT_REQUIRES_APPROVAL: bool = false;
const SEED_DEFAULT_ALLOW_WAITLIST: bool = true;
const SEED_AUTO_PROMOTE_WAITLIST: bool = true;
const SEED_DEFAULT_TIMEZONE: &str = "UTC";
const SEED_REMINDER_LEAD_MINS: u64 = 60;
const SEED_DROP_OFF_BUCKET_MINS: u64 = 5;
const SEED_POLL_MAX_OPTIONS: usize = 10;
const SEED_CHAT_ENABLED_DEFAULT: bool = true;
const SEED_QA_ENABLED_DEFAULT: bool = true;
const SEED_POLLS_ENABLED_DEFAULT: bool = true;
const SEED_RECORDING_ENABLED_DEFAULT: bool = true;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A presenter's role inside one webinar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebinarRole {
    Host,
    CoHost,
    Panelist,
}

impl Default for WebinarRole {
    fn default() -> Self {
        Self::Panelist
    }
}

/// Lifecycle of a webinar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebinarStatus {
    Draft,
    Scheduled,
    Live,
    Ended,
    Cancelled,
}

/// How access to a webinar is sold. `free` needs no product; `paid`
/// requires a `product_id` on the webinar and an entitlement check
/// at join time. The product itself (name, price, billing) lives in
/// the store module; only the reference is kept here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebinarAccessModel {
    Free,
    Paid,
}

impl Default for WebinarAccessModel {
    fn default() -> Self {
        Self::Free
    }
}

/// One webinar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Webinar {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub host_id: String,
    pub co_host_ids: Vec<String>,
    pub panelist_ids: Vec<String>,
    pub status: WebinarStatus,
    pub access_model: WebinarAccessModel,
    /// Store product id that grants access, or `None` for a free
    /// webinar. Entitlement is checked through the
    /// `product.entitlement.check` hook, never by a join from this
    /// module.
    pub product_id: Option<String>,
    pub capacity: usize,
    pub requires_approval: bool,
    pub allow_waitlist: bool,
    pub chat_enabled: bool,
    pub qa_enabled: bool,
    pub polls_enabled: bool,
    pub recording_enabled: bool,
    /// Linked meetings room that carries the live video session, or
    /// `None` until `link-meeting` is called. The webinar never
    /// creates its own video transport.
    pub meeting_room_id: Option<Uuid>,
    /// `unlinked`, `linked`, `live`, or `ended`: the link state of
    /// the meetings room above.
    pub meeting_link_status: String,
    /// Chat channel id in the chat module, or `None` (the channel
    /// then defaults to `webinar:{id}` at post time).
    pub chat_channel_id: Option<String>,
    /// Survey (in the surveys module) attendees are invited to
    /// after the webinar ends, and the distribution id the
    /// follow-up event is sent with. Both are references only.
    pub survey_id: Option<String>,
    pub survey_distribution_id: Option<String>,
    /// Calendar data. Times are stored as UTC; `timezone` is the
    /// IANA name the host scheduled in (format-checked only, never
    /// used to shift the stored UTC times).
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub timezone: String,
    /// Recurrence in the scheduler's own schedule format: an
    /// interval in seconds (`"604800"`) or a cron-like expression
    /// (`"0 9 * * 1"`). Only meaningful together with `starts_at`.
    pub recurrence_rule: Option<String>,
    /// Scheduler linkage for the start job, filled by
    /// `schedule_webinar`: the queue item id when the scheduler
    /// accepted the job, and one of `not_scheduled`, `queued`,
    /// `declined`.
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub reminder_scheduler_status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub follow_up_sent_at: Option<DateTime<Utc>>,
    pub follow_up_count: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One registration. `status` is `registered`, `waitlisted`,
/// `pending_approval`, `cancelled`, or `denied`.
/// `payment_status` is `not_required` (free webinar), `pending`
/// (paid, order not confirmed yet), or `paid`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebinarRegistration {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub user_id: String,
    pub status: String,
    pub payment_status: String,
    pub registered_at: DateTime<Utc>,
    pub approved_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub promoted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One attendance session: one join/leave pair for one attendee.
/// `left_at` is `None` while the person is still in the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebinarAttendance {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub user_id: String,
    pub joined_at: DateTime<Utc>,
    pub left_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Attendance view: one session plus the derived stay length.
#[derive(Debug, Clone, Serialize)]
pub struct AttendanceView {
    pub id: Uuid,
    pub user_id: String,
    pub joined_at: DateTime<Utc>,
    pub left_at: Option<DateTime<Utc>>,
    /// Seconds between join and leave (or join and now, while the
    /// person is still in the session).
    pub duration_secs: i64,
}

/// One live poll. `status` is `open` or `closed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebinarPoll {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub question: String,
    pub options: Vec<String>,
    pub allow_multiple: bool,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

/// One vote. A user holds at most one vote row per poll; voting
/// again replaces the previous choice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebinarPollVote {
    pub id: Uuid,
    pub poll_id: Uuid,
    pub webinar_id: Uuid,
    pub user_id: String,
    pub option_indexes: Vec<usize>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Result tally for one poll option.
#[derive(Debug, Clone, Serialize)]
pub struct PollOptionResult {
    pub option_index: usize,
    pub option: String,
    pub votes: u64,
    /// Share of the voters who picked this option (0.0..=1.0).
    pub share: f64,
}

/// A poll with its live results attached.
#[derive(Debug, Clone, Serialize)]
pub struct PollView {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub question: String,
    pub options: Vec<String>,
    pub allow_multiple: bool,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub total_votes: u64,
    pub unique_voters: u64,
    pub results: Vec<PollOptionResult>,
}

/// One Q&A question. Upvotes are stored as the list of voter ids so
/// the count is derived and a user can upvote only once.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebinarQuestion {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub author_id: String,
    pub body: String,
    pub upvoted_by: Vec<String>,
    pub answered: bool,
    pub answered_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Question view: the stored row plus the derived upvote count.
#[derive(Debug, Clone, Serialize)]
pub struct QuestionView {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub author_id: String,
    pub body: String,
    pub upvotes: u64,
    pub upvoted_by_caller: bool,
    pub answered: bool,
    pub answered_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One recording reference. Only metadata lives here: the media
/// bytes are uploaded through the storage module and referenced by
/// target + key (+ an optional URL). `status` is `processing`,
/// `ready`, or `failed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebinarRecording {
    pub id: Uuid,
    pub webinar_id: Uuid,
    /// The meetings recording this replay was cut from, when the
    /// live session was recorded through the meetings module.
    pub meeting_recording_id: Option<Uuid>,
    pub title: Option<String>,
    pub status: String,
    pub storage_target: Option<String>,
    pub storage_key: Option<String>,
    pub storage_url: Option<String>,
    pub size_bytes: Option<u64>,
    pub duration_secs: Option<u64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Mute state for one user inside one webinar. The media itself is
/// controlled by the meetings session; this record is the webinar's
/// view of it, changed through `POST /{id}/mute` and announced on
/// the hook bus as `webinar.mute_changed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebinarMuteState {
    pub webinar_id: Uuid,
    pub user_id: String,
    pub audio_muted: bool,
    pub video_muted: bool,
    pub updated_at: DateTime<Utc>,
}

/// Configurable defaults for new webinars. Every field is editable
/// by an admin at `PATCH /api/v1/webinars/config`; the `SEED_*`
/// constants above are only the initial values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebinarConfig {
    pub default_capacity: usize,
    pub min_capacity: usize,
    pub max_capacity: usize,
    pub default_requires_approval: bool,
    pub default_allow_waitlist: bool,
    pub auto_promote_waitlist: bool,
    pub default_timezone: String,
    pub reminder_lead_mins: u64,
    pub drop_off_bucket_mins: u64,
    pub poll_max_options: usize,
    pub chat_enabled_default: bool,
    pub qa_enabled_default: bool,
    pub polls_enabled_default: bool,
    pub recording_enabled_default: bool,
}

impl Default for WebinarConfig {
    fn default() -> Self {
        Self {
            default_capacity: SEED_DEFAULT_CAPACITY,
            min_capacity: SEED_MIN_CAPACITY,
            max_capacity: SEED_MAX_CAPACITY,
            default_requires_approval: SEED_DEFAULT_REQUIRES_APPROVAL,
            default_allow_waitlist: SEED_DEFAULT_ALLOW_WAITLIST,
            auto_promote_waitlist: SEED_AUTO_PROMOTE_WAITLIST,
            default_timezone: SEED_DEFAULT_TIMEZONE.to_string(),
            reminder_lead_mins: SEED_REMINDER_LEAD_MINS,
            drop_off_bucket_mins: SEED_DROP_OFF_BUCKET_MINS,
            poll_max_options: SEED_POLL_MAX_OPTIONS,
            chat_enabled_default: SEED_CHAT_ENABLED_DEFAULT,
            qa_enabled_default: SEED_QA_ENABLED_DEFAULT,
            polls_enabled_default: SEED_POLLS_ENABLED_DEFAULT,
            recording_enabled_default: SEED_RECORDING_ENABLED_DEFAULT,
        }
    }
}

/// One drop-off bucket: how many attendees were still in the
/// session during this slice of the live webinar.
#[derive(Debug, Clone, Serialize)]
pub struct DropOffBucket {
    pub bucket_start: DateTime<Utc>,
    pub bucket_end: DateTime<Utc>,
    pub active_count: u64,
}

/// Analytics for one webinar.
#[derive(Debug, Clone, Serialize)]
pub struct WebinarAnalytics {
    pub webinar_id: Uuid,
    pub status: String,
    pub registrations_total: u64,
    pub registered_count: u64,
    pub waitlisted_count: u64,
    pub pending_approval_count: u64,
    pub cancelled_count: u64,
    pub denied_count: u64,
    pub unique_attendees: u64,
    /// Unique attendees / registered seats (0.0 when nothing was
    /// registered).
    pub attendance_rate: f64,
    pub average_watch_secs: f64,
    pub drop_off: Vec<DropOffBucket>,
    pub polls_total: u64,
    pub poll_votes_total: u64,
    pub poll_unique_voters: u64,
    pub poll_participation_rate: f64,
    pub questions_total: u64,
    pub questions_answered: u64,
    pub qa_unique_participants: u64,
    pub qa_participation_rate: f64,
    pub recordings_ready: u64,
    pub follow_up_sent_at: Option<DateTime<Utc>>,
}

/// Live session view: status, linked room, current attendees, mute
/// states, open polls, unanswered questions.
#[derive(Debug, Clone, Serialize)]
pub struct LiveView {
    pub webinar_id: Uuid,
    pub status: String,
    pub meeting_room_id: Option<Uuid>,
    pub meeting_link_status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub attendee_count: u64,
    pub registered_count: u64,
    pub mute_states: Vec<WebinarMuteState>,
    pub open_polls: u64,
    pub unanswered_questions: u64,
}

/// Replay page data: the webinar, its ready recordings, and whether
/// the caller may watch.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayView {
    pub webinar: Webinar,
    pub recordings: Vec<WebinarRecording>,
    pub can_watch: bool,
    pub attended: bool,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateWebinarRequest {
    pub title: String,
    pub description: Option<String>,
    pub co_host_ids: Option<Vec<String>>,
    pub panelist_ids: Option<Vec<String>>,
    pub access_model: Option<WebinarAccessModel>,
    pub product_id: Option<String>,
    pub capacity: Option<usize>,
    pub requires_approval: Option<bool>,
    pub allow_waitlist: Option<bool>,
    pub chat_enabled: Option<bool>,
    pub qa_enabled: Option<bool>,
    pub polls_enabled: Option<bool>,
    pub recording_enabled: Option<bool>,
    pub meeting_room_id: Option<String>,
    pub chat_channel_id: Option<String>,
    pub survey_id: Option<String>,
    pub survey_distribution_id: Option<String>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub timezone: Option<String>,
    pub recurrence_rule: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWebinarRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub co_host_ids: Option<Vec<String>>,
    pub panelist_ids: Option<Vec<String>>,
    pub access_model: Option<WebinarAccessModel>,
    pub product_id: Option<Option<String>>,
    pub capacity: Option<usize>,
    pub requires_approval: Option<bool>,
    pub allow_waitlist: Option<bool>,
    pub chat_enabled: Option<bool>,
    pub qa_enabled: Option<bool>,
    pub polls_enabled: Option<bool>,
    pub recording_enabled: Option<bool>,
    pub meeting_room_id: Option<Option<String>>,
    pub chat_channel_id: Option<Option<String>>,
    pub survey_id: Option<Option<String>>,
    pub survey_distribution_id: Option<Option<String>>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub timezone: Option<String>,
    pub recurrence_rule: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateConfigRequest {
    pub default_capacity: Option<usize>,
    pub min_capacity: Option<usize>,
    pub max_capacity: Option<usize>,
    pub default_requires_approval: Option<bool>,
    pub default_allow_waitlist: Option<bool>,
    pub auto_promote_waitlist: Option<bool>,
    pub default_timezone: Option<String>,
    pub reminder_lead_mins: Option<u64>,
    pub drop_off_bucket_mins: Option<u64>,
    pub poll_max_options: Option<usize>,
    pub chat_enabled_default: Option<bool>,
    pub qa_enabled_default: Option<bool>,
    pub polls_enabled_default: Option<bool>,
    pub recording_enabled_default: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct LinkMeetingRequest {
    pub meeting_room_id: String,
}

#[derive(Debug, Deserialize)]
pub struct MuteRequest {
    pub user_id: Option<String>,
    pub mute_all: Option<bool>,
    pub audio_muted: Option<bool>,
    pub video_muted: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePollRequest {
    pub question: String,
    pub options: Vec<String>,
    pub allow_multiple: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct VotePollRequest {
    pub option_indexes: Vec<usize>,
}

#[derive(Debug, Deserialize)]
pub struct AskQuestionRequest {
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct PostChatRequest {
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct AttachRecordingRequest {
    pub meeting_recording_id: Option<String>,
    pub title: Option<String>,
    pub status: Option<String>,
    pub storage_target: String,
    pub storage_key: String,
    pub storage_url: Option<String>,
    pub size_bytes: Option<u64>,
    pub duration_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct FollowUpRequest {
    pub survey_distribution_id: Option<String>,
    pub include_no_shows: Option<bool>,
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AnalyticsQuery {
    pub bucket_mins: Option<u64>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for webinars. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); handlers tolerate a missing bus
/// by skipping event emission, except the paid entitlement check,
/// which fails closed when there is no bus to ask. Durable rows for
/// these types are defined by migration 000077; until that migration
/// is applied and a persistence writer lands, webinars do not
/// survive a restart.
#[derive(Clone)]
pub struct WebinarsState {
    pub webinars: Arc<DashMap<Uuid, Webinar>>,
    pub registrations: Arc<DashMap<Uuid, WebinarRegistration>>,
    pub attendance: Arc<DashMap<Uuid, WebinarAttendance>>,
    pub polls: Arc<DashMap<Uuid, WebinarPoll>>,
    /// Keyed by `"{poll_id}:{user_id}"`.
    pub votes: Arc<DashMap<String, WebinarPollVote>>,
    pub questions: Arc<DashMap<Uuid, WebinarQuestion>>,
    pub recordings: Arc<DashMap<Uuid, WebinarRecording>>,
    /// Keyed by `"{webinar_id}:{user_id}"`.
    pub mutes: Arc<DashMap<String, WebinarMuteState>>,
    pub config: Arc<RwLock<WebinarConfig>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for WebinarsState {
    fn default() -> Self {
        Self {
            webinars: Arc::new(DashMap::new()),
            registrations: Arc::new(DashMap::new()),
            attendance: Arc::new(DashMap::new()),
            polls: Arc::new(DashMap::new()),
            votes: Arc::new(DashMap::new()),
            questions: Arc::new(DashMap::new()),
            recordings: Arc::new(DashMap::new()),
            mutes: Arc::new(DashMap::new()),
            config: Arc::new(RwLock::new(WebinarConfig::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl WebinarsState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn bus_has(&self, event: &str) -> bool {
        self.bus().is_some_and(|bus| bus.has(event))
    }

    fn config(&self) -> WebinarConfig {
        self.config.read().expect("config lock poisoned").clone()
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape. Delivery is owned by the
    /// notifications module; when no handler is registered the event
    /// is simply not delivered, and this module never pretends
    /// otherwise.
    fn notify(&self, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": "webinar",
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "webinars",
                "category": "webinars",
                "priority": "normal",
                "link": link,
            }),
        );
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

fn is_admin(roles: &[String]) -> bool {
    roles
        .iter()
        .any(|role| role == "admin" || role == "webinars.admin")
}

fn is_host(webinar: &Webinar, user_id: &str) -> bool {
    webinar.host_id == user_id
}

fn is_co_host(webinar: &Webinar, user_id: &str) -> bool {
    webinar.co_host_ids.iter().any(|id| id == user_id)
}

fn is_panelist(webinar: &Webinar, user_id: &str) -> bool {
    webinar.panelist_ids.iter().any(|id| id == user_id)
}

/// Host, co-host, or an admin caller. This is the management
/// capability: update, publish, cancel, start, end, link a room,
/// mute, approve/deny/promote registrations, manage polls and
/// recordings, send follow-up, read analytics.
fn can_manage(webinar: &Webinar, user_id: &str, roles: &[String]) -> bool {
    is_admin(roles) || is_host(webinar, user_id) || is_co_host(webinar, user_id)
}

/// Host, co-host, panelist, or admin: staff who may present (create
/// polls, answer questions) without holding a registration.
fn is_staff(webinar: &Webinar, user_id: &str, roles: &[String]) -> bool {
    can_manage(webinar, user_id, roles) || is_panelist(webinar, user_id)
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

fn require_user_ids(values: &[String], field: &str) -> UnifiedResult<Vec<String>> {
    let mut cleaned: Vec<String> = Vec::new();
    for raw in values {
        let id = require_text(raw, field, 128)?;
        if !cleaned.contains(&id) {
            cleaned.push(id);
        }
    }
    Ok(cleaned)
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

fn load_webinar(state: &WebinarsState, raw_id: &str) -> UnifiedResult<Webinar> {
    let id = parse_uuid(raw_id, "webinar")?;
    state
        .webinars
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Webinar not found: {id}")))
}

fn load_poll(state: &WebinarsState, webinar_id: Uuid, raw_id: &str) -> UnifiedResult<WebinarPoll> {
    let id = parse_uuid(raw_id, "poll")?;
    match state.polls.get(&id) {
        Some(entry) if entry.value().webinar_id == webinar_id => Ok(entry.value().clone()),
        _ => Err(UnifiedError::not_found(format!("Poll not found: {id}"))),
    }
}

fn load_question(
    state: &WebinarsState,
    webinar_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<WebinarQuestion> {
    let id = parse_uuid(raw_id, "question")?;
    match state.questions.get(&id) {
        Some(entry) if entry.value().webinar_id == webinar_id => Ok(entry.value().clone()),
        _ => Err(UnifiedError::not_found(format!("Question not found: {id}"))),
    }
}

fn load_recording(
    state: &WebinarsState,
    webinar_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<WebinarRecording> {
    let id = parse_uuid(raw_id, "recording")?;
    match state.recordings.get(&id) {
        Some(entry) if entry.value().webinar_id == webinar_id => Ok(entry.value().clone()),
        _ => Err(UnifiedError::not_found(format!("Recording not found: {id}"))),
    }
}

/// The caller's registration for one webinar, if any. When several
/// rows exist (a cancelled row plus a fresh one), the newest active
/// row wins; otherwise the newest row of any status.
fn find_registration(
    state: &WebinarsState,
    webinar_id: Uuid,
    user_id: &str,
) -> Option<WebinarRegistration> {
    let mut rows: Vec<WebinarRegistration> = state
        .registrations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|row| row.webinar_id == webinar_id && row.user_id == user_id)
        .collect();
    rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    rows.into_iter()
        .find(|row| matches!(row.status.as_str(), "registered" | "waitlisted" | "pending_approval"))
        .or_else(|| {
            state
                .registrations
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|row| row.webinar_id == webinar_id && row.user_id == user_id)
                .max_by(|a, b| a.created_at.cmp(&b.created_at))
        })
}

/// Registrations that currently hold a seat.
fn registered_count(state: &WebinarsState, webinar_id: Uuid) -> usize {
    state
        .registrations
        .iter()
        .filter(|entry| {
            entry.value().webinar_id == webinar_id && entry.value().status == "registered"
        })
        .count()
}

fn registration_counts(state: &WebinarsState, webinar_id: Uuid) -> (u64, u64, u64, u64, u64, u64) {
    let mut total = 0u64;
    let mut registered = 0u64;
    let mut waitlisted = 0u64;
    let mut pending = 0u64;
    let mut cancelled = 0u64;
    let mut denied = 0u64;
    for entry in state.registrations.iter() {
        let row = entry.value();
        if row.webinar_id != webinar_id {
            continue;
        }
        total += 1;
        match row.status.as_str() {
            "registered" => registered += 1,
            "waitlisted" => waitlisted += 1,
            "pending_approval" => pending += 1,
            "cancelled" => cancelled += 1,
            "denied" => denied += 1,
            _ => {}
        }
    }
    (total, registered, waitlisted, pending, cancelled, denied)
}

/// The caller's open attendance session for one webinar, if any.
fn open_session(
    state: &WebinarsState,
    webinar_id: Uuid,
    user_id: &str,
) -> Option<WebinarAttendance> {
    state
        .attendance
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|row| {
            row.webinar_id == webinar_id && row.user_id == user_id && row.left_at.is_none()
        })
        .max_by(|a, b| a.joined_at.cmp(&b.joined_at))
}

/// A timezone is an IANA name (`"Europe/London"`, `"UTC"`):
/// letters, digits, `_`, `-`, `+`, and `/` only, no empty segments.
/// This checks the shape only; no timezone conversion is done
/// anywhere (all stored times are UTC).
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

/// A recurrence / schedule string, using the scheduler's own rules:
/// an interval in seconds, or five cron-like fields where each field
/// is `*`, `*/N`, or a plain number.
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
            "Invalid timezone: {timezone}. Use an IANA name such as \"Europe/London\" or \"UTC\""
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
                "recurrence_rule requires starts_at: a recurring webinar must have a first occurrence",
            ));
        }
    }
    match (starts_at, ends_at) {
        (Some(start), Some(end)) if end <= start => Err(UnifiedError::bad_request(
            "ends_at must be after starts_at",
        )),
        (None, Some(end)) if end <= Utc::now() => Err(UnifiedError::bad_request(
            "ends_at must be in the future for an instant webinar",
        )),
        (Some(start), None) if start <= Utc::now() => Err(UnifiedError::bad_request(
            "starts_at must be in the future for a scheduled webinar",
        )),
        _ => Ok(()),
    }
}

fn validate_capacity(config: &WebinarConfig, capacity: usize) -> UnifiedResult<()> {
    if capacity < config.min_capacity || capacity > config.max_capacity {
        return Err(UnifiedError::bad_request(format!(
            "capacity must be between {} and {}",
            config.min_capacity, config.max_capacity
        )));
    }
    Ok(())
}

fn status_name(status: WebinarStatus) -> &'static str {
    match status {
        WebinarStatus::Draft => "draft",
        WebinarStatus::Scheduled => "scheduled",
        WebinarStatus::Live => "live",
        WebinarStatus::Ended => "ended",
        WebinarStatus::Cancelled => "cancelled",
    }
}

fn access_name(model: WebinarAccessModel) -> &'static str {
    match model {
        WebinarAccessModel::Free => "free",
        WebinarAccessModel::Paid => "paid",
    }
}

fn poll_view(state: &WebinarsState, poll: &WebinarPoll) -> PollView {
    let mut results: Vec<PollOptionResult> = poll
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| PollOptionResult {
            option_index: index,
            option: option.clone(),
            votes: 0,
            share: 0.0,
        })
        .collect();
    let mut voters: Vec<String> = Vec::new();
    let mut total_votes = 0u64;
    for entry in state.votes.iter() {
        let vote = entry.value();
        if vote.poll_id != poll.id {
            continue;
        }
        if !voters.contains(&vote.user_id) {
            voters.push(vote.user_id.clone());
        }
        for index in &vote.option_indexes {
            if let Some(result) = results.get_mut(*index) {
                result.votes += 1;
                total_votes += 1;
            }
        }
    }
    let unique_voters = voters.len() as u64;
    for result in results.iter_mut() {
        result.share = if unique_voters > 0 {
            result.votes as f64 / unique_voters as f64
        } else {
            0.0
        };
    }
    PollView {
        id: poll.id,
        webinar_id: poll.webinar_id,
        question: poll.question.clone(),
        options: poll.options.clone(),
        allow_multiple: poll.allow_multiple,
        status: poll.status.clone(),
        created_by: poll.created_by.clone(),
        created_at: poll.created_at,
        closed_at: poll.closed_at,
        total_votes,
        unique_voters,
        results,
    }
}

fn question_view(question: &WebinarQuestion, caller: &str) -> QuestionView {
    QuestionView {
        id: question.id,
        webinar_id: question.webinar_id,
        author_id: question.author_id.clone(),
        body: question.body.clone(),
        upvotes: question.upvoted_by.len() as u64,
        upvoted_by_caller: question.upvoted_by.iter().any(|id| id == caller),
        answered: question.answered,
        answered_by: question.answered_by.clone(),
        created_at: question.created_at,
        updated_at: question.updated_at,
    }
}

// ---------------------------------------------------------------------------
// Entitlement (paid webinars)
// ---------------------------------------------------------------------------

/// Check, at join time, whether `user_id` may enter a paid webinar.
///
/// Free webinars (`product_id` is `None`) skip the check entirely.
/// The host, co-hosts, panelists, and callers with the admin
/// capability skip it too. Otherwise the store / product module is
/// asked through the hook event `product.entitlement.check` with
/// payload `{"product_id": ..., "user_id": ...}`, and the first hook
/// result must carry `{"entitled": true}`.
///
/// Fail-closed on purpose: when no handler answers the event (no
/// product module registered yet, or the store has not wired the
/// check), joining a paid webinar answers 503 instead of letting
/// someone in for free. An explicit `{"entitled": false}` (or an
/// `error` in the result) answers 403 / 502.
fn check_entitlement(
    state: &WebinarsState,
    webinar: &Webinar,
    user_id: &str,
    roles: &[String],
) -> UnifiedResult<()> {
    let Some(product_id) = &webinar.product_id else {
        return Ok(());
    };
    if is_staff(webinar, user_id, roles) {
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
            "A paid product entitlement is required to join this webinar",
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

/// Link a scheduled webinar to the scheduler module.
///
/// The scheduler listens to the `scheduler.enqueue` event with the
/// shape `{"kind", "payload"}`. A webinar start is queued as kind
/// `webinar.start` and a reminder as kind `webinar.reminder`, each
/// with the webinar id, start time, and host id in the payload. The
/// result is read back from the first hook result:
/// `{"queued": true, "job_id": ...}` means the scheduler accepted
/// the job.
///
/// CURRENT LIMIT, stated plainly: the scheduler's known-kinds list
/// may not contain a webinar kind yet, so today the scheduler can
/// answer `{"queued": false, "error": "Unknown job kind: ..."}`
/// and this function records `declined` instead of pretending the
/// job was scheduled. The webinar is still created and stays
/// usable; the follow-up is to add `webinar.start` and
/// `webinar.reminder` kinds to the scheduler (which would then emit
/// `webinar.start_requested` / `webinar.reminder_requested`, the
/// events this module already listens to below). Webinars without
/// `starts_at` are never queued and keep `not_scheduled`.
///
/// Migration: `migrations/2026-11-22-000077_webinars` (000071 through
/// 000076 were already taken by other rounds when this module was
/// written; 000074 exists twice and 000075/000076 belong to sibling
/// batch-10 builders).
fn schedule_webinar(state: &WebinarsState, webinar: &mut Webinar) {
    let config = state.config();
    let Some(starts_at) = webinar.starts_at else {
        webinar.scheduler_job_id = None;
        webinar.scheduler_status = "not_scheduled".to_string();
        webinar.reminder_scheduler_status = "not_scheduled".to_string();
        return;
    };
    let Some(bus) = state.bus() else {
        webinar.scheduler_job_id = None;
        webinar.scheduler_status = "declined".to_string();
        webinar.reminder_scheduler_status = "declined".to_string();
        return;
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "webinar.start",
            "payload": {
                "webinar_id": webinar.id.to_string(),
                "starts_at": starts_at.to_rfc3339(),
                "host_id": webinar.host_id,
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            webinar.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            webinar.scheduler_status = "queued".to_string();
        }
        _ => {
            webinar.scheduler_job_id = None;
            webinar.scheduler_status = "declined".to_string();
        }
    }
    let reminder_at = starts_at - chrono::Duration::minutes(config.reminder_lead_mins as i64);
    let reminder = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "webinar.reminder",
            "payload": {
                "webinar_id": webinar.id.to_string(),
                "starts_at": starts_at.to_rfc3339(),
                "remind_at": reminder_at.to_rfc3339(),
                "host_id": webinar.host_id,
            },
        }),
    );
    webinar.reminder_scheduler_status =
        match reminder.results.first() {
            Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
                "queued".to_string()
            }
            _ => "declined".to_string(),
        };
}

/// Promote waitlisted registrations into free seats, earliest
/// registration first. Called after a seat frees up (a registered
/// attendee cancels, or capacity grows). Each promotion is announced
/// with `webinar.registration.promoted` and a notification to the
/// promoted user. Returns the number of promotions made.
fn promote_waitlist(state: &WebinarsState, webinar: &Webinar) -> usize {
    let config = state.config();
    if !config.auto_promote_waitlist {
        return 0;
    }
    let mut promoted = 0usize;
    loop {
        if registered_count(state, webinar.id) >= webinar.capacity {
            break;
        }
        let mut waiting: Vec<WebinarRegistration> = state
            .registrations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.webinar_id == webinar.id && row.status == "waitlisted")
            .collect();
        waiting.sort_by(|a, b| a.registered_at.cmp(&b.registered_at));
        let Some(next) = waiting.into_iter().next() else {
            break;
        };
        let now = Utc::now();
        let mut row = next.clone();
        row.status = "registered".to_string();
        row.promoted_at = Some(now);
        row.updated_at = now;
        state.registrations.insert(row.id, row.clone());
        promoted += 1;
        state.emit(
            "webinar.registration.promoted",
            json!({
                "webinar_id": webinar.id.to_string(),
                "registration_id": row.id.to_string(),
                "user_id": row.user_id,
                "promoted_at": now.to_rfc3339(),
            }),
        );
        state.notify(
            &row.user_id,
            &format!("A seat opened up: {}", webinar.title),
            "You have been promoted from the waitlist and now hold a seat.",
            Some(format!("/api/v1/webinars/{}", webinar.id)),
        );
    }
    promoted
}

/// Close every open attendance session of one webinar at `at`
/// (used when the webinar ends while people are still in it).
fn close_open_sessions(state: &WebinarsState, webinar_id: Uuid, at: DateTime<Utc>) {
    let open: Vec<Uuid> = state
        .attendance
        .iter()
        .filter(|entry| {
            entry.value().webinar_id == webinar_id && entry.value().left_at.is_none()
        })
        .map(|entry| *entry.key())
        .collect();
    for id in open {
        if let Some(mut session) = state.attendance.get_mut(&id) {
            session.left_at = Some(at);
            session.updated_at = at;
        }
    }
}

// ---------------------------------------------------------------------------
// Scheduler / order hook handlers
// ---------------------------------------------------------------------------

/// Answer `webinar.start_requested`: the event the scheduler emits
/// when a `webinar.start` job fires. A scheduled webinar whose start
/// time has arrived goes live; any other state is left alone, and an
/// unknown webinar id is ignored so a stale job can never resurrect
/// a deleted webinar.
fn on_webinar_start_requested(state: WebinarsState, ctx: &mut HookContext) {
    let webinar_id = ctx
        .data
        .get("webinar_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(webinar_id) = webinar_id else {
        ctx.add_result(json!({ "started": false, "error": "webinar.start_requested requires 'webinar_id'" }));
        return;
    };
    let Some(mut entry) = state.webinars.get_mut(&webinar_id) else {
        ctx.add_result(json!({ "started": false, "webinar_id": webinar_id.to_string(), "error": "Webinar not found" }));
        return;
    };
    if entry.status != WebinarStatus::Scheduled {
        ctx.add_result(json!({
            "started": false,
            "webinar_id": webinar_id.to_string(),
            "reason": "Webinar is not scheduled",
        }));
        return;
    }
    entry.status = WebinarStatus::Live;
    entry.started_at = Some(Utc::now());
    entry.updated_at = Utc::now();
    if entry.meeting_room_id.is_some() {
        entry.meeting_link_status = "live".to_string();
    }
    let webinar = entry.clone();
    drop(entry);
    state.emit(
        "webinar.started",
        json!({
            "webinar_id": webinar.id.to_string(),
            "host_id": webinar.host_id,
            "started_at": webinar.started_at,
            "meeting_room_id": webinar.meeting_room_id.map(|id| id.to_string()),
            "source": "scheduler",
        }),
    );
    ctx.add_result(json!({ "started": true, "webinar_id": webinar_id.to_string() }));
}

/// Answer `webinar.reminder_requested`: the event the scheduler
/// emits when a `webinar.reminder` job fires. Every registered
/// attendee gets a `notification.requested` reminder; the result
/// reports how many were notified.
fn on_webinar_reminder_requested(state: WebinarsState, ctx: &mut HookContext) {
    let webinar_id = ctx
        .data
        .get("webinar_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(webinar_id) = webinar_id else {
        ctx.add_result(json!({ "reminded": 0, "error": "webinar.reminder_requested requires 'webinar_id'" }));
        return;
    };
    let Some(webinar) = state
        .webinars
        .get(&webinar_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({ "reminded": 0, "webinar_id": webinar_id.to_string(), "error": "Webinar not found" }));
        return;
    };
    if webinar.status != WebinarStatus::Scheduled && webinar.status != WebinarStatus::Live {
        ctx.add_result(json!({
            "reminded": 0,
            "webinar_id": webinar_id.to_string(),
            "reason": "Webinar is not scheduled or live",
        }));
        return;
    }
    let mut reminded = 0u64;
    for entry in state.registrations.iter() {
        let row = entry.value();
        if row.webinar_id == webinar_id && row.status == "registered" {
            state.notify(
                &row.user_id,
                &format!("Reminder: {}", webinar.title),
                "Your webinar starts soon.",
                Some(format!("/api/v1/webinars/{}", webinar.id)),
            );
            reminded += 1;
        }
    }
    ctx.add_result(json!({ "reminded": reminded, "webinar_id": webinar_id.to_string() }));
}

/// Answer `order.paid` / `store.order.completed`: a paid order for
/// a webinar's product marks the matching registration paid. The
/// payload shape is read defensively (`product_id` plus `user_id`,
/// `buyer_id`, or `customer_id`), because the store module owns the
/// order shape; rows that do not match are ignored.
fn on_order_paid(state: WebinarsState, ctx: &mut HookContext) {
    let product_id = ctx
        .data
        .get("product_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let user_id = ["user_id", "buyer_id", "customer_id"]
        .iter()
        .find_map(|key| ctx.data.get(*key).and_then(|value| value.as_str()))
        .map(str::to_string);
    let (Some(product_id), Some(user_id)) = (product_id, user_id) else {
        ctx.add_result(json!({ "marked_paid": 0, "error": "order event requires 'product_id' and a buyer id" }));
        return;
    };
    let mut marked = 0u64;
    let matches: Vec<Uuid> = state
        .registrations
        .iter()
        .filter(|entry| {
            let row = entry.value();
            row.user_id == user_id
                && row.payment_status == "pending"
                && state
                    .webinars
                    .get(&row.webinar_id)
                    .is_some_and(|w| w.product_id.as_deref() == Some(product_id.as_str()))
        })
        .map(|entry| *entry.key())
        .collect();
    for id in matches {
        if let Some(mut row) = state.registrations.get_mut(&id) {
            row.payment_status = "paid".to_string();
            row.updated_at = Utc::now();
            marked += 1;
        }
    }
    ctx.add_result(json!({ "marked_paid": marked, "user_id": user_id, "product_id": product_id }));
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct WebinarsModule {
    state: WebinarsState,
}

impl WebinarsModule {
    pub fn new() -> Self {
        Self {
            state: WebinarsState::default(),
        }
    }
}

impl Default for WebinarsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for WebinarsModule {
    fn id(&self) -> &'static str {
        "webinars"
    }

    fn name(&self) -> &'static str {
        "Webinars"
    }

    fn description(&self) -> &'static str {
        "Live webinars on top of the meetings video module: scheduled and recurring webinars, host / co-host / panelist roles, registration with approval and waitlist, live polls and Q&A, chat events, attendance tracking, recording references with replay data, post-webinar follow-up, and analytics. Video transport, payments, chat storage, survey delivery, and recording bytes stay with the meetings, billing, chat, surveys, and storage modules."
    }

    /// Webinars is sold as a product: a customer buys a module
    /// entitlement for it (see `crate::modules::licensing`).
    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
    }

    fn requires(&self) -> &[&'static str] {
        &["meetings", "chat", "storage", "billing"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["meetings", "chat", "storage", "billing"]
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
            "webinar.start_requested",
            "webinars",
            Arc::new(move |ctx: &mut HookContext| {
                on_webinar_start_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "webinar.reminder_requested",
            "webinars",
            Arc::new(move |ctx: &mut HookContext| {
                on_webinar_reminder_requested(state.clone(), ctx)
            }),
        );
        for event in ["order.paid", "store.order.completed"] {
            let state = self.state.clone();
            bus.register(
                event,
                "webinars",
                Arc::new(move |ctx: &mut HookContext| on_order_paid(state.clone(), ctx)),
            );
        }
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/webinars")
                .app_data(state)
                // Static segments first: Actix matches routes in
                // registration order, so `/config` must precede
                // `/{id}` or a request for the config would be read
                // as a webinar id.
                .route("/config", web::get().to(get_config))
                .route("/config", web::patch().to(update_config))
                .route("", web::get().to(list_webinars))
                .route("", web::post().to(create_webinar))
                .route("/{id}", web::get().to(get_webinar))
                .route("/{id}", web::patch().to(update_webinar))
                .route("/{id}", web::delete().to(delete_webinar))
                .route("/{id}/publish", web::post().to(publish_webinar))
                .route("/{id}/cancel", web::post().to(cancel_webinar))
                .route("/{id}/start", web::post().to(start_webinar))
                .route("/{id}/end", web::post().to(end_webinar))
                .route("/{id}/link-meeting", web::post().to(link_meeting))
                .route("/{id}/mute", web::post().to(mute_webinar))
                .route("/{id}/live", web::get().to(get_live))
                .route("/{id}/register", web::post().to(register_for_webinar))
                .route("/{id}/registrations", web::get().to(list_registrations))
                .route("/{id}/registrations/mine", web::get().to(my_registration))
                .route(
                    "/{id}/registrations/{user_id}/approve",
                    web::post().to(approve_registration),
                )
                .route(
                    "/{id}/registrations/{user_id}/deny",
                    web::post().to(deny_registration),
                )
                .route(
                    "/{id}/registrations/{user_id}/cancel",
                    web::post().to(cancel_registration),
                )
                .route(
                    "/{id}/registrations/{user_id}/promote",
                    web::post().to(promote_registration),
                )
                .route("/{id}/join", web::post().to(join_webinar))
                .route("/{id}/leave", web::post().to(leave_webinar))
                .route("/{id}/attendance", web::get().to(list_attendance))
                .route("/{id}/polls", web::get().to(list_polls))
                .route("/{id}/polls", web::post().to(create_poll))
                .route("/{id}/polls/{poll_id}", web::get().to(get_poll))
                .route("/{id}/polls/{poll_id}/vote", web::post().to(vote_poll))
                .route("/{id}/polls/{poll_id}/close", web::post().to(close_poll))
                .route("/{id}/polls/{poll_id}", web::delete().to(delete_poll))
                .route("/{id}/questions", web::get().to(list_questions))
                .route("/{id}/questions", web::post().to(ask_question))
                .route(
                    "/{id}/questions/{question_id}/upvote",
                    web::post().to(upvote_question),
                )
                .route(
                    "/{id}/questions/{question_id}/answer",
                    web::post().to(answer_question),
                )
                .route(
                    "/{id}/questions/{question_id}",
                    web::delete().to(delete_question),
                )
                .route("/{id}/chat", web::get().to(get_chat_channel))
                .route("/{id}/chat", web::post().to(post_chat_message))
                .route("/{id}/recordings", web::get().to(list_recordings))
                .route("/{id}/recordings", web::post().to(attach_recording))
                .route(
                    "/{id}/recordings/{recording_id}/publish",
                    web::post().to(publish_recording),
                )
                .route(
                    "/{id}/recordings/{recording_id}",
                    web::delete().to(delete_recording),
                )
                .route("/{id}/replay", web::get().to(get_replay))
                .route("/{id}/follow-up", web::post().to(send_follow_up))
                .route("/{id}/analytics", web::get().to(get_analytics)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/webinars/config", owner: "webinars", description: "Read the configurable webinar defaults." },
            RouteInfo { method: "PATCH", path: "/api/v1/webinars/config", owner: "webinars", description: "Update the configurable webinar defaults (admin only)." },
            RouteInfo { method: "GET", path: "/api/v1/webinars", owner: "webinars", description: "List webinars (paginated, filter by status, host_id, access_model, from, to; search title and description)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars", owner: "webinars", description: "Create a webinar (draft, or scheduled when starts_at is in the future)." },
            RouteInfo { method: "GET", path: "/api/v1/webinars/{id}", owner: "webinars", description: "Get one webinar." },
            RouteInfo { method: "PATCH", path: "/api/v1/webinars/{id}", owner: "webinars", description: "Update a webinar (host or co-host; ended or cancelled webinars cannot be updated)." },
            RouteInfo { method: "DELETE", path: "/api/v1/webinars/{id}", owner: "webinars", description: "Delete a webinar and its registrations, polls, questions, attendance, and recordings (host only, never while live)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/publish", owner: "webinars", description: "Publish a draft webinar: it becomes scheduled and start/reminder jobs are queued (host or co-host)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/cancel", owner: "webinars", description: "Cancel a draft or scheduled webinar and notify its registrants (host or co-host)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/start", owner: "webinars", description: "Start a scheduled webinar live; asks the linked meetings room to go live too (host or co-host)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/end", owner: "webinars", description: "End a live webinar and close open attendance sessions (host or co-host)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/link-meeting", owner: "webinars", description: "Link an existing meetings room as the webinar's live video session (host or co-host)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/mute", owner: "webinars", description: "Set audio/video mute state for one attendee or everyone (host or co-host; announced as webinar.mute_changed)." },
            RouteInfo { method: "GET", path: "/api/v1/webinars/{id}/live", owner: "webinars", description: "Live session view: status, linked room, current attendees, mute states, open polls, unanswered questions." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/register", owner: "webinars", description: "Register for a webinar (free, paid via product entitlement, waitlist when full, pending approval when configured)." },
            RouteInfo { method: "GET", path: "/api/v1/webinars/{id}/registrations", owner: "webinars", description: "List registrations (host or co-host; paginated, filter by status, user_id)." },
            RouteInfo { method: "GET", path: "/api/v1/webinars/{id}/registrations/mine", owner: "webinars", description: "Get the caller's own registration for a webinar." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/registrations/{user_id}/approve", owner: "webinars", description: "Approve a pending registration (host or co-host; takes a seat or moves to the waitlist when full)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/registrations/{user_id}/deny", owner: "webinars", description: "Deny a pending registration (host or co-host)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/registrations/{user_id}/cancel", owner: "webinars", description: "Cancel a registration (the registrant or host/co-host; frees the seat and auto-promotes the waitlist)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/registrations/{user_id}/promote", owner: "webinars", description: "Promote a waitlisted registration into a free seat (host or co-host)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/join", owner: "webinars", description: "Join the live session (registered attendee or staff; paid entitlement checked, attendance recorded)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/leave", owner: "webinars", description: "Leave the live session (closes the attendance session)." },
            RouteInfo { method: "GET", path: "/api/v1/webinars/{id}/attendance", owner: "webinars", description: "Attendance of a webinar: join/leave sessions with durations (host or co-host; paginated, filter by user_id)." },
            RouteInfo { method: "GET", path: "/api/v1/webinars/{id}/polls", owner: "webinars", description: "List polls with live results (registered attendee or staff; paginated, filter by status)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/polls", owner: "webinars", description: "Create a live poll (host, co-host, or panelist)." },
            RouteInfo { method: "GET", path: "/api/v1/webinars/{id}/polls/{poll_id}", owner: "webinars", description: "Get one poll with live results (registered attendee or staff)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/polls/{poll_id}/vote", owner: "webinars", description: "Vote in an open poll (registered attendee or staff; voting again replaces the previous vote)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/polls/{poll_id}/close", owner: "webinars", description: "Close a poll (host or co-host)." },
            RouteInfo { method: "DELETE", path: "/api/v1/webinars/{id}/polls/{poll_id}", owner: "webinars", description: "Delete a poll and its votes (host only)." },
            RouteInfo { method: "GET", path: "/api/v1/webinars/{id}/questions", owner: "webinars", description: "List Q&A questions, most upvoted first (registered attendee or staff; paginated, filter by answered)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/questions", owner: "webinars", description: "Ask a question during the live session (registered attendee or staff)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/questions/{question_id}/upvote", owner: "webinars", description: "Upvote a question (registered attendee or staff; one upvote per user, repeat upvotes are a no-op)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/questions/{question_id}/answer", owner: "webinars", description: "Mark a question answered (host, co-host, or panelist)." },
            RouteInfo { method: "DELETE", path: "/api/v1/webinars/{id}/questions/{question_id}", owner: "webinars", description: "Delete a question (host or its author)." },
            RouteInfo { method: "GET", path: "/api/v1/webinars/{id}/chat", owner: "webinars", description: "Chat channel descriptor for a webinar (messages live in the chat module)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/chat", owner: "webinars", description: "Post a live chat message (registered attendee or staff; emitted as webinar.chat.message for the chat module)." },
            RouteInfo { method: "GET", path: "/api/v1/webinars/{id}/recordings", owner: "webinars", description: "List recording references of a webinar (paginated, filter by status)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/recordings", owner: "webinars", description: "Attach a recording reference (stored object in the storage module) to a webinar (host or co-host)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/recordings/{recording_id}/publish", owner: "webinars", description: "Mark a recording ready for replay (host or co-host)." },
            RouteInfo { method: "DELETE", path: "/api/v1/webinars/{id}/recordings/{recording_id}", owner: "webinars", description: "Delete a recording reference (host only)." },
            RouteInfo { method: "GET", path: "/api/v1/webinars/{id}/replay", owner: "webinars", description: "Replay page data: webinar plus its ready recordings (ended webinars; host, staff, or registered attendees)." },
            RouteInfo { method: "POST", path: "/api/v1/webinars/{id}/follow-up", owner: "webinars", description: "Send post-webinar follow-up: recording link notifications plus a survey invitation (host or co-host; ended webinars only)." },
            RouteInfo { method: "GET", path: "/api/v1/webinars/{id}/analytics", owner: "webinars", description: "Analytics for a webinar: registrations, attendance rate, drop-off buckets, poll and Q&A participation (host or co-host)." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(WebinarsModule::new()));
}

// ---------------------------------------------------------------------------
// Config handlers
// ---------------------------------------------------------------------------

async fn get_config(state: web::Data<WebinarsState>, req: HttpRequest) -> HttpResponse {
    match actor_id(&req) {
        Ok(_) => ok(state.config()),
        Err(error) => err(error),
    }
}

async fn update_config(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    body: web::Json<UpdateConfigRequest>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarConfig> = (|| {
        actor_id(&req)?;
        let roles = caller_roles(&req);
        if !is_admin(&roles) {
            return Err(UnifiedError::forbidden(
                "Only an admin can change the webinar defaults",
            ));
        }
        let input = body.into_inner();
        let mut config = state.config();
        if let Some(value) = input.default_capacity {
            config.default_capacity = value;
        }
        if let Some(value) = input.min_capacity {
            config.min_capacity = value;
        }
        if let Some(value) = input.max_capacity {
            config.max_capacity = value;
        }
        if config.min_capacity < 1 {
            return Err(UnifiedError::bad_request("min_capacity must be at least 1"));
        }
        if config.max_capacity < config.min_capacity {
            return Err(UnifiedError::bad_request(
                "max_capacity must not be below min_capacity",
            ));
        }
        if config.default_capacity < config.min_capacity
            || config.default_capacity > config.max_capacity
        {
            return Err(UnifiedError::bad_request(
                "default_capacity must be between min_capacity and max_capacity",
            ));
        }
        if let Some(value) = input.default_requires_approval {
            config.default_requires_approval = value;
        }
        if let Some(value) = input.default_allow_waitlist {
            config.default_allow_waitlist = value;
        }
        if let Some(value) = input.auto_promote_waitlist {
            config.auto_promote_waitlist = value;
        }
        if let Some(raw) = &input.default_timezone {
            let timezone = require_text(raw, "default_timezone", MAX_TIMEZONE_LEN)?;
            if !valid_timezone(&timezone) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid default_timezone: {timezone}. Use an IANA name such as \"Europe/London\" or \"UTC\""
                )));
            }
            config.default_timezone = timezone;
        }
        if let Some(value) = input.reminder_lead_mins {
            config.reminder_lead_mins = value;
        }
        if let Some(value) = input.drop_off_bucket_mins {
            if value == 0 {
                return Err(UnifiedError::bad_request(
                    "drop_off_bucket_mins must be at least 1",
                ));
            }
            config.drop_off_bucket_mins = value;
        }
        if let Some(value) = input.poll_max_options {
            if value < 2 {
                return Err(UnifiedError::bad_request(
                    "poll_max_options must be at least 2",
                ));
            }
            config.poll_max_options = value;
        }
        if let Some(value) = input.chat_enabled_default {
            config.chat_enabled_default = value;
        }
        if let Some(value) = input.qa_enabled_default {
            config.qa_enabled_default = value;
        }
        if let Some(value) = input.polls_enabled_default {
            config.polls_enabled_default = value;
        }
        if let Some(value) = input.recording_enabled_default {
            config.recording_enabled_default = value;
        }
        *state
            .config
            .write()
            .expect("config lock poisoned") = config.clone();
        state.emit(
            "webinar.config.updated",
            json!({
                "default_capacity": config.default_capacity,
                "min_capacity": config.min_capacity,
                "max_capacity": config.max_capacity,
                "default_requires_approval": config.default_requires_approval,
                "default_allow_waitlist": config.default_allow_waitlist,
                "auto_promote_waitlist": config.auto_promote_waitlist,
                "default_timezone": config.default_timezone,
                "reminder_lead_mins": config.reminder_lead_mins,
                "drop_off_bucket_mins": config.drop_off_bucket_mins,
                "poll_max_options": config.poll_max_options,
            }),
        );
        Ok(config)
    })();
    match result {
        Ok(config) => ok(config),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Webinar handlers
// ---------------------------------------------------------------------------

async fn list_webinars(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = actor_id(&req) {
        return err(error);
    }
    let params = query.into_inner();
    let status_filter = params.filters.get("status").cloned();
    let host_filter = params.filters.get("host_id").cloned();
    let model_filter = params.filters.get("access_model").cloned();
    let from_filter = params.filters.get("from").cloned();
    let to_filter = params.filters.get("to").cloned();
    let search = params.search.clone().unwrap_or_default().to_lowercase();
    let mut items: Vec<Webinar> = state
        .webinars
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|webinar| {
            if let Some(status) = &status_filter {
                let expected = match status.as_str() {
                    "draft" => WebinarStatus::Draft,
                    "scheduled" => WebinarStatus::Scheduled,
                    "live" => WebinarStatus::Live,
                    "ended" => WebinarStatus::Ended,
                    "cancelled" => WebinarStatus::Cancelled,
                    _ => return false,
                };
                if webinar.status != expected {
                    return false;
                }
            }
            if let Some(host_id) = &host_filter {
                if &webinar.host_id != host_id {
                    return false;
                }
            }
            if let Some(model) = &model_filter {
                let expected = match model.as_str() {
                    "free" => WebinarAccessModel::Free,
                    "paid" => WebinarAccessModel::Paid,
                    _ => return false,
                };
                if webinar.access_model != expected {
                    return false;
                }
            }
            if let Some(from) = &from_filter {
                if let Ok(from) = from.parse::<DateTime<Utc>>() {
                    if webinar.starts_at.is_none_or(|start| start < from) {
                        return false;
                    }
                }
            }
            if let Some(to) = &to_filter {
                if let Ok(to) = to.parse::<DateTime<Utc>>() {
                    if webinar.starts_at.is_none_or(|start| start > to) {
                        return false;
                    }
                }
            }
            if !search.is_empty() {
                let haystack = format!(
                    "{} {}",
                    webinar.title,
                    webinar.description.clone().unwrap_or_default()
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

async fn create_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    body: web::Json<CreateWebinarRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Webinar> = (|| {
        let actor = actor_id(&req)?;
        let input = body.into_inner();
        let config = state.config();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let co_host_ids = match &input.co_host_ids {
            Some(ids) => require_user_ids(ids, "co_host_ids")?,
            None => Vec::new(),
        };
        let panelist_ids = match &input.panelist_ids {
            Some(ids) => require_user_ids(ids, "panelist_ids")?,
            None => Vec::new(),
        };
        if co_host_ids.contains(&actor) || panelist_ids.contains(&actor) {
            return Err(UnifiedError::bad_request(
                "The host cannot also be listed as a co-host or panelist",
            ));
        }
        for id in &co_host_ids {
            if panelist_ids.contains(id) {
                return Err(UnifiedError::bad_request(
                    "A user cannot be both a co-host and a panelist",
                ));
            }
        }
        let access_model = input.access_model.unwrap_or_default();
        let product_id = match &input.product_id {
            Some(raw) => Some(require_text(raw, "product_id", MAX_PRODUCT_ID_LEN)?),
            None => None,
        };
        if access_model != WebinarAccessModel::Free && product_id.is_none() {
            return Err(UnifiedError::bad_request(
                "A paid webinar requires product_id",
            ));
        }
        if access_model == WebinarAccessModel::Free && product_id.is_some() {
            return Err(UnifiedError::bad_request(
                "A free webinar must not carry product_id; use access_model paid for a paid webinar",
            ));
        }
        let capacity = input.capacity.unwrap_or(config.default_capacity);
        validate_capacity(&config, capacity)?;
        let timezone = match &input.timezone {
            Some(raw) => require_text(raw, "timezone", MAX_TIMEZONE_LEN)?,
            None => config.default_timezone.clone(),
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
        let meeting_room_id = match &input.meeting_room_id {
            Some(raw) => Some(parse_uuid(raw, "meeting_room")?),
            None => None,
        };
        let chat_channel_id = match &input.chat_channel_id {
            Some(raw) => Some(require_text(raw, "chat_channel_id", MAX_CHANNEL_ID_LEN)?),
            None => None,
        };
        let survey_id = match &input.survey_id {
            Some(raw) => Some(require_text(raw, "survey_id", MAX_PRODUCT_ID_LEN)?),
            None => None,
        };
        let survey_distribution_id = match &input.survey_distribution_id {
            Some(raw) => Some(require_text(
                raw,
                "survey_distribution_id",
                MAX_PRODUCT_ID_LEN,
            )?),
            None => None,
        };
        let now = Utc::now();
        // A future start stays scheduled (draft when no start is
        // set yet); an instant webinar (no future start) is live
        // immediately, like an instant meeting room.
        let (status, started_at) = match input.starts_at {
            Some(start) if start > now => (WebinarStatus::Scheduled, None),
            Some(_) => (WebinarStatus::Live, Some(now)),
            None => (WebinarStatus::Draft, None),
        };
        let mut webinar = Webinar {
            id: Uuid::now_v7(),
            title,
            description,
            host_id: actor.clone(),
            co_host_ids,
            panelist_ids,
            status,
            access_model,
            product_id,
            capacity,
            requires_approval: input
                .requires_approval
                .unwrap_or(config.default_requires_approval),
            allow_waitlist: input
                .allow_waitlist
                .unwrap_or(config.default_allow_waitlist),
            chat_enabled: input.chat_enabled.unwrap_or(config.chat_enabled_default),
            qa_enabled: input.qa_enabled.unwrap_or(config.qa_enabled_default),
            polls_enabled: input
                .polls_enabled
                .unwrap_or(config.polls_enabled_default),
            recording_enabled: input
                .recording_enabled
                .unwrap_or(config.recording_enabled_default),
            meeting_room_id,
            meeting_link_status: if meeting_room_id.is_some() {
                "linked".to_string()
            } else {
                "unlinked".to_string()
            },
            chat_channel_id,
            survey_id,
            survey_distribution_id,
            starts_at: input.starts_at,
            ends_at: input.ends_at,
            timezone,
            recurrence_rule,
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            reminder_scheduler_status: "not_scheduled".to_string(),
            started_at,
            ended_at: None,
            follow_up_sent_at: None,
            follow_up_count: 0,
            created_at: now,
            updated_at: now,
        };
        if webinar.status == WebinarStatus::Scheduled {
            schedule_webinar(&state, &mut webinar);
        }
        state.webinars.insert(webinar.id, webinar.clone());
        state.emit(
            "webinar.created",
            json!({
                "webinar_id": webinar.id.to_string(),
                "host_id": actor,
                "status": status_name(status),
                "access_model": access_name(webinar.access_model),
                "product_id": webinar.product_id,
                "capacity": webinar.capacity,
                "requires_approval": webinar.requires_approval,
                "allow_waitlist": webinar.allow_waitlist,
                "meeting_room_id": webinar.meeting_room_id.map(|id| id.to_string()),
                "starts_at": webinar.starts_at,
                "ends_at": webinar.ends_at,
                "timezone": webinar.timezone,
                "recurrence_rule": webinar.recurrence_rule,
                "scheduler_job_id": webinar.scheduler_job_id,
                "scheduler_status": webinar.scheduler_status,
                "reminder_scheduler_status": webinar.reminder_scheduler_status,
            }),
        );
        Ok(webinar)
    })();
    match result {
        Ok(webinar) => created(webinar),
        Err(error) => err(error),
    }
}

async fn get_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(error) = actor_id(&req) {
        return err(error);
    }
    match load_webinar(&state, &path.into_inner()) {
        Ok(webinar) => ok(webinar),
        Err(error) => err(error),
    }
}

async fn update_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateWebinarRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Webinar> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let mut webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can update the webinar",
            ));
        }
        if webinar.status == WebinarStatus::Ended || webinar.status == WebinarStatus::Cancelled {
            return Err(UnifiedError::unprocessable(
                "An ended or cancelled webinar cannot be updated",
            ));
        }
        // While live, only the description and the live feature
        // flags may change; scheduling, capacity, and pricing are
        // frozen once the session has started.
        let live = webinar.status == WebinarStatus::Live;
        if let Some(title) = &input.title {
            if live {
                return Err(UnifiedError::unprocessable(
                    "The title cannot change while the webinar is live",
                ));
            }
            webinar.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            webinar.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(ids) = &input.co_host_ids {
            if live {
                return Err(UnifiedError::unprocessable(
                    "Co-hosts cannot change while the webinar is live",
                ));
            }
            let cleaned = require_user_ids(ids, "co_host_ids")?;
            if cleaned.contains(&webinar.host_id) {
                return Err(UnifiedError::bad_request(
                    "The host cannot also be listed as a co-host",
                ));
            }
            webinar.co_host_ids = cleaned;
        }
        if let Some(ids) = &input.panelist_ids {
            if live {
                return Err(UnifiedError::unprocessable(
                    "Panelists cannot change while the webinar is live",
                ));
            }
            let cleaned = require_user_ids(ids, "panelist_ids")?;
            if cleaned.contains(&webinar.host_id)
                || webinar.co_host_ids.iter().any(|id| cleaned.contains(id))
            {
                return Err(UnifiedError::bad_request(
                    "A user cannot hold two presenter roles at once",
                ));
            }
            webinar.panelist_ids = cleaned;
        }
        if let Some(model) = input.access_model {
            if live {
                return Err(UnifiedError::unprocessable(
                    "The access model cannot change while the webinar is live",
                ));
            }
            webinar.access_model = model;
        }
        if let Some(product_id) = &input.product_id {
            if live {
                return Err(UnifiedError::unprocessable(
                    "The product cannot change while the webinar is live",
                ));
            }
            webinar.product_id = match product_id {
                Some(raw) => Some(require_text(raw, "product_id", MAX_PRODUCT_ID_LEN)?),
                None => None,
            };
        }
        if webinar.access_model != WebinarAccessModel::Free && webinar.product_id.is_none() {
            return Err(UnifiedError::bad_request(
                "A paid webinar requires product_id",
            ));
        }
        if webinar.access_model == WebinarAccessModel::Free && webinar.product_id.is_some() {
            return Err(UnifiedError::bad_request(
                "A free webinar must not carry product_id; use access_model paid for a paid webinar",
            ));
        }
        if let Some(capacity) = input.capacity {
            if live {
                return Err(UnifiedError::unprocessable(
                    "Capacity cannot change while the webinar is live",
                ));
            }
            let config = state.config();
            validate_capacity(&config, capacity)?;
            if capacity < registered_count(&state, webinar.id) {
                return Err(UnifiedError::bad_request(
                    "Capacity cannot drop below the number of already registered seats",
                ));
            }
            webinar.capacity = capacity;
        }
        if let Some(value) = input.requires_approval {
            webinar.requires_approval = value;
        }
        if let Some(value) = input.allow_waitlist {
            webinar.allow_waitlist = value;
        }
        if let Some(value) = input.chat_enabled {
            webinar.chat_enabled = value;
        }
        if let Some(value) = input.qa_enabled {
            webinar.qa_enabled = value;
        }
        if let Some(value) = input.polls_enabled {
            webinar.polls_enabled = value;
        }
        if let Some(value) = input.recording_enabled {
            webinar.recording_enabled = value;
        }
        if let Some(room) = &input.meeting_room_id {
            if live {
                return Err(UnifiedError::unprocessable(
                    "The linked meetings room cannot change while the webinar is live",
                ));
            }
            webinar.meeting_room_id = match room {
                Some(raw) => Some(parse_uuid(raw, "meeting_room")?),
                None => None,
            };
            webinar.meeting_link_status = if webinar.meeting_room_id.is_some() {
                "linked".to_string()
            } else {
                "unlinked".to_string()
            };
        }
        if let Some(channel) = &input.chat_channel_id {
            webinar.chat_channel_id = match channel {
                Some(raw) => Some(require_text(raw, "chat_channel_id", MAX_CHANNEL_ID_LEN)?),
                None => None,
            };
        }
        if let Some(survey) = &input.survey_id {
            webinar.survey_id = match survey {
                Some(raw) => Some(require_text(raw, "survey_id", MAX_PRODUCT_ID_LEN)?),
                None => None,
            };
        }
        if let Some(distribution) = &input.survey_distribution_id {
            webinar.survey_distribution_id = match distribution {
                Some(raw) => Some(require_text(
                    raw,
                    "survey_distribution_id",
                    MAX_PRODUCT_ID_LEN,
                )?),
                None => None,
            };
        }
        let mut starts_changed = false;
        if let Some(starts_at) = input.starts_at {
            if live {
                return Err(UnifiedError::unprocessable(
                    "The start time cannot change while the webinar is live",
                ));
            }
            webinar.starts_at = Some(starts_at);
            starts_changed = true;
        }
        if let Some(ends_at) = input.ends_at {
            webinar.ends_at = Some(ends_at);
        }
        if let Some(timezone) = &input.timezone {
            if live {
                return Err(UnifiedError::unprocessable(
                    "The timezone cannot change while the webinar is live",
                ));
            }
            webinar.timezone = require_text(timezone, "timezone", MAX_TIMEZONE_LEN)?;
        }
        if let Some(rule) = &input.recurrence_rule {
            if live {
                return Err(UnifiedError::unprocessable(
                    "The recurrence rule cannot change while the webinar is live",
                ));
            }
            webinar.recurrence_rule = match rule {
                Some(raw) => Some(require_text(raw, "recurrence_rule", MAX_RECURRENCE_LEN)?),
                None => None,
            };
        }
        validate_calendar(
            webinar.starts_at,
            webinar.ends_at,
            &webinar.timezone,
            webinar.recurrence_rule.as_deref(),
        )?;
        if starts_changed && webinar.status == WebinarStatus::Scheduled {
            schedule_webinar(&state, &mut webinar);
        }
        webinar.updated_at = Utc::now();
        state.webinars.insert(webinar.id, webinar.clone());
        // A capacity increase can free seats for the waitlist.
        promote_waitlist(&state, &webinar);
        state.emit(
            "webinar.updated",
            json!({
                "webinar_id": webinar.id.to_string(),
                "status": status_name(webinar.status),
                "capacity": webinar.capacity,
                "starts_at": webinar.starts_at,
                "ends_at": webinar.ends_at,
                "timezone": webinar.timezone,
                "recurrence_rule": webinar.recurrence_rule,
                "scheduler_job_id": webinar.scheduler_job_id,
                "scheduler_status": webinar.scheduler_status,
            }),
        );
        Ok(webinar)
    })();
    match result {
        Ok(webinar) => ok(webinar),
        Err(error) => err(error),
    }
}

async fn delete_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let webinar = load_webinar(&state, &path.into_inner())?;
        if !is_host(&webinar, &actor) && !is_admin(&roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host or an admin can delete the webinar",
            ));
        }
        if webinar.status == WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "A live webinar cannot be deleted; end it first",
            ));
        }
        let id = webinar.id;
        state.webinars.remove(&id);
        // Typed sweeps: remove every row that belongs to this
        // webinar, one store at a time.
        let reg_keys: Vec<Uuid> = state
            .registrations
            .iter()
            .filter(|entry| entry.value().webinar_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in reg_keys {
            state.registrations.remove(&key);
        }
        let att_keys: Vec<Uuid> = state
            .attendance
            .iter()
            .filter(|entry| entry.value().webinar_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in att_keys {
            state.attendance.remove(&key);
        }
        let poll_keys: Vec<Uuid> = state
            .polls
            .iter()
            .filter(|entry| entry.value().webinar_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in &poll_keys {
            state.polls.remove(key);
        }
        let vote_keys: Vec<String> = state
            .votes
            .iter()
            .filter(|entry| entry.value().webinar_id == id)
            .map(|entry| entry.key().clone())
            .collect();
        for key in vote_keys {
            state.votes.remove(&key);
        }
        let q_keys: Vec<Uuid> = state
            .questions
            .iter()
            .filter(|entry| entry.value().webinar_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in q_keys {
            state.questions.remove(&key);
        }
        let rec_keys: Vec<Uuid> = state
            .recordings
            .iter()
            .filter(|entry| entry.value().webinar_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in rec_keys {
            state.recordings.remove(&key);
        }
        let mute_keys: Vec<String> = state
            .mutes
            .iter()
            .filter(|entry| entry.value().webinar_id == id)
            .map(|entry| entry.key().clone())
            .collect();
        for key in mute_keys {
            state.mutes.remove(&key);
        }
        state.emit(
            "webinar.deleted",
            json!({ "webinar_id": id.to_string(), "host_id": webinar.host_id }),
        );
        Ok(json!({ "deleted": true, "webinar_id": id.to_string() }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error),
    }
}

async fn publish_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Webinar> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let mut webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can publish the webinar",
            ));
        }
        if webinar.status != WebinarStatus::Draft {
            return Err(UnifiedError::unprocessable(
                "Only a draft webinar can be published",
            ));
        }
        let Some(starts_at) = webinar.starts_at else {
            return Err(UnifiedError::bad_request(
                "A webinar needs starts_at before it can be published",
            ));
        };
        if starts_at <= Utc::now() {
            return Err(UnifiedError::bad_request(
                "starts_at must be in the future for a scheduled webinar",
            ));
        }
        webinar.status = WebinarStatus::Scheduled;
        webinar.updated_at = Utc::now();
        schedule_webinar(&state, &mut webinar);
        state.webinars.insert(webinar.id, webinar.clone());
        state.emit(
            "webinar.published",
            json!({
                "webinar_id": webinar.id.to_string(),
                "host_id": webinar.host_id,
                "starts_at": webinar.starts_at,
                "scheduler_job_id": webinar.scheduler_job_id,
                "scheduler_status": webinar.scheduler_status,
                "reminder_scheduler_status": webinar.reminder_scheduler_status,
            }),
        );
        Ok(webinar)
    })();
    match result {
        Ok(webinar) => ok(webinar),
        Err(error) => err(error),
    }
}

async fn cancel_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Webinar> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let mut webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can cancel the webinar",
            ));
        }
        if webinar.status != WebinarStatus::Draft && webinar.status != WebinarStatus::Scheduled {
            return Err(UnifiedError::unprocessable(
                "Only a draft or scheduled webinar can be cancelled; end a live webinar instead",
            ));
        }
        webinar.status = WebinarStatus::Cancelled;
        webinar.updated_at = Utc::now();
        state.webinars.insert(webinar.id, webinar.clone());
        for entry in state.registrations.iter() {
            let row = entry.value();
            if row.webinar_id == webinar.id && row.status == "registered" {
                state.notify(
                    &row.user_id,
                    &format!("Cancelled: {}", webinar.title),
                    "This webinar has been cancelled.",
                    Some(format!("/api/v1/webinars/{}", webinar.id)),
                );
            }
        }
        state.emit(
            "webinar.cancelled",
            json!({
                "webinar_id": webinar.id.to_string(),
                "host_id": webinar.host_id,
            }),
        );
        Ok(webinar)
    })();
    match result {
        Ok(webinar) => ok(webinar),
        Err(error) => err(error),
    }
}

async fn start_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Webinar> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let mut webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can start the webinar",
            ));
        }
        if webinar.status != WebinarStatus::Scheduled {
            return Err(UnifiedError::unprocessable(
                "Only a scheduled webinar can be started; publish a draft first",
            ));
        }
        let now = Utc::now();
        webinar.status = WebinarStatus::Live;
        webinar.started_at = Some(now);
        webinar.updated_at = now;
        if webinar.meeting_room_id.is_some() {
            webinar.meeting_link_status = "live".to_string();
            // Ask the linked meetings room to go live as well. The
            // meetings module listens to `meeting.start_requested`
            // and takes its own room live; this webinar does not
            // manage the room's state directly.
            state.emit(
                "meeting.start_requested",
                json!({
                    "room_id": webinar.meeting_room_id.map(|id| id.to_string()),
                    "meeting_id": webinar.meeting_room_id.map(|id| id.to_string()),
                    "webinar_id": webinar.id.to_string(),
                }),
            );
        }
        state.webinars.insert(webinar.id, webinar.clone());
        for entry in state.registrations.iter() {
            let row = entry.value();
            if row.webinar_id == webinar.id && row.status == "registered" {
                state.notify(
                    &row.user_id,
                    &format!("Live now: {}", webinar.title),
                    "Your webinar has started.",
                    Some(format!("/api/v1/webinars/{}", webinar.id)),
                );
            }
        }
        state.emit(
            "webinar.started",
            json!({
                "webinar_id": webinar.id.to_string(),
                "host_id": webinar.host_id,
                "started_at": now.to_rfc3339(),
                "meeting_room_id": webinar.meeting_room_id.map(|id| id.to_string()),
                "meeting_link_status": webinar.meeting_link_status,
                "source": "manual",
            }),
        );
        Ok(webinar)
    })();
    match result {
        Ok(webinar) => ok(webinar),
        Err(error) => err(error),
    }
}

async fn end_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Webinar> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let mut webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can end the webinar",
            ));
        }
        if webinar.status != WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "Only a live webinar can be ended",
            ));
        }
        let now = Utc::now();
        webinar.status = WebinarStatus::Ended;
        webinar.ended_at = Some(now);
        webinar.updated_at = now;
        if webinar.meeting_room_id.is_some() {
            webinar.meeting_link_status = "ended".to_string();
        }
        state.webinars.insert(webinar.id, webinar.clone());
        close_open_sessions(&state, webinar.id, now);
        state.emit(
            "webinar.ended",
            json!({
                "webinar_id": webinar.id.to_string(),
                "host_id": webinar.host_id,
                "started_at": webinar.started_at,
                "ended_at": now.to_rfc3339(),
                "meeting_room_id": webinar.meeting_room_id.map(|id| id.to_string()),
            }),
        );
        Ok(webinar)
    })();
    match result {
        Ok(webinar) => ok(webinar),
        Err(error) => err(error),
    }
}

async fn link_meeting(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<LinkMeetingRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Webinar> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let mut webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can link a meetings room",
            ));
        }
        if webinar.status == WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "The linked meetings room cannot change while the webinar is live",
            ));
        }
        if webinar.status == WebinarStatus::Ended || webinar.status == WebinarStatus::Cancelled {
            return Err(UnifiedError::unprocessable(
                "An ended or cancelled webinar cannot link a meetings room",
            ));
        }
        let room_id = parse_uuid(&input.meeting_room_id, "meeting_room")?;
        webinar.meeting_room_id = Some(room_id);
        webinar.meeting_link_status = "linked".to_string();
        webinar.updated_at = Utc::now();
        state.webinars.insert(webinar.id, webinar.clone());
        state.emit(
            "webinar.meeting.linked",
            json!({
                "webinar_id": webinar.id.to_string(),
                "meeting_room_id": room_id.to_string(),
            }),
        );
        Ok(webinar)
    })();
    match result {
        Ok(webinar) => ok(webinar),
        Err(error) => err(error),
    }
}

async fn mute_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MuteRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can change mute state",
            ));
        }
        if webinar.status != WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "Mute controls are only available while the webinar is live",
            ));
        }
        let audio_muted = input.audio_muted.unwrap_or(true);
        let video_muted = input.video_muted.unwrap_or(false);
        let now = Utc::now();
        let mut targets: Vec<String> = Vec::new();
        if input.mute_all.unwrap_or(false) {
            targets = state
                .attendance
                .iter()
                .filter(|entry| {
                    entry.value().webinar_id == webinar.id && entry.value().left_at.is_none()
                })
                .map(|entry| entry.value().user_id.clone())
                .collect();
            targets.sort();
            targets.dedup();
            if targets.is_empty() {
                return Err(UnifiedError::unprocessable(
                    "Nobody is currently in the live session to mute",
                ));
            }
        } else {
            let target = match &input.user_id {
                Some(raw) => require_text(raw, "user_id", 128)?,
                None => {
                    return Err(UnifiedError::bad_request(
                        "user_id is required unless mute_all is true",
                    ));
                }
            };
            if open_session(&state, webinar.id, &target).is_none()
                && !is_staff(&webinar, &target, &[])
            {
                return Err(UnifiedError::not_found(format!(
                    "User {target} is not currently in this webinar"
                )));
            }
            targets.push(target);
        }
        for user_id in &targets {
            let key = format!("{}:{}", webinar.id, user_id);
            let record = WebinarMuteState {
                webinar_id: webinar.id,
                user_id: user_id.clone(),
                audio_muted,
                video_muted,
                updated_at: now,
            };
            state.mutes.insert(key, record);
        }
        state.emit(
            "webinar.mute_changed",
            json!({
                "webinar_id": webinar.id.to_string(),
                "changed_by": actor,
                "user_ids": targets,
                "mute_all": input.mute_all.unwrap_or(false),
                "audio_muted": audio_muted,
                "video_muted": video_muted,
            }),
        );
        Ok(json!({
            "webinar_id": webinar.id.to_string(),
            "user_ids": targets,
            "audio_muted": audio_muted,
            "video_muted": video_muted,
        }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error),
    }
}

async fn get_live(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<LiveView> = (|| {
        let _actor = actor_id(&req)?;
        let webinar = load_webinar(&state, &path.into_inner())?;
        let attendee_count = state
            .attendance
            .iter()
            .filter(|entry| {
                entry.value().webinar_id == webinar.id && entry.value().left_at.is_none()
            })
            .count() as u64;
        let mute_states: Vec<WebinarMuteState> = state
            .mutes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.webinar_id == webinar.id)
            .collect();
        let open_polls = state
            .polls
            .iter()
            .filter(|entry| entry.value().webinar_id == webinar.id && entry.value().status == "open")
            .count() as u64;
        let unanswered_questions = state
            .questions
            .iter()
            .filter(|entry| entry.value().webinar_id == webinar.id && !entry.value().answered)
            .count() as u64;
        Ok(LiveView {
            webinar_id: webinar.id,
            status: status_name(webinar.status).to_string(),
            meeting_room_id: webinar.meeting_room_id,
            meeting_link_status: webinar.meeting_link_status.clone(),
            started_at: webinar.started_at,
            attendee_count,
            registered_count: registered_count(&state, webinar.id) as u64,
            mute_states,
            open_polls,
            unanswered_questions,
        })
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Registration handlers
// ---------------------------------------------------------------------------

async fn register_for_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarRegistration> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let webinar = load_webinar(&state, &path.into_inner())?;
        if webinar.status != WebinarStatus::Scheduled && webinar.status != WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "Registration is only open while the webinar is scheduled or live",
            ));
        }
        if is_staff(&webinar, &actor, &roles) {
            return Err(UnifiedError::conflict(
                "Hosts, co-hosts, and panelists do not register as attendees",
            ));
        }
        // An existing active registration is returned as-is, so a
        // double register is idempotent instead of an error.
        if let Some(existing) = find_registration(&state, webinar.id, &actor) {
            if matches!(
                existing.status.as_str(),
                "registered" | "waitlisted" | "pending_approval"
            ) {
                return Ok(existing);
            }
        }
        let now = Utc::now();
        let payment_status = if webinar.access_model == WebinarAccessModel::Free {
            "not_required".to_string()
        } else {
            "pending".to_string()
        };
        let status = if webinar.requires_approval {
            "pending_approval".to_string()
        } else if registered_count(&state, webinar.id) < webinar.capacity {
            "registered".to_string()
        } else if webinar.allow_waitlist {
            "waitlisted".to_string()
        } else {
            return Err(UnifiedError::conflict(
                "This webinar is full and its waitlist is closed",
            ));
        };
        let registration = WebinarRegistration {
            id: Uuid::now_v7(),
            webinar_id: webinar.id,
            user_id: actor.clone(),
            status: status.clone(),
            payment_status,
            registered_at: now,
            approved_at: None,
            cancelled_at: None,
            promoted_at: None,
            created_at: now,
            updated_at: now,
        };
        state
            .registrations
            .insert(registration.id, registration.clone());
        state.emit(
            "webinar.registration.created",
            json!({
                "webinar_id": webinar.id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": actor,
                "status": status,
                "access_model": access_name(webinar.access_model),
                "product_id": webinar.product_id,
                "payment_status": registration.payment_status,
                "registered_at": now.to_rfc3339(),
            }),
        );
        if status == "waitlisted" {
            state.emit(
                "webinar.registration.waitlisted",
                json!({
                    "webinar_id": webinar.id.to_string(),
                    "registration_id": registration.id.to_string(),
                    "user_id": registration.user_id,
                }),
            );
        }
        // Paid registration: ask billing / store to collect payment.
        // This module never charges anything itself; the order and
        // the entitlement live with the store module, and the
        // registration flips to `paid` when `order.paid` (or
        // `store.order.completed`) answers for this product.
        if webinar.access_model == WebinarAccessModel::Paid {
            state.emit(
                "webinar.registration.payment_required",
                json!({
                    "webinar_id": webinar.id.to_string(),
                    "registration_id": registration.id.to_string(),
                    "user_id": registration.user_id,
                    "product_id": webinar.product_id,
                }),
            );
        }
        Ok(registration)
    })();
    match result {
        Ok(registration) => created(registration),
        Err(error) => err(error),
    }
}

async fn list_registrations(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<WebinarRegistration>> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let params = query.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can list registrations",
            ));
        }
        let status_filter = params.filters.get("status").cloned();
        let user_filter = params.filters.get("user_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<WebinarRegistration> = state
            .registrations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.webinar_id != webinar.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &row.status != status {
                        return false;
                    }
                }
                if let Some(user_id) = &user_filter {
                    if &row.user_id != user_id {
                        return false;
                    }
                }
                if !search.is_empty() && !row.user_id.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["registered_at", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "registered_at" => a.registered_at.cmp(&b.registered_at),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

async fn my_registration(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarRegistration> = (|| {
        let actor = actor_id(&req)?;
        let webinar = load_webinar(&state, &path.into_inner())?;
        find_registration(&state, webinar.id, &actor).ok_or_else(|| {
            UnifiedError::not_found(format!(
                "No registration for user {actor} on webinar {}",
                webinar.id
            ))
        })
    })();
    match result {
        Ok(registration) => ok(registration),
        Err(error) => err(error),
    }
}

async fn approve_registration(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarRegistration> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, user_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can approve registrations",
            ));
        }
        let mut registration =
            find_registration(&state, webinar.id, &user_id).ok_or_else(|| {
                UnifiedError::not_found(format!("No registration for user {user_id}"))
            })?;
        if registration.status != "pending_approval" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a pending registration can be approved (current status: {})",
                registration.status
            )));
        }
        let now = Utc::now();
        if registered_count(&state, webinar.id) < webinar.capacity {
            registration.status = "registered".to_string();
            registration.approved_at = Some(now);
        } else if webinar.allow_waitlist {
            // No free seat: approval still counts, but the attendee
            // joins the waitlist and is auto-promoted when a seat
            // frees up.
            registration.status = "waitlisted".to_string();
            registration.approved_at = Some(now);
        } else {
            return Err(UnifiedError::conflict(
                "This webinar is full and its waitlist is closed",
            ));
        }
        registration.updated_at = now;
        state
            .registrations
            .insert(registration.id, registration.clone());
        state.emit(
            "webinar.registration.approved",
            json!({
                "webinar_id": webinar.id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": registration.user_id,
                "status": registration.status,
                "approved_by": actor,
            }),
        );
        state.notify(
            &registration.user_id,
            &format!("Approved: {}", webinar.title),
            "Your registration has been approved.",
            Some(format!("/api/v1/webinars/{}", webinar.id)),
        );
        Ok(registration)
    })();
    match result {
        Ok(registration) => ok(registration),
        Err(error) => err(error),
    }
}

async fn deny_registration(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarRegistration> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, user_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can deny registrations",
            ));
        }
        let mut registration =
            find_registration(&state, webinar.id, &user_id).ok_or_else(|| {
                UnifiedError::not_found(format!("No registration for user {user_id}"))
            })?;
        if registration.status != "pending_approval" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a pending registration can be denied (current status: {})",
                registration.status
            )));
        }
        registration.status = "denied".to_string();
        registration.updated_at = Utc::now();
        state
            .registrations
            .insert(registration.id, registration.clone());
        state.emit(
            "webinar.registration.denied",
            json!({
                "webinar_id": webinar.id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": registration.user_id,
                "denied_by": actor,
            }),
        );
        state.notify(
            &registration.user_id,
            &format!("Registration denied: {}", webinar.title),
            "Your registration request was not approved.",
            Some(format!("/api/v1/webinars/{}", webinar.id)),
        );
        Ok(registration)
    })();
    match result {
        Ok(registration) => ok(registration),
        Err(error) => err(error),
    }
}

async fn cancel_registration(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarRegistration> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, user_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        let is_self = actor == user_id;
        if !is_self && !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the registrant, the webinar host, a co-host, or an admin can cancel a registration",
            ));
        }
        let mut registration =
            find_registration(&state, webinar.id, &user_id).ok_or_else(|| {
                UnifiedError::not_found(format!("No registration for user {user_id}"))
            })?;
        if !matches!(
            registration.status.as_str(),
            "registered" | "waitlisted" | "pending_approval"
        ) {
            return Err(UnifiedError::unprocessable(format!(
                "A registration with status {} cannot be cancelled",
                registration.status
            )));
        }
        let was_registered = registration.status == "registered";
        registration.status = "cancelled".to_string();
        registration.cancelled_at = Some(Utc::now());
        registration.updated_at = Utc::now();
        state
            .registrations
            .insert(registration.id, registration.clone());
        state.emit(
            "webinar.registration.cancelled",
            json!({
                "webinar_id": webinar.id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": registration.user_id,
                "cancelled_by": actor,
            }),
        );
        // A freed seat auto-promotes the earliest waitlisted user.
        if was_registered {
            promote_waitlist(&state, &webinar);
        }
        Ok(registration)
    })();
    match result {
        Ok(registration) => ok(registration),
        Err(error) => err(error),
    }
}

async fn promote_registration(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarRegistration> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, user_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can promote a waitlisted registration",
            ));
        }
        let mut registration =
            find_registration(&state, webinar.id, &user_id).ok_or_else(|| {
                UnifiedError::not_found(format!("No registration for user {user_id}"))
            })?;
        if registration.status != "waitlisted" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a waitlisted registration can be promoted (current status: {})",
                registration.status
            )));
        }
        if registered_count(&state, webinar.id) >= webinar.capacity {
            return Err(UnifiedError::conflict(
                "There is no free seat to promote into",
            ));
        }
        let now = Utc::now();
        registration.status = "registered".to_string();
        registration.promoted_at = Some(now);
        registration.updated_at = now;
        state
            .registrations
            .insert(registration.id, registration.clone());
        state.emit(
            "webinar.registration.promoted",
            json!({
                "webinar_id": webinar.id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": registration.user_id,
                "promoted_by": actor,
                "promoted_at": now.to_rfc3339(),
            }),
        );
        state.notify(
            &registration.user_id,
            &format!("A seat opened up: {}", webinar.title),
            "You have been promoted from the waitlist and now hold a seat.",
            Some(format!("/api/v1/webinars/{}", webinar.id)),
        );
        Ok(registration)
    })();
    match result {
        Ok(registration) => ok(registration),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Attendance handlers
// ---------------------------------------------------------------------------

async fn join_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarAttendance> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let webinar = load_webinar(&state, &path.into_inner())?;
        if webinar.status != WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "The webinar is not live; only live webinars can be joined",
            ));
        }
        let staff = is_staff(&webinar, &actor, &roles);
        if !staff {
            let registration = find_registration(&state, webinar.id, &actor).ok_or_else(|| {
                UnifiedError::forbidden("Only registered attendees can join this webinar")
            })?;
            if registration.status != "registered" {
                return Err(UnifiedError::forbidden(format!(
                    "Registration status {} cannot join this webinar",
                    registration.status
                )));
            }
            check_entitlement(&state, &webinar, &actor, &roles)?;
        }
        // Re-joining while an open session exists returns that
        // session instead of opening a second one.
        if let Some(session) = open_session(&state, webinar.id, &actor) {
            return Ok(session);
        }
        let now = Utc::now();
        let session = WebinarAttendance {
            id: Uuid::now_v7(),
            webinar_id: webinar.id,
            user_id: actor.clone(),
            joined_at: now,
            left_at: None,
            created_at: now,
            updated_at: now,
        };
        state.attendance.insert(session.id, session.clone());
        state.emit(
            "webinar.attendee.joined",
            json!({
                "webinar_id": webinar.id.to_string(),
                "session_id": session.id.to_string(),
                "user_id": actor,
                "joined_at": now.to_rfc3339(),
                "meeting_room_id": webinar.meeting_room_id.map(|id| id.to_string()),
            }),
        );
        Ok(session)
    })();
    match result {
        Ok(session) => created(session),
        Err(error) => err(error),
    }
}

async fn leave_webinar(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarAttendance> = (|| {
        let actor = actor_id(&req)?;
        let webinar = load_webinar(&state, &path.into_inner())?;
        let session = open_session(&state, webinar.id, &actor).ok_or_else(|| {
            UnifiedError::not_found(format!(
                "User {actor} has no open attendance session on this webinar"
            ))
        })?;
        let now = Utc::now();
        let mut closed = session.clone();
        closed.left_at = Some(now);
        closed.updated_at = now;
        state.attendance.insert(closed.id, closed.clone());
        state.emit(
            "webinar.attendee.left",
            json!({
                "webinar_id": webinar.id.to_string(),
                "session_id": closed.id.to_string(),
                "user_id": actor,
                "joined_at": closed.joined_at.to_rfc3339(),
                "left_at": now.to_rfc3339(),
                "duration_secs": (now - closed.joined_at).num_seconds(),
            }),
        );
        Ok(closed)
    })();
    match result {
        Ok(session) => ok(session),
        Err(error) => err(error),
    }
}

async fn list_attendance(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<AttendanceView>> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let params = query.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can list attendance",
            ));
        }
        let user_filter = params.filters.get("user_id").cloned();
        let now = Utc::now();
        let mut items: Vec<AttendanceView> = state
            .attendance
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.webinar_id != webinar.id {
                    return false;
                }
                if let Some(user_id) = &user_filter {
                    if &row.user_id != user_id {
                        return false;
                    }
                }
                true
            })
            .map(|row| {
                let end = row.left_at.unwrap_or(now);
                AttendanceView {
                    id: row.id,
                    user_id: row.user_id,
                    joined_at: row.joined_at,
                    left_at: row.left_at,
                    duration_secs: (end - row.joined_at).num_seconds(),
                }
            })
            .collect();
        let sort = params.allowed_sort(&["joined_at", "duration_secs"], "joined_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "duration_secs" => a.duration_secs.cmp(&b.duration_secs),
                _ => a.joined_at.cmp(&b.joined_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Poll handlers
// ---------------------------------------------------------------------------

fn require_poll_access(
    state: &WebinarsState,
    webinar: &Webinar,
    user_id: &str,
    roles: &[String],
) -> UnifiedResult<()> {
    if is_staff(webinar, user_id, roles) {
        return Ok(());
    }
    match find_registration(state, webinar.id, user_id) {
        Some(row) if row.status == "registered" => Ok(()),
        _ => Err(UnifiedError::forbidden(
            "Only registered attendees and webinar staff can use polls",
        )),
    }
}

async fn list_polls(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<PollView>> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let params = query.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        require_poll_access(&state, &webinar, &actor, &roles)?;
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<PollView> = state
            .polls
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|poll| {
                if poll.webinar_id != webinar.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &poll.status != status {
                        return false;
                    }
                }
                true
            })
            .map(|poll| poll_view(&state, &poll))
            .collect();
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = a.created_at.cmp(&b.created_at);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

async fn create_poll(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreatePollRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PollView> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        if !is_staff(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, a panelist, or an admin can create polls",
            ));
        }
        if !webinar.polls_enabled {
            return Err(UnifiedError::unprocessable(
                "Polls are disabled for this webinar",
            ));
        }
        if webinar.status == WebinarStatus::Ended || webinar.status == WebinarStatus::Cancelled {
            return Err(UnifiedError::unprocessable(
                "Polls cannot be created for an ended or cancelled webinar",
            ));
        }
        let question = require_text(&input.question, "question", MAX_QUESTION_LEN)?;
        let config = state.config();
        if input.options.len() < 2 {
            return Err(UnifiedError::bad_request(
                "A poll needs at least 2 options",
            ));
        }
        if input.options.len() > config.poll_max_options {
            return Err(UnifiedError::bad_request(format!(
                "A poll can have at most {} options",
                config.poll_max_options
            )));
        }
        let mut options: Vec<String> = Vec::new();
        for raw in &input.options {
            options.push(require_text(raw, "options", MAX_POLL_OPTION_LEN)?);
        }
        let now = Utc::now();
        let poll = WebinarPoll {
            id: Uuid::now_v7(),
            webinar_id: webinar.id,
            question,
            options,
            allow_multiple: input.allow_multiple.unwrap_or(false),
            status: "open".to_string(),
            created_by: actor.clone(),
            created_at: now,
            closed_at: None,
            updated_at: now,
        };
        state.polls.insert(poll.id, poll.clone());
        state.emit(
            "webinar.poll.created",
            json!({
                "webinar_id": webinar.id.to_string(),
                "poll_id": poll.id.to_string(),
                "question": poll.question,
                "options": poll.options,
                "allow_multiple": poll.allow_multiple,
                "created_by": actor,
            }),
        );
        Ok(poll_view(&state, &poll))
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error),
    }
}

async fn get_poll(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<PollView> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, poll_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        require_poll_access(&state, &webinar, &actor, &roles)?;
        let poll = load_poll(&state, webinar.id, &poll_id)?;
        Ok(poll_view(&state, &poll))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error),
    }
}

async fn vote_poll(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<VotePollRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PollView> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let (webinar_id, poll_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        require_poll_access(&state, &webinar, &actor, &roles)?;
        if webinar.status != WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "Polls can only be voted in while the webinar is live",
            ));
        }
        let poll = load_poll(&state, webinar.id, &poll_id)?;
        if poll.status != "open" {
            return Err(UnifiedError::unprocessable("This poll is closed"));
        }
        if input.option_indexes.is_empty() {
            return Err(UnifiedError::bad_request(
                "option_indexes must not be empty",
            ));
        }
        if !poll.allow_multiple && input.option_indexes.len() != 1 {
            return Err(UnifiedError::bad_request(
                "This poll accepts exactly one option",
            ));
        }
        let mut cleaned: Vec<usize> = Vec::new();
        for index in &input.option_indexes {
            if *index >= poll.options.len() {
                return Err(UnifiedError::bad_request(format!(
                    "Option index {index} is out of range for this poll",
                )));
            }
            if !cleaned.contains(index) {
                cleaned.push(*index);
            }
        }
        let now = Utc::now();
        let key = format!("{}:{}", poll.id, actor);
        let vote = match state.votes.get(&key) {
            Some(existing) => {
                let mut updated = existing.value().clone();
                updated.option_indexes = cleaned.clone();
                updated.updated_at = now;
                updated
            }
            None => WebinarPollVote {
                id: Uuid::now_v7(),
                poll_id: poll.id,
                webinar_id: webinar.id,
                user_id: actor.clone(),
                option_indexes: cleaned.clone(),
                created_at: now,
                updated_at: now,
            },
        };
        state.votes.insert(key, vote);
        state.emit(
            "webinar.poll.voted",
            json!({
                "webinar_id": webinar.id.to_string(),
                "poll_id": poll.id.to_string(),
                "user_id": actor,
                "option_indexes": cleaned,
            }),
        );
        Ok(poll_view(&state, &poll))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error),
    }
}

async fn close_poll(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<PollView> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, poll_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can close polls",
            ));
        }
        let mut poll = load_poll(&state, webinar.id, &poll_id)?;
        if poll.status != "open" {
            return Err(UnifiedError::unprocessable("This poll is already closed"));
        }
        let now = Utc::now();
        poll.status = "closed".to_string();
        poll.closed_at = Some(now);
        poll.updated_at = now;
        state.polls.insert(poll.id, poll.clone());
        state.emit(
            "webinar.poll.closed",
            json!({
                "webinar_id": webinar.id.to_string(),
                "poll_id": poll.id.to_string(),
                "closed_by": actor,
                "closed_at": now.to_rfc3339(),
            }),
        );
        Ok(poll_view(&state, &poll))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error),
    }
}

async fn delete_poll(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, poll_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        if !is_host(&webinar, &actor) && !is_admin(&roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host or an admin can delete polls",
            ));
        }
        let poll = load_poll(&state, webinar.id, &poll_id)?;
        state.polls.remove(&poll.id);
        let vote_keys: Vec<String> = state
            .votes
            .iter()
            .filter(|entry| entry.value().poll_id == poll.id)
            .map(|entry| entry.key().clone())
            .collect();
        for key in vote_keys {
            state.votes.remove(&key);
        }
        state.emit(
            "webinar.poll.deleted",
            json!({
                "webinar_id": webinar.id.to_string(),
                "poll_id": poll.id.to_string(),
                "deleted_by": actor,
            }),
        );
        Ok(json!({ "deleted": true, "poll_id": poll.id.to_string() }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Q&A handlers
// ---------------------------------------------------------------------------

fn require_qa_access(
    state: &WebinarsState,
    webinar: &Webinar,
    user_id: &str,
    roles: &[String],
) -> UnifiedResult<()> {
    if is_staff(webinar, user_id, roles) {
        return Ok(());
    }
    match find_registration(state, webinar.id, user_id) {
        Some(row) if row.status == "registered" => Ok(()),
        _ => Err(UnifiedError::forbidden(
            "Only registered attendees and webinar staff can use Q&A",
        )),
    }
}

async fn list_questions(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<QuestionView>> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let params = query.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        require_qa_access(&state, &webinar, &actor, &roles)?;
        let answered_filter = params.filters.get("answered").cloned();
        let mut items: Vec<QuestionView> = state
            .questions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.webinar_id != webinar.id {
                    return false;
                }
                if let Some(answered) = &answered_filter {
                    let expected = match answered.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if row.answered != expected {
                        return false;
                    }
                }
                true
            })
            .map(|row| question_view(&row, &actor))
            .collect();
        // Most upvoted first; ties break by earliest asked.
        items.sort_by(|a, b| {
            b.upvotes
                .cmp(&a.upvotes)
                .then_with(|| a.created_at.cmp(&b.created_at))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

async fn ask_question(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AskQuestionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<QuestionView> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        require_qa_access(&state, &webinar, &actor, &roles)?;
        if !webinar.qa_enabled {
            return Err(UnifiedError::unprocessable(
                "Q&A is disabled for this webinar",
            ));
        }
        if webinar.status != WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "Questions can only be asked while the webinar is live",
            ));
        }
        let body_text = require_text(&input.body, "body", MAX_QA_BODY_LEN)?;
        let now = Utc::now();
        let question = WebinarQuestion {
            id: Uuid::now_v7(),
            webinar_id: webinar.id,
            author_id: actor.clone(),
            body: body_text,
            upvoted_by: Vec::new(),
            answered: false,
            answered_by: None,
            created_at: now,
            updated_at: now,
        };
        state.questions.insert(question.id, question.clone());
        state.emit(
            "webinar.question.asked",
            json!({
                "webinar_id": webinar.id.to_string(),
                "question_id": question.id.to_string(),
                "author_id": actor,
                "body": question.body,
            }),
        );
        Ok(question_view(&question, &actor))
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error),
    }
}

async fn upvote_question(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<QuestionView> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, question_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        require_qa_access(&state, &webinar, &actor, &roles)?;
        if webinar.status != WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "Questions can only be upvoted while the webinar is live",
            ));
        }
        let mut question = load_question(&state, webinar.id, &question_id)?;
        // One upvote per user: a repeat upvote is a no-op that
        // returns the current state instead of an error.
        if !question.upvoted_by.iter().any(|id| id == &actor) {
            question.upvoted_by.push(actor.clone());
            question.updated_at = Utc::now();
            state.questions.insert(question.id, question.clone());
            state.emit(
                "webinar.question.upvoted",
                json!({
                    "webinar_id": webinar.id.to_string(),
                    "question_id": question.id.to_string(),
                    "user_id": actor,
                    "upvotes": question.upvoted_by.len(),
                }),
            );
        }
        Ok(question_view(&question, &actor))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error),
    }
}

async fn answer_question(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<QuestionView> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, question_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        if !is_staff(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, a panelist, or an admin can mark a question answered",
            ));
        }
        let mut question = load_question(&state, webinar.id, &question_id)?;
        if question.answered {
            return Err(UnifiedError::unprocessable(
                "This question is already marked answered",
            ));
        }
        question.answered = true;
        question.answered_by = Some(actor.clone());
        question.updated_at = Utc::now();
        state.questions.insert(question.id, question.clone());
        state.emit(
            "webinar.question.answered",
            json!({
                "webinar_id": webinar.id.to_string(),
                "question_id": question.id.to_string(),
                "answered_by": actor,
            }),
        );
        Ok(question_view(&question, &actor))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error),
    }
}

async fn delete_question(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, question_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        let question = load_question(&state, webinar.id, &question_id)?;
        if !is_host(&webinar, &actor) && !is_admin(&roles) && question.author_id != actor {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, an admin, or the question's author can delete it",
            ));
        }
        state.questions.remove(&question.id);
        state.emit(
            "webinar.question.deleted",
            json!({
                "webinar_id": webinar.id.to_string(),
                "question_id": question.id.to_string(),
                "deleted_by": actor,
            }),
        );
        Ok(json!({ "deleted": true, "question_id": question.id.to_string() }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Chat handlers
// ---------------------------------------------------------------------------

async fn get_chat_channel(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let webinar = load_webinar(&state, &path.into_inner())?;
        require_qa_access(&state, &webinar, &actor, &roles)?;
        let channel_id = webinar
            .chat_channel_id
            .clone()
            .unwrap_or_else(|| format!("webinar:{}", webinar.id));
        Ok(json!({
            "webinar_id": webinar.id.to_string(),
            "channel_id": channel_id,
            "chat_enabled": webinar.chat_enabled,
            "status": status_name(webinar.status),
            "module": "chat",
            "note": "Messages live in the chat module; this webinar only names the channel and emits webinar.chat.message events for it.",
        }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error),
    }
}

async fn post_chat_message(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PostChatRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        require_qa_access(&state, &webinar, &actor, &roles)?;
        if !webinar.chat_enabled {
            return Err(UnifiedError::unprocessable(
                "Chat is disabled for this webinar",
            ));
        }
        if webinar.status != WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "Chat messages can only be posted while the webinar is live",
            ));
        }
        let body_text = require_text(&input.body, "body", MAX_CHAT_BODY_LEN)?;
        let channel_id = webinar
            .chat_channel_id
            .clone()
            .unwrap_or_else(|| format!("webinar:{}", webinar.id));
        let now = Utc::now();
        let message_id = Uuid::now_v7();
        // The chat module owns storage and delivery: this module
        // emits the event and reports honestly whether any handler
        // is listening, instead of pretending the message landed.
        let delivered = state.bus_has("webinar.chat.message");
        state.emit(
            "webinar.chat.message",
            json!({
                "webinar_id": webinar.id.to_string(),
                "message_id": message_id.to_string(),
                "channel_id": channel_id,
                "author_id": actor,
                "body": body_text,
                "sent_at": now.to_rfc3339(),
            }),
        );
        Ok(json!({
            "message_id": message_id.to_string(),
            "webinar_id": webinar.id.to_string(),
            "channel_id": channel_id,
            "author_id": actor,
            "body": body_text,
            "sent_at": now.to_rfc3339(),
            "delivered": delivered,
            "delivered_via": "webinar.chat.message",
        }))
    })();
    match result {
        Ok(value) => created(value),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Recording / replay handlers
// ---------------------------------------------------------------------------

async fn list_recordings(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<WebinarRecording>> = (|| {
        let actor = actor_id(&req)?;
        let params = query.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<WebinarRecording> = state
            .recordings
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.webinar_id != webinar.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &row.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        let _ = actor;
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = a.created_at.cmp(&b.created_at);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

async fn attach_recording(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AttachRecordingRequest>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarRecording> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can attach recordings",
            ));
        }
        if !webinar.recording_enabled {
            return Err(UnifiedError::unprocessable(
                "Recording is disabled for this webinar",
            ));
        }
        let meeting_recording_id = match &input.meeting_recording_id {
            Some(raw) => Some(parse_uuid(raw, "meeting_recording")?),
            None => None,
        };
        let title = match &input.title {
            Some(raw) => Some(require_text(raw, "title", MAX_TITLE_LEN)?),
            None => None,
        };
        let status = match &input.status {
            Some(raw) => {
                let cleaned = require_text(raw, "status", 32)?;
                match cleaned.as_str() {
                    "processing" | "ready" | "failed" => cleaned,
                    _ => {
                        return Err(UnifiedError::bad_request(
                            "status must be processing, ready, or failed",
                        ));
                    }
                }
            }
            None => "processing".to_string(),
        };
        let storage_target = require_text(&input.storage_target, "storage_target", 128)?;
        let storage_key = require_text(&input.storage_key, "storage_key", 512)?;
        let storage_url = match &input.storage_url {
            Some(raw) => Some(require_text(raw, "storage_url", 1024)?),
            None => None,
        };
        let now = Utc::now();
        let recording = WebinarRecording {
            id: Uuid::now_v7(),
            webinar_id: webinar.id,
            meeting_recording_id,
            title,
            status: status.clone(),
            storage_target: Some(storage_target),
            storage_key: Some(storage_key),
            storage_url,
            size_bytes: input.size_bytes,
            duration_secs: input.duration_secs,
            created_at: now,
            updated_at: now,
        };
        state.recordings.insert(recording.id, recording.clone());
        state.emit(
            "webinar.recording.attached",
            json!({
                "webinar_id": webinar.id.to_string(),
                "recording_id": recording.id.to_string(),
                "meeting_recording_id": meeting_recording_id.map(|id| id.to_string()),
                "status": status,
                "storage_target": recording.storage_target,
                "storage_key": recording.storage_key,
                "attached_by": actor,
            }),
        );
        Ok(recording)
    })();
    match result {
        Ok(recording) => created(recording),
        Err(error) => err(error),
    }
}

async fn publish_recording(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarRecording> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, recording_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can publish recordings",
            ));
        }
        let mut recording = load_recording(&state, webinar.id, &recording_id)?;
        if recording.status == "ready" {
            return Err(UnifiedError::unprocessable(
                "This recording is already ready for replay",
            ));
        }
        recording.status = "ready".to_string();
        recording.updated_at = Utc::now();
        state.recordings.insert(recording.id, recording.clone());
        state.emit(
            "webinar.recording.ready",
            json!({
                "webinar_id": webinar.id.to_string(),
                "recording_id": recording.id.to_string(),
                "published_by": actor,
            }),
        );
        Ok(recording)
    })();
    match result {
        Ok(recording) => ok(recording),
        Err(error) => err(error),
    }
}

async fn delete_recording(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, recording_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        if !is_host(&webinar, &actor) && !is_admin(&roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host or an admin can delete recordings",
            ));
        }
        let recording = load_recording(&state, webinar.id, &recording_id)?;
        state.recordings.remove(&recording.id);
        state.emit(
            "webinar.recording.deleted",
            json!({
                "webinar_id": webinar.id.to_string(),
                "recording_id": recording.id.to_string(),
                "deleted_by": actor,
            }),
        );
        Ok(json!({ "deleted": true, "recording_id": recording.id.to_string() }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error),
    }
}

async fn get_replay(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ReplayView> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let webinar = load_webinar(&state, &path.into_inner())?;
        let staff = is_staff(&webinar, &actor, &roles);
        let registered = find_registration(&state, webinar.id, &actor)
            .is_some_and(|row| row.status == "registered");
        let attended = state
            .attendance
            .iter()
            .any(|entry| entry.value().webinar_id == webinar.id && entry.value().user_id == actor);
        if webinar.status != WebinarStatus::Ended && !staff {
            return Err(UnifiedError::unprocessable(
                "Replay is only available once the webinar has ended",
            ));
        }
        if !staff && !registered && !attended {
            return Err(UnifiedError::forbidden(
                "Replay is only available to the webinar staff and its registered attendees",
            ));
        }
        let mut recordings: Vec<WebinarRecording> = state
            .recordings
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.webinar_id == webinar.id && row.status == "ready")
            .collect();
        recordings.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(ReplayView {
            webinar: webinar.clone(),
            recordings,
            can_watch: true,
            attended,
        })
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Follow-up handler
// ---------------------------------------------------------------------------

async fn send_follow_up(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FollowUpRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let mut webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can send the follow-up",
            ));
        }
        if webinar.status != WebinarStatus::Ended {
            return Err(UnifiedError::unprocessable(
                "Follow-up can only be sent after the webinar has ended",
            ));
        }
        let message = match &input.message {
            Some(raw) => Some(require_text(raw, "message", MAX_FOLLOW_UP_MESSAGE_LEN)?),
            None => None,
        };
        let distribution_id = input
            .survey_distribution_id
            .as_ref()
            .map(|raw| raw.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| webinar.survey_distribution_id.clone());
        let include_no_shows = input.include_no_shows.unwrap_or(false);
        let replay_link = format!("/api/v1/webinars/{}/replay", webinar.id);
        let ready_recording_url = state
            .recordings
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.webinar_id == webinar.id && row.status == "ready")
            .min_by(|a, b| a.created_at.cmp(&b.created_at))
            .and_then(|row| row.storage_url.clone());
        let mut recipients: Vec<String> = Vec::new();
        for entry in state.attendance.iter() {
            let row = entry.value();
            if row.webinar_id == webinar.id && !recipients.contains(&row.user_id) {
                recipients.push(row.user_id.clone());
            }
        }
        if include_no_shows {
            for entry in state.registrations.iter() {
                let row = entry.value();
                if row.webinar_id == webinar.id
                    && row.status == "registered"
                    && !recipients.contains(&row.user_id)
                {
                    recipients.push(row.user_id.clone());
                }
            }
        }
        let body_text = message.unwrap_or_else(|| {
            "Thank you for joining. The recording and a short feedback survey are ready for you.".to_string()
        });
        let mut notified = 0u64;
        for user_id in &recipients {
            state.notify(
                user_id,
                &format!("Follow-up: {}", webinar.title),
                &body_text,
                Some(replay_link.clone()),
            );
            notified += 1;
        }
        // Ask the surveys module to send the follow-up survey to
        // its distribution. The surveys module owns the
        // distribution, the recipients, and the delivery; this
        // module only requests it.
        let survey_requested = match &distribution_id {
            Some(id) => {
                state.emit(
                    "surveys.send_requested",
                    json!({
                        "distribution_id": id,
                        "survey_id": webinar.survey_id,
                        "webinar_id": webinar.id.to_string(),
                    }),
                );
                true
            }
            None => false,
        };
        let now = Utc::now();
        webinar.follow_up_sent_at = Some(now);
        webinar.follow_up_count = notified;
        webinar.updated_at = now;
        state.webinars.insert(webinar.id, webinar.clone());
        state.emit(
            "webinar.follow_up.sent",
            json!({
                "webinar_id": webinar.id.to_string(),
                "sent_by": actor,
                "notified": notified,
                "include_no_shows": include_no_shows,
                "survey_distribution_id": distribution_id,
                "survey_requested": survey_requested,
                "recording_url": ready_recording_url,
                "replay_link": replay_link,
                "sent_at": now.to_rfc3339(),
            }),
        );
        Ok(json!({
            "webinar_id": webinar.id.to_string(),
            "notified": notified,
            "survey_requested": survey_requested,
            "survey_distribution_id": distribution_id,
            "recording_url": ready_recording_url,
            "replay_link": replay_link,
            "sent_at": now.to_rfc3339(),
        }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error),
    }
}

// ---------------------------------------------------------------------------
// Analytics handler
// ---------------------------------------------------------------------------

async fn get_analytics(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<AnalyticsQuery>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarAnalytics> = (|| {
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can read analytics",
            ));
        }
        let config = state.config();
        let (total, registered, waitlisted, pending, cancelled, denied) =
            registration_counts(&state, webinar.id);
        let sessions: Vec<WebinarAttendance> = state
            .attendance
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.webinar_id == webinar.id)
            .collect();
        let mut attendee_ids: Vec<String> = Vec::new();
        for session in &sessions {
            if !attendee_ids.contains(&session.user_id) {
                attendee_ids.push(session.user_id.clone());
            }
        }
        let unique_attendees = attendee_ids.len() as u64;
        let attendance_rate = if registered > 0 {
            unique_attendees as f64 / registered as f64
        } else {
            0.0
        };
        let now = Utc::now();
        let total_watch: i64 = sessions
            .iter()
            .map(|row| (row.left_at.unwrap_or(now) - row.joined_at).num_seconds().max(0))
            .sum();
        let average_watch_secs = if unique_attendees > 0 {
            total_watch as f64 / unique_attendees as f64
        } else {
            0.0
        };
        // Drop-off buckets across the live window. The bucket width
        // comes from config (or the `bucket_mins` query override)
        // and widens automatically so a very long webinar never
        // produces more than 288 buckets.
        let start = webinar.started_at.unwrap_or(webinar.created_at);
        let end = webinar.ended_at.unwrap_or(now);
        let window_mins = ((end - start).num_seconds() / 60).max(1) as u64;
        let requested = query.bucket_mins.unwrap_or(config.drop_off_bucket_mins);
        let bucket_mins = requested
            .max(1)
            .max(window_mins.div_ceil(288));
        let mut drop_off: Vec<DropOffBucket> = Vec::new();
        let mut cursor = start;
        while cursor < end {
            let bucket_end = (cursor + chrono::Duration::minutes(bucket_mins as i64)).min(end);
            let active = sessions
                .iter()
                .filter(|row| row.joined_at < bucket_end && row.left_at.unwrap_or(end) > cursor)
                .count() as u64;
            drop_off.push(DropOffBucket {
                bucket_start: cursor,
                bucket_end,
                active_count: active,
            });
            cursor = bucket_end;
        }
        let polls: Vec<WebinarPoll> = state
            .polls
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|poll| poll.webinar_id == webinar.id)
            .collect();
        let mut poll_voters: Vec<String> = Vec::new();
        let mut poll_votes_total = 0u64;
        for entry in state.votes.iter() {
            let vote = entry.value();
            if vote.webinar_id != webinar.id {
                continue;
            }
            poll_votes_total += vote.option_indexes.len() as u64;
            if !poll_voters.contains(&vote.user_id) {
                poll_voters.push(vote.user_id.clone());
            }
        }
        let poll_unique_voters = poll_voters.len() as u64;
        let poll_participation_rate = if unique_attendees > 0 {
            poll_unique_voters as f64 / unique_attendees as f64
        } else {
            0.0
        };
        let questions: Vec<WebinarQuestion> = state
            .questions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.webinar_id == webinar.id)
            .collect();
        let questions_answered = questions.iter().filter(|row| row.answered).count() as u64;
        let mut qa_people: Vec<String> = Vec::new();
        for row in &questions {
            if !qa_people.contains(&row.author_id) {
                qa_people.push(row.author_id.clone());
            }
            for voter in &row.upvoted_by {
                if !qa_people.contains(voter) {
                    qa_people.push(voter.clone());
                }
            }
        }
        let qa_unique_participants = qa_people.len() as u64;
        let qa_participation_rate = if unique_attendees > 0 {
            qa_unique_participants as f64 / unique_attendees as f64
        } else {
            0.0
        };
        let recordings_ready = state
            .recordings
            .iter()
            .filter(|entry| entry.value().webinar_id == webinar.id && entry.value().status == "ready")
            .count() as u64;
        Ok(WebinarAnalytics {
            webinar_id: webinar.id,
            status: status_name(webinar.status).to_string(),
            registrations_total: total,
            registered_count: registered,
            waitlisted_count: waitlisted,
            pending_approval_count: pending,
            cancelled_count: cancelled,
            denied_count: denied,
            unique_attendees,
            attendance_rate,
            average_watch_secs,
            drop_off,
            polls_total: polls.len() as u64,
            poll_votes_total,
            poll_unique_voters,
            poll_participation_rate,
            questions_total: questions.len() as u64,
            questions_answered,
            qa_unique_participants,
            qa_participation_rate,
            recordings_ready,
            follow_up_sent_at: webinar.follow_up_sent_at,
        })
    })();
    match result {
        Ok(analytics) => ok(analytics),
        Err(error) => err(error),
    }
}
