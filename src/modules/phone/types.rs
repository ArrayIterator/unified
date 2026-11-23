//! Phone types: 1:1 and group voice/video calls over WebRTC,
//! store-and-forward signalling, call queues and routing
//! rules as data, presence, IVR menus and auto-response
//! scripts as data, consent-gated recording references, and
//! AI call summaries. Nothing about call handling behaviour
//! is hard-coded here: queues, ring order and timeouts,
//! routing rules and their priority, IVR menus and key-press
//! trees, and auto-response scripts are all rows an admin
//! creates through the API. Only the call status machine,
//! the signalling vocabulary, and the documented pure
//! functions below are fixed in code. Money: this module
//! carries no money fields, so there is no floating point
//! money anywhere in it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_LABEL_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_CODE_LEN: usize = 128;

pub(crate) const MAX_USER_ID_LEN: usize = 200;

pub(crate) const MAX_GREETING_LEN: usize = 5000;

pub(crate) const MAX_TEMPLATE_LEN: usize = 10000;

/// Default ring timeout in seconds when a queue or the tenant
/// settings do not say otherwise.
pub(crate) const DEFAULT_RING_TIMEOUT_SECS: i64 = 30;

/// Default maximum wait in a queue before overflow, seconds.
pub(crate) const DEFAULT_MAX_WAIT_SECS: i64 = 300;

/// Default maximum call duration in seconds (0 = unlimited).
pub(crate) const DEFAULT_MAX_CALL_DURATION_SECS: i64 = 0;

/// Default maximum participants in one group call.
pub(crate) const DEFAULT_MAX_GROUP_PARTICIPANTS: i64 = 8;

/// Presence entries older than this are stale and read as
/// offline.
pub(crate) const PRESENCE_STALE_SECS: i64 = 90;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_PHONE_VIEW: &str = "phone.view";

pub(crate) const PERM_PHONE_MANAGE: &str = "phone.manage";

/// Hook event names this module emits.
pub(crate) const EVENT_CALL_INITIATED: &str = "phone.call.initiated";

pub(crate) const EVENT_CALL_ANSWERED: &str = "phone.call.answered";

pub(crate) const EVENT_CALL_ENDED: &str = "phone.call.ended";

pub(crate) const EVENT_CALL_MISSED: &str = "phone.call.missed";

pub(crate) const EVENT_CALL_HELD: &str = "phone.call.held";

pub(crate) const EVENT_CALL_RESUMED: &str = "phone.call.resumed";

pub(crate) const EVENT_QUEUE_JOINED: &str = "phone.queue.joined";

pub(crate) const EVENT_QUEUE_ANSWERED: &str = "phone.queue.answered";

pub(crate) const EVENT_RECORDING_STARTED: &str = "phone.recording.started";

pub(crate) const EVENT_RECORDING_STOPPED: &str = "phone.recording.stopped";

pub(crate) const EVENT_SIGNAL_RELAYED: &str = "phone.signal";

pub(crate) const EVENT_PRESENCE_UPDATED: &str = "phone.presence.updated";

pub(crate) const EVENT_IVR_RESOLVED: &str = "phone.ivr.resolved";

pub(crate) const EVENT_CALL_SUMMARIZED: &str = "phone.call.summarized";

/// Hook event names this module listens to.
pub(crate) const EVENT_USER_DELETED: &str = "user.deleted";

/// The AI grounding hook the AI module's runner emits for this
/// module's caller-question capability
/// (`<module>.ai.<capability>_requested`).
pub(crate) const EVENT_AI_ANSWER_REQUESTED: &str = "phone.ai.answer_caller_question_requested";

/// The AI grounding hook for the call-summary capability.
pub(crate) const EVENT_AI_SUMMARIZE_REQUESTED: &str = "phone.ai.summarize_call_requested";

/// Marker written into caller / participant / actor ids when
/// the owning user is deleted. Rows are kept (call logs stay
/// auditable); only the identity is anonymized.
pub(crate) const DELETED_USER_ID: &str = "deleted-user";

// ---------------------------------------------------------------------------
// Pure domain functions (unit-tested below)
// ---------------------------------------------------------------------------

/// Call status machine. Allowed transitions:
/// ringing -> active, held (hold before answer is a no-op in
/// practice, so it is rejected here), ended, missed;
/// active -> held, ended; held -> active, ended. Ended and
/// missed are terminal. Anything else is rejected.
pub fn can_transition(from: CallStatus, to: CallStatus) -> bool {
    use CallStatus::{Active, Ended, Held, Missed, Ringing};
    matches!(
        (from, to),
        (Ringing, Active)
            | (Ringing, Ended)
            | (Ringing, Missed)
            | (Active, Held)
            | (Active, Ended)
            | (Held, Active)
            | (Held, Ended)
    )
}

