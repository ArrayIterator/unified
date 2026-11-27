// webinars -- Webinar, registration, poll, question and request types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::helpers::*;

// Webinars module -- scheduled and recurring live webinars built on
// top of the meetings / video-stream module.
//
// Routes live under `/api/v1/webinars`:
//
//   Webinars:
//     GET/POST / -- list webinars / create a webinar.
//     GET/PATCH/DELETE /{id}.
//     POST /{id}/publish, /cancel, /start, /end, /link-meeting, /mute.
//     GET  /{id}/live -- live session view (status, attendees, mutes).
//     GET/PATCH /config -- configurable defaults (PATCH is admin only).
//   Registration:
//     POST /{id}/register -- register (free, paid, waitlist, approval).
//     GET  /{id}/registrations -- paginated, filter by status.
//     GET  /{id}/registrations/mine -- the caller's own registration.
//     POST /{id}/registrations/{user_id}/approve, /deny, /cancel,
//       /promote.
//   Attendance:
//     POST /{id}/join, /leave.
//     GET  /{id}/attendance -- join/leave history (paginated).
//   Live polls:
//     GET/POST /{id}/polls.
//     GET  /{id}/polls/{poll_id} -- poll with live results.
//     POST /{id}/polls/{poll_id}/vote, /close.
//     DELETE /{id}/polls/{poll_id}.
//   Live Q&A:
//     GET/POST /{id}/questions.
//     POST /{id}/questions/{question_id}/upvote, /answer.
//     DELETE /{id}/questions/{question_id}.
//   Live chat:
//     GET  /{id}/chat -- channel descriptor (messages live in chat).
//     POST /{id}/chat -- post a message (emitted as an event).
//   Recordings and replay:
//     GET/POST /{id}/recordings.
//     POST /{id}/recordings/{recording_id}/publish.
//     DELETE /{id}/recordings/{recording_id}.
//     GET  /{id}/replay -- replay page data.
//   Follow-up and analytics:
//     POST /{id}/follow-up -- recording link + survey invitation.
//     GET  /{id}/analytics -- registrations, attendance, drop-off,
//       poll and Q&A participation.
//
// HONESTY -- READ THIS FIRST. What is real in this module:
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
//   - `webinar.start_requested` -- the scheduler firing a
//     `webinar.start` job takes a scheduled webinar live.
//   - `webinar.reminder_requested` -- the scheduler firing a
//     `webinar.reminder` job notifies registered attendees.
//   - `order.paid`, `store.order.completed` -- a paid order for the
//     webinar's product marks the matching registration paid.

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_TITLE_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 2000;

pub(crate) const MAX_TIMEZONE_LEN: usize = 64;

pub(crate) const MAX_RECURRENCE_LEN: usize = 128;

/// Maximum recurrence interval in seconds (one year). A larger
/// interval is almost certainly a typo, not a real schedule.
pub(crate) const MAX_RECURRENCE_INTERVAL_SECS: u64 = 31_536_000;

pub(crate) const MAX_QUESTION_LEN: usize = 500;

pub(crate) const MAX_POLL_OPTION_LEN: usize = 200;

pub(crate) const MAX_CHAT_BODY_LEN: usize = 2000;

pub(crate) const MAX_QA_BODY_LEN: usize = 1000;

pub(crate) const MAX_FOLLOW_UP_MESSAGE_LEN: usize = 2000;

pub(crate) const MAX_PRODUCT_ID_LEN: usize = 128;

pub(crate) const MAX_CHANNEL_ID_LEN: usize = 128;

/// Configurable-default seed values. These are ONLY the initial
/// values loaded into `WebinarConfig`; an admin can change every one
/// of them at `PATCH /api/v1/webinars/config`, and per-webinar
/// fields override them. Nothing below is a hard-coded rule.
pub(crate) const SEED_DEFAULT_CAPACITY: usize = 100;

pub(crate) const SEED_MAX_CAPACITY: usize = 10_000;

pub(crate) const SEED_MIN_CAPACITY: usize = 1;

pub(crate) const SEED_DEFAULT_REQUIRES_APPROVAL: bool = false;

pub(crate) const SEED_DEFAULT_ALLOW_WAITLIST: bool = true;

pub(crate) const SEED_AUTO_PROMOTE_WAITLIST: bool = true;

pub(crate) const SEED_DEFAULT_TIMEZONE: &str = "UTC";

pub(crate) const SEED_REMINDER_LEAD_MINS: u64 = 60;

pub(crate) const SEED_DROP_OFF_BUCKET_MINS: u64 = 5;

pub(crate) const SEED_POLL_MAX_OPTIONS: usize = 10;

pub(crate) const SEED_CHAT_ENABLED_DEFAULT: bool = true;

pub(crate) const SEED_QA_ENABLED_DEFAULT: bool = true;

pub(crate) const SEED_POLLS_ENABLED_DEFAULT: bool = true;

pub(crate) const SEED_RECORDING_ENABLED_DEFAULT: bool = true;

// Types

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

// Request bodies

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