/// Call duration in whole seconds between the moment the call
/// was answered (`started_at`) and the moment it ended
/// (`ended_at`). `None` when either bound is missing (the call
/// never connected); a negative span (clock skew) clamps to 0.
pub fn compute_duration_secs(
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
) -> Option<i64> {
    let start = started_at?;
    let end = ended_at?;
    Some((end - start).num_seconds().max(0))
}

/// Ring order for one queue: which agents ring, and in which
/// order, for the queue's strategy. Only active members are
/// considered; members are never invented.
/// - `sequential`: active members in `position` order, lowest
///   first; the caller rings them one at a time.
/// - `simultaneous`: every active member at once, in
///   `position` order.
/// - `round_robin`: active members ordered by least recently
///   answered (`last_answered_at`, never-answered first), ties
///   broken by `position`; the head of the list rings next and
///   the rest follow in the same order.
pub fn next_ring_targets(members: &[QueueMember], strategy: RingStrategy) -> Vec<String> {
    let mut active: Vec<&QueueMember> = members.iter().filter(|member| member.active).collect();
    match strategy {
        RingStrategy::Sequential | RingStrategy::Simultaneous => {
            active.sort_by_key(|member| member.position);
        }
        RingStrategy::RoundRobin => {
            active.sort_by(|a, b| {
                a.last_answered_at
                    .cmp(&b.last_answered_at)
                    .then_with(|| a.position.cmp(&b.position))
            });
        }
    }
    active
        .into_iter()
        .map(|member| member.user_id.clone())
        .collect()
}

/// Resolve one IVR key press against a menu's nodes: the node
/// whose `key` matches (case-insensitive for letters, exact
/// for digits and `*` / `#`). `None` when the menu has no node
/// for the key — the caller hears the menu again, it is not an
/// error.
pub fn resolve_ivr_key<'a>(nodes: &'a [IvrNode], key: &str) -> Option<&'a IvrNode> {
    let wanted = key.trim().to_lowercase();
    if wanted.chars().count() != 1 {
        return None;
    }
    nodes.iter().find(|node| node.key.to_lowercase() == wanted)
}

/// True when `pattern` matches `caller_id`. Patterns are data:
/// `*` matches every caller, a trailing `*` matches a prefix
/// (`sales-*` matches `sales-42`), anything else matches
/// exactly. An empty pattern never matches.
pub fn caller_pattern_matches(pattern: &str, caller_id: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return caller_id.starts_with(prefix);
    }
    pattern == caller_id
}

/// Parse an `HH:MM` time of day into minutes since midnight.
/// `None` for anything that is not a valid 24-hour time.
pub fn parse_time_of_day(raw: &str) -> Option<i64> {
    let (hour, minute) = raw.split_once(':')?;
    if hour.len() != 2 || minute.len() != 2 {
        return None;
    }
    let hour: i64 = hour.parse().ok()?;
    let minute: i64 = minute.parse().ok()?;
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) {
        return None;
    }
    Some(hour * 60 + minute)
}

/// True when `now_minutes` (minutes since midnight UTC) falls
/// inside the `[start, end)` window. A window whose end is not
/// after its start wraps past midnight (for example 22:00 to
/// 06:00). `None` bounds mean that side is open: both `None`
/// matches any time.
pub fn in_time_window(start: Option<&str>, end: Option<&str>, now_minutes: i64) -> bool {
    let start_minutes = start.and_then(parse_time_of_day);
    let end_minutes = end.and_then(parse_time_of_day);
    match (start_minutes, end_minutes) {
        (None, None) => true,
        (Some(start), None) => now_minutes >= start,
        (None, Some(end)) => now_minutes < end,
        (Some(start), Some(end)) => {
            if end > start {
                now_minutes >= start && now_minutes < end
            } else {
                // Overnight window: 22:00-06:00 covers 22:00-23:59
                // and 00:00-05:59.
                now_minutes >= start || now_minutes < end
            }
        }
    }
}

/// Evaluate the tenant's routing rules for one incoming call:
/// active rules in `priority` order (lowest first), the first
/// rule whose caller pattern matches and whose time window
/// contains `now_minutes` wins. `None` when no rule matches —
/// the caller falls through to the tenant default (direct call
/// or the default queue), never to an invented target.
pub fn evaluate_routing_rules<'a>(
    rules: &'a [RoutingRule],
    caller_id: &str,
    now_minutes: i64,
) -> Option<&'a RoutingRule> {
    let mut ordered: Vec<&RoutingRule> = rules.iter().filter(|rule| rule.active).collect();
    ordered.sort_by_key(|rule| rule.priority);
    ordered.into_iter().find(|rule| {
        let caller_ok = match &rule.match_caller_id {
            Some(pattern) => caller_pattern_matches(pattern, caller_id),
            None => true,
        };
        caller_ok
            && in_time_window(
                rule.match_time_start.as_deref(),
                rule.match_time_end.as_deref(),
                now_minutes,
            )
    })
}

/// Parse the AI call-summary output into the summary text
/// stored on the call log. The model is asked to return JSON
/// with `summary` (a short paragraph); the output may carry
/// surrounding prose, so the first `{` … last `}` span is
/// parsed first. When no JSON is found, plain-text output is
/// accepted as the summary itself (trimmed, capped at 5000
/// characters). `None` when the output is empty.
pub fn parse_ai_summary(output: &str) -> Option<String> {
    if let (Some(start), Some(end)) = (output.find('{'), output.rfind('}')) {
        if end > start {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&output[start..=end]) {
                if let Some(summary) = parsed.get("summary").and_then(|value| value.as_str()) {
                    let summary = summary.trim();
                    if !summary.is_empty() {
                        return Some(summary.chars().take(5000).collect());
                    }
                }
            }
        }
    }
    let plain = output.trim();
    if plain.is_empty() {
        None
    } else {
        Some(plain.chars().take(5000).collect())
    }
}

/// True when `code` is a valid data-row code: lowercase
/// letters, digits, and single hyphens only, never leading or
/// trailing with a hyphen, at most 128 chars.
pub fn valid_code(code: &str) -> bool {
    if code.is_empty() || code.len() > MAX_CODE_LEN {
        return false;
    }
    if code.starts_with('-') || code.ends_with('-') || code.contains("--") {
        return false;
    }
    code.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// True when `key` is a valid single IVR key: one digit, `*`,
/// or `#` (letters are accepted too and matched
/// case-insensitively by `resolve_ivr_key`).
pub fn valid_ivr_key(key: &str) -> bool {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => c.is_ascii_alphanumeric() || c == '*' || c == '#',
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Call status. Serialized snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    Ringing,
    Active,
    Held,
    Ended,
    Missed,
}

impl Default for CallStatus {
    fn default() -> Self {
        Self::Ringing
    }
}

impl CallStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Ringing => "ringing",
            Self::Active => "active",
            Self::Held => "held",
            Self::Ended => "ended",
            Self::Missed => "missed",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "ringing" => Some(Self::Ringing),
            "active" => Some(Self::Active),
            "held" => Some(Self::Held),
            "ended" => Some(Self::Ended),
            "missed" => Some(Self::Missed),
            _ => None,
        }
    }

    /// Terminal statuses: no transition leaves them.
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Ended | Self::Missed)
    }
}

/// Call kind: voice only, or voice plus video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallKind {
    Voice,
    Video,
}

impl Default for CallKind {
    fn default() -> Self {
        Self::Voice
    }
}

impl CallKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Voice => "voice",
            Self::Video => "video",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "voice" => Some(Self::Voice),
            "video" => Some(Self::Video),
            _ => None,
        }
    }
}

/// Call outcome, recorded when the call ends: how the call
/// finished, for the call log. A call that was answered and
/// talked is `completed`; a call nobody answered is `missed`;
/// a call a callee declined is `declined`; a call ended by
/// the caller while ringing is `cancelled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallOutcome {
    Completed,
    Missed,
    Declined,
    Busy,
    NoAnswer,
    Cancelled,
}

impl CallOutcome {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Missed => "missed",
            Self::Declined => "declined",
            Self::Busy => "busy",
            Self::NoAnswer => "no_answer",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "completed" => Some(Self::Completed),
            "missed" => Some(Self::Missed),
            "declined" => Some(Self::Declined),
            "busy" => Some(Self::Busy),
            "no_answer" => Some(Self::NoAnswer),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// One participant's state inside a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantStatus {
    Invited,
    Ringing,
    Joined,
    Declined,
    Left,
    Missed,
}

impl ParticipantStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Invited => "invited",
            Self::Ringing => "ringing",
            Self::Joined => "joined",
            Self::Declined => "declined",
            Self::Left => "left",
            Self::Missed => "missed",
        }
    }
}

/// Presence status of one user. Serialized snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceStatus {
    Available,
    Busy,
    DoNotDisturb,
    Offline,
}

impl Default for PresenceStatus {
    fn default() -> Self {
        Self::Offline
    }
}

impl PresenceStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Busy => "busy",
            Self::DoNotDisturb => "do_not_disturb",
            Self::Offline => "offline",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "available" => Some(Self::Available),
            "busy" => Some(Self::Busy),
            "do_not_disturb" => Some(Self::DoNotDisturb),
            "offline" => Some(Self::Offline),
            _ => None,
        }
    }
}

/// Queue ring strategy, as data on the queue row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RingStrategy {
    Sequential,
    Simultaneous,
    RoundRobin,
}

impl Default for RingStrategy {
    fn default() -> Self {
        Self::Sequential
    }
}

impl RingStrategy {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Sequential => "sequential",
            Self::Simultaneous => "simultaneous",
            Self::RoundRobin => "round_robin",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "sequential" => Some(Self::Sequential),
            "simultaneous" => Some(Self::Simultaneous),
            "round_robin" => Some(Self::RoundRobin),
            _ => None,
        }
    }
}

/// WebRTC signalling message kind. `ice_candidate` carries one
/// ICE candidate; trickle ICE sends one request per candidate.
/// Reuses the meetings module's vocabulary so clients that
/// already speak meetings signalling need no new shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    Offer,
    Answer,
    IceCandidate,
}

impl SignalKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Offer => "offer",
            Self::Answer => "answer",
            Self::IceCandidate => "ice_candidate",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "offer" => Some(Self::Offer),
            "answer" => Some(Self::Answer),
            "ice_candidate" | "ice-candidate" => Some(Self::IceCandidate),
            _ => None,
        }
    }
}

/// IVR node action: what one key press does. The target is
/// data (a queue code, a user id, or a menu code) — never a
/// hard-coded destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IvrAction {
    TransferQueue,
    TransferUser,
    PlayMessage,
    RunScript,
    GoToMenu,
    RepeatMenu,
    Hangup,
}

impl IvrAction {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::TransferQueue => "transfer_queue",
            Self::TransferUser => "transfer_user",
            Self::PlayMessage => "play_message",
            Self::RunScript => "run_script",
            Self::GoToMenu => "go_to_menu",
            Self::RepeatMenu => "repeat_menu",
            Self::Hangup => "hangup",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "transfer_queue" => Some(Self::TransferQueue),
            "transfer_user" => Some(Self::TransferUser),
            "play_message" => Some(Self::PlayMessage),
            "run_script" => Some(Self::RunScript),
            "go_to_menu" => Some(Self::GoToMenu),
            "repeat_menu" => Some(Self::RepeatMenu),
            "hangup" => Some(Self::Hangup),
            _ => None,
        }
    }
}

/// Recording status. Serialized snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingStatus {
    Recording,
    Stopped,
    Stored,
}

impl RecordingStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::Stopped => "stopped",
            Self::Stored => "stored",
        }
    }
}

/// One voice/video call. `caller_id` initiated it;
/// participants live in `phone_call_participants`.
/// `queue_id` / `ivr_menu_id` record how the call arrived
/// (NULL for a direct call). `recording_consent` gates
/// recording; `summary` is the AI call summary, written after
/// the call ends; `duration_secs` is computed on end.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhoneCall {
    pub id: Uuid,
    pub tenant_id: String,
    pub caller_id: String,
    pub kind: CallKind,
    pub status: CallStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<CallOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ivr_menu_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub recording_consent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_consent_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_model: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One participant of one call. `direction` is relative to
/// the participant: `outbound` for the caller, `inbound` for
/// everyone invited. A participant who never answered ends as
/// `missed`; one who declined ends as `declined`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallParticipant {
    pub id: Uuid,
    pub tenant_id: String,
    pub call_id: Uuid,
    pub user_id: String,
    pub direction: String,
    pub status: ParticipantStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joined_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One stored signalling message (store-and-forward). The
/// server stores it, broadcasts it on the shared WebSocket
/// hub, and keeps it until the recipient fetches it from the
/// signals inbox — so a reconnecting client does not lose an
/// offer or ICE candidate sent while offline. `payload` is
/// the SDP or ICE JSON object, opaque to the server. Media
/// never passes through here: only signalling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalMessage {
    pub id: Uuid,
    pub tenant_id: String,
    pub call_id: Uuid,
    pub sender_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_id: Option<String>,
    pub kind: SignalKind,
    pub payload: serde_json::Value,
    pub delivered: bool,
    pub created_at: DateTime<Utc>,
}

/// One call queue, as data. `ring_strategy` picks the ring
/// order (see `next_ring_targets`); `ring_timeout_secs` bounds
/// one ring attempt; `max_wait_secs` bounds the wait before
/// overflow. `overflow_queue_id` names the queue a caller
/// falls through to when the wait expires (NULL means the
/// caller stays queued); `overflow_action` names what happens
/// with no overflow queue (`ivr`, `hangup`, or a script code).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhoneQueue {
    pub id: Uuid,
    pub tenant_id: String,
    pub code: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub ring_strategy: RingStrategy,
    pub ring_timeout_secs: i64,
    pub max_wait_secs: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overflow_queue_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overflow_action: Option<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One agent on a queue. `position` is the ring order for
/// sequential / simultaneous ringing; `last_answered_at`
/// drives round-robin (least recently answered rings first).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueMember {
    pub id: Uuid,
    pub tenant_id: String,
    pub queue_id: Uuid,
    pub user_id: String,
    pub position: i64,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_answered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One caller waiting in a queue. An admin answering the
/// queue turns the oldest waiting waiter into a call
/// (`call_id`, status `answered`); a waiter who hangs up
/// first is `abandoned`, one whose wait expires is `timed_out`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueWaiter {
    pub id: Uuid,
    pub tenant_id: String,
    pub queue_id: Uuid,
    pub caller_id: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<Uuid>,
    pub joined_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<DateTime<Utc>>,
}

/// One routing rule, as data. Rules are evaluated in
/// `priority` order (lowest first); the first matching rule
/// wins. `match_caller_id` is a caller pattern (`*`, a
/// `prefix*`, or an exact id; NULL matches every caller).
/// `match_time_start` / `match_time_end` are `HH:MM` bounds
/// (NULL means that side is open; an end before the start
/// wraps past midnight). `target_type` is `queue`, `user`, or
/// `ivr_menu`; `target_id` names the target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRule {
    pub id: Uuid,
    pub tenant_id: String,
    pub code: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub priority: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_caller_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_time_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_time_end: Option<String>,
    pub target_type: String,
    pub target_id: String,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One user's presence. `status` is the declared status;
/// `updated_at` is the last heartbeat, and a stale entry
/// reads as `offline` (see `PRESENCE_STALE_SECS`). `note` is
/// a free-text status message, if the user set one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceEntry {
    pub tenant_id: String,
    pub user_id: String,
    pub status: PresenceStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// One IVR menu, as data. `greeting` is the text the caller
/// hears when the menu opens; `greeting_audio_ref` names a
/// documents/storage object by reference when a recorded
/// greeting exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IvrMenu {
    pub id: Uuid,
    pub tenant_id: String,
    pub code: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub greeting: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub greeting_audio_ref: Option<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One key of one IVR menu. `key` is a single digit, `*`, or
/// `#`. `action` says what the press does; `target_id` names
/// the target (queue code, user id, script code, or menu code
/// depending on the action); `message` is the text played for
/// `play_message`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IvrNode {
    pub id: Uuid,
    pub tenant_id: String,
    pub menu_id: Uuid,
    pub key: String,
    pub action: IvrAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One auto-response script, as data: a routine caller
/// question matched by `trigger` (a keyword or phrase, matched
/// case-insensitively as a substring) and answered with
/// `response_template`, or handed to the AI capability named
/// by `ai_capability` when set (the AI answers routine
/// questions and can route or escalate — it never books or
/// charges anything on its own).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoResponseScript {
    pub id: Uuid,
    pub tenant_id: String,
    pub code: String,
    pub label: String,
    pub trigger: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_capability: Option<String>,
    pub priority: i64,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One call recording: metadata only. The bytes live in the
/// storage layer, referenced by `storage_target` /
/// `storage_key` (plus an optional signed `url`) once
/// attached. Recording is consent-gated: a row is only
/// created when the call carries `recording_consent`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallRecording {
    pub id: Uuid,
    pub tenant_id: String,
    pub call_id: Uuid,
    pub started_by: String,
    pub status: RecordingStatus,
    pub consent_given: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<i64>,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Per-tenant phone settings. `recording_requires_consent`
/// defaults to true and is the only switch that can relax the
/// consent gate — it can only be turned off by a manager, and
/// recordings started while it is off still record
/// `consent_given: false` on the row so the gap stays visible.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhoneSettings {
    pub default_ring_timeout_secs: i64,
    pub max_call_duration_secs: i64,
    pub recording_requires_consent: bool,
    pub missed_call_timeout_secs: i64,
    pub allow_group_calls: bool,
    pub max_group_participants: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_queue_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_ivr_menu_code: Option<String>,
}

impl Default for PhoneSettings {
    fn default() -> Self {
        Self {
            default_ring_timeout_secs: DEFAULT_RING_TIMEOUT_SECS,
            max_call_duration_secs: DEFAULT_MAX_CALL_DURATION_SECS,
            recording_requires_consent: true,
            missed_call_timeout_secs: DEFAULT_RING_TIMEOUT_SECS,
            allow_group_calls: true,
            max_group_participants: DEFAULT_MAX_GROUP_PARTICIPANTS,
            default_queue_code: None,
            default_ivr_menu_code: None,
        }
    }
}

// Request bodies live in `requests.rs` (kept separate so this
// file stays under the ~1500 line convention); they are
// re-exported here so handlers keep importing from `types`.
pub use super::requests::{
    AddQueueMemberRequest, AttachRecordingRequest, AutoResponseRequest, CreateIvrMenuRequest,
    CreateIvrNodeRequest, CreateQueueRequest, CreateRoutingRuleRequest, CreateScriptRequest,
    EndCallRequest, EvaluateRoutingRequest, InitiateCallRequest, InviteParticipantRequest,
    ResolveIvrRequest, SetPresenceRequest, SignalRequest, UpdateCallRequest, UpdateIvrMenuRequest,
    UpdateQueueRequest, UpdateRoutingRuleRequest, UpdateScriptRequest, UpdateSettingsRequest,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn member(
        user_id: &str,
        position: i64,
        active: bool,
        last_answered: Option<DateTime<Utc>>,
    ) -> QueueMember {
        QueueMember {
            id: Uuid::new_v4(),
            tenant_id: "tenant-1".to_string(),
            queue_id: Uuid::new_v4(),
            user_id: user_id.to_string(),
            position,
            active,
            last_answered_at: last_answered,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn node(key: &str, action: IvrAction) -> IvrNode {
        IvrNode {
            id: Uuid::new_v4(),
            tenant_id: "tenant-1".to_string(),
            menu_id: Uuid::new_v4(),
            key: key.to_string(),
            action,
            target_id: None,
            message: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn rule(
        code: &str,
        priority: i64,
        match_caller_id: Option<&str>,
        start: Option<&str>,
        end: Option<&str>,
        active: bool,
    ) -> RoutingRule {
        RoutingRule {
            id: Uuid::new_v4(),
            tenant_id: "tenant-1".to_string(),
            code: code.to_string(),
            label: code.to_string(),
            description: None,
            priority,
            match_caller_id: match_caller_id.map(str::to_string),
            match_time_start: start.map(str::to_string),
            match_time_end: end.map(str::to_string),
            target_type: "queue".to_string(),
            target_id: "support".to_string(),
            active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn call_status_machine_transitions() {
        use CallStatus::{Active, Ended, Held, Missed, Ringing};
        assert!(can_transition(Ringing, Active));
        assert!(can_transition(Ringing, Ended));
        assert!(can_transition(Ringing, Missed));
        assert!(!can_transition(Ringing, Held));
        assert!(!can_transition(Ringing, Ringing));
        assert!(can_transition(Active, Held));
        assert!(can_transition(Active, Ended));
        assert!(!can_transition(Active, Missed));
        assert!(!can_transition(Active, Ringing));
        assert!(can_transition(Held, Active));
        assert!(can_transition(Held, Ended));
        assert!(!can_transition(Held, Ringing));
        assert!(!can_transition(Held, Missed));
        assert!(!can_transition(Ended, Active));
        assert!(!can_transition(Missed, Active));
        assert!(Ended.is_terminal());
        assert!(Missed.is_terminal());
        assert!(!Ringing.is_terminal());
        assert!(!Active.is_terminal());
        assert!(!Held.is_terminal());
    }

    #[test]
    fn call_status_round_trips() {
        for (status, text) in [
            (CallStatus::Ringing, "ringing"),
            (CallStatus::Active, "active"),
            (CallStatus::Held, "held"),
            (CallStatus::Ended, "ended"),
            (CallStatus::Missed, "missed"),
        ] {
            assert_eq!(status.as_str(), text);
            assert_eq!(CallStatus::from_str(text), Some(status));
        }
        assert_eq!(CallStatus::from_str("waiting"), None);
    }

    #[test]
    fn duration_computation() {
        let start = Utc::now();
        let end = start + chrono::Duration::seconds(137);
        assert_eq!(compute_duration_secs(Some(start), Some(end)), Some(137));
        assert_eq!(compute_duration_secs(Some(start), Some(start)), Some(0));
        // Clock skew clamps to zero, never negative.
        assert_eq!(compute_duration_secs(Some(end), Some(start)), Some(0));
        // A call that never connected has no duration.
        assert_eq!(compute_duration_secs(None, Some(end)), None);
        assert_eq!(compute_duration_secs(Some(start), None), None);
        assert_eq!(compute_duration_secs(None, None), None);
    }

    #[test]
    fn ring_order_sequential_and_simultaneous() {
        let members = vec![
            member("agent-c", 3, true, None),
            member("agent-a", 1, true, None),
            member("agent-b", 2, true, None),
            member("agent-off", 0, false, None),
        ];
        assert_eq!(
            next_ring_targets(&members, RingStrategy::Sequential),
            vec!["agent-a", "agent-b", "agent-c"]
        );
        assert_eq!(
            next_ring_targets(&members, RingStrategy::Simultaneous),
            vec!["agent-a", "agent-b", "agent-c"]
        );
        assert!(next_ring_targets(&[], RingStrategy::Sequential).is_empty());
    }

    #[test]
    fn ring_order_round_robin_least_recently_answered_first() {
        let earlier = Utc::now() - chrono::Duration::hours(2);
        let later = Utc::now() - chrono::Duration::minutes(5);
        let members = vec![
            member("agent-recent", 1, true, Some(later)),
            member("agent-never", 2, true, None),
            member("agent-older", 3, true, Some(earlier)),
            member("agent-off", 0, false, None),
        ];
        // Never-answered first, then oldest answer, then most recent.
        assert_eq!(
            next_ring_targets(&members, RingStrategy::RoundRobin),
            vec!["agent-never", "agent-older", "agent-recent"]
        );
    }

    #[test]
    fn ivr_key_resolution() {
        let nodes = vec![
            node("1", IvrAction::TransferQueue),
            node("2", IvrAction::TransferUser),
            node("*", IvrAction::RepeatMenu),
            node("#", IvrAction::Hangup),
        ];
        assert_eq!(
            resolve_ivr_key(&nodes, "1").unwrap().action,
            IvrAction::TransferQueue
        );
        assert_eq!(
            resolve_ivr_key(&nodes, "2").unwrap().action,
            IvrAction::TransferUser
        );
        assert_eq!(
            resolve_ivr_key(&nodes, "*").unwrap().action,
            IvrAction::RepeatMenu
        );
        assert_eq!(
            resolve_ivr_key(&nodes, "#").unwrap().action,
            IvrAction::Hangup
        );
        // No node for the key: the caller hears the menu again.
        assert!(resolve_ivr_key(&nodes, "9").is_none());
        assert!(resolve_ivr_key(&nodes, "12").is_none());
        assert!(resolve_ivr_key(&nodes, "").is_none());
        assert!(resolve_ivr_key(&nodes, " ").is_none());
        assert!(resolve_ivr_key(&[], "1").is_none());
    }

    #[test]
    fn caller_pattern_matching() {
        assert!(caller_pattern_matches("*", "anyone"));
        assert!(caller_pattern_matches("sales-*", "sales-42"));
        assert!(!caller_pattern_matches("sales-*", "support-42"));
        assert!(caller_pattern_matches("user-1", "user-1"));
        assert!(!caller_pattern_matches("user-1", "user-2"));
        assert!(!caller_pattern_matches("", "user-1"));
    }

    #[test]
    fn time_of_day_parsing() {
        assert_eq!(parse_time_of_day("00:00"), Some(0));
        assert_eq!(parse_time_of_day("09:30"), Some(570));
        assert_eq!(parse_time_of_day("23:59"), Some(1439));
        assert_eq!(parse_time_of_day("24:00"), None);
        assert_eq!(parse_time_of_day("9:30"), None);
        assert_eq!(parse_time_of_day("09:60"), None);
        assert_eq!(parse_time_of_day("nope"), None);
        assert_eq!(parse_time_of_day(""), None);
    }

    #[test]
    fn time_window_matching() {
        assert!(in_time_window(None, None, 0));
        assert!(in_time_window(None, None, 1439));
        // Daytime window 09:00-17:00.
        assert!(in_time_window(Some("09:00"), Some("17:00"), 540));
        assert!(in_time_window(Some("09:00"), Some("17:00"), 1019));
        assert!(!in_time_window(Some("09:00"), Some("17:00"), 539));
        assert!(!in_time_window(Some("09:00"), Some("17:00"), 1020));
        // Overnight window 22:00-06:00 wraps past midnight.
        assert!(in_time_window(Some("22:00"), Some("06:00"), 1380));
        assert!(in_time_window(Some("22:00"), Some("06:00"), 300));
        assert!(!in_time_window(Some("22:00"), Some("06:00"), 720));
        // One-sided bounds.
        assert!(in_time_window(Some("09:00"), None, 1439));
        assert!(!in_time_window(Some("09:00"), None, 100));
        assert!(in_time_window(None, Some("06:00"), 0));
        assert!(!in_time_window(None, Some("06:00"), 400));
    }

    #[test]
    fn routing_rule_evaluation_priority_and_matching() {
        let rules = vec![
            rule("night", 10, None, Some("22:00"), Some("06:00"), true),
            rule("sales", 5, Some("sales-*"), None, None, true),
            rule("catch-all", 99, None, None, None, true),
            rule("inactive-first", 1, None, None, None, false),
        ];
        // Lowest priority matching rule wins, even when a later
        // rule would also match.
        let hit = evaluate_routing_rules(&rules, "sales-42", 600).unwrap();
        assert_eq!(hit.code, "sales");
        // Night window matches at 02:00; the sales rule does not
        // match this caller, so night wins over catch-all.
        let hit = evaluate_routing_rules(&rules, "user-9", 120).unwrap();
        assert_eq!(hit.code, "night");
        // Daytime, unknown caller: only the catch-all matches.
        let hit = evaluate_routing_rules(&rules, "user-9", 600).unwrap();
        assert_eq!(hit.code, "catch-all");
        // Inactive rules never match.
        assert!(
            evaluate_routing_rules(&[rule("off", 1, None, None, None, false)], "x", 0).is_none()
        );
        assert!(evaluate_routing_rules(&[], "x", 0).is_none());
    }

    #[test]
    fn ai_summary_parsing() {
        let output = r#"{"summary": "The caller asked about opening hours and was told the office opens at 09:00."}"#;
        assert_eq!(
            parse_ai_summary(output).unwrap(),
            "The caller asked about opening hours and was told the office opens at 09:00."
        );
        // Surrounding prose is tolerated.
        let output = "Here is the summary:\n{\"summary\": \"Short call.\"}\nDone.";
        assert_eq!(parse_ai_summary(output).unwrap(), "Short call.");
        // Plain text without JSON is accepted as the summary.
        assert_eq!(
            parse_ai_summary("  A brief call about billing.  ").unwrap(),
            "A brief call about billing."
        );
        assert!(parse_ai_summary("").is_none());
        assert!(parse_ai_summary("   ").is_none());
        assert!(parse_ai_summary("{}").is_none());
        assert!(parse_ai_summary(r#"{"summary": ""}"#).is_none());
    }

    #[test]
    fn code_and_key_validation() {
        assert!(valid_code("support-queue"));
        assert!(valid_code("a1-b2"));
        assert!(!valid_code("Support"));
        assert!(!valid_code("a--b"));
        assert!(!valid_code("-a"));
        assert!(!valid_code("a-"));
        assert!(!valid_code(""));
        assert!(!valid_code("a_b"));
        assert!(valid_ivr_key("1"));
        assert!(valid_ivr_key("*"));
        assert!(valid_ivr_key("#"));
        assert!(!valid_ivr_key("12"));
        assert!(!valid_ivr_key(""));
    }

    #[test]
    fn settings_defaults_are_safe() {
        let settings = PhoneSettings::default();
        assert!(settings.recording_requires_consent);
        assert!(settings.allow_group_calls);
        assert_eq!(
            settings.default_ring_timeout_secs,
            DEFAULT_RING_TIMEOUT_SECS
        );
        assert_eq!(
            settings.max_group_participants,
            DEFAULT_MAX_GROUP_PARTICIPANTS
        );
        assert_eq!(settings.max_call_duration_secs, 0);
    }

    #[test]
    fn enum_round_trips() {
        // One assert pair per variant; from_str is the inverse of as_str.
        for (kind, text) in [(CallKind::Voice, "voice"), (CallKind::Video, "video")] {
            assert_eq!(kind.as_str(), text);
            assert_eq!(CallKind::from_str(text), Some(kind));
        }
        assert_eq!(CallKind::from_str("audio"), None);
        for (outcome, text) in [
            (CallOutcome::Completed, "completed"),
            (CallOutcome::Missed, "missed"),
            (CallOutcome::Declined, "declined"),
            (CallOutcome::Busy, "busy"),
            (CallOutcome::NoAnswer, "no_answer"),
            (CallOutcome::Cancelled, "cancelled"),
        ] {
            assert_eq!(outcome.as_str(), text);
            assert_eq!(CallOutcome::from_str(text), Some(outcome));
        }
        for (status, text) in [
            (PresenceStatus::Available, "available"),
            (PresenceStatus::Busy, "busy"),
            (PresenceStatus::DoNotDisturb, "do_not_disturb"),
            (PresenceStatus::Offline, "offline"),
        ] {
            assert_eq!(status.as_str(), text);
            assert_eq!(PresenceStatus::from_str(text), Some(status));
        }
        for (strategy, text) in [
            (RingStrategy::Sequential, "sequential"),
            (RingStrategy::Simultaneous, "simultaneous"),
            (RingStrategy::RoundRobin, "round_robin"),
        ] {
            assert_eq!(strategy.as_str(), text);
            assert_eq!(RingStrategy::from_str(text), Some(strategy));
        }
        for (kind, text) in [
            (SignalKind::Offer, "offer"),
            (SignalKind::Answer, "answer"),
            (SignalKind::IceCandidate, "ice_candidate"),
        ] {
            assert_eq!(kind.as_str(), text);
            assert_eq!(SignalKind::from_str(text), Some(kind));
        }
        assert_eq!(
            SignalKind::from_str("ice-candidate"),
            Some(SignalKind::IceCandidate)
        );
        for (action, text) in [
            (IvrAction::TransferQueue, "transfer_queue"),
            (IvrAction::TransferUser, "transfer_user"),
            (IvrAction::PlayMessage, "play_message"),
            (IvrAction::RunScript, "run_script"),
            (IvrAction::GoToMenu, "go_to_menu"),
            (IvrAction::RepeatMenu, "repeat_menu"),
            (IvrAction::Hangup, "hangup"),
        ] {
            assert_eq!(action.as_str(), text);
            assert_eq!(IvrAction::from_str(text), Some(action));
        }
    }
}
