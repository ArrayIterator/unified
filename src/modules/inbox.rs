// Smart Inbox module — one queue unifying email, chat, tickets, and
// social mentions, with assignment, snooze, canned replies, SLA
// surfacing, per-agent read tracking, and explicit thread merging.
//
// Routes live under `/api/v1/inbox`:
//
//   Queue:
//     GET    /threads — unified queue (filter by channel, status,
//       assignee, priority, department_id, unread, overdue;
//       overdue threads first, then most recent activity).
//     POST   /threads — register a thread reference manually
//       (email / chat / ticket / social; the owning module keeps
//       the messages — this only stores the reference).
//     GET    /threads/{id}.
//     PATCH  /threads/{id} — status / priority / department.
//     GET    /stats — queue counts by status and channel, overdue
//       count, unassigned count, snoozed count.
//   Assignment / snooze / read:
//     POST   /threads/{id}/assign — assign to an agent.
//     POST   /threads/{id}/snooze — hide until a time, then reappear
//       (scheduler job `inbox.unsnooze`).
//     POST   /threads/{id}/unsnooze — bring a snoozed thread back.
//     POST   /threads/{id}/read — mark read for the caller.
//     POST   /threads/{id}/unread — mark unread for the caller.
//   Merge:
//     POST   /threads/{id}/merge — merge into another thread
//       (explicit only; same customer, multiple channels).
//     POST   /threads/{id}/unmerge — reverse a merge.
//   Reply:
//     POST   /threads/{id}/reply — request a reply from the owning
//       module (the inbox never sends mail or chat itself); an
//       optional canned reply id expands into the draft body.
//   Canned replies:
//     GET/POST /canned-replies — list (filter by department_id) /
//       create (per department).
//     GET/PATCH/DELETE /canned-replies/{id}.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory thread index, assignment history, canned replies, and
// per-agent read receipts; ingestion listeners that maintain the
// index from `ticket.created` / `ticket.updated` /
// `ticket.commented` / `ticket.assigned`, `chat.message` /
// `chat.session.created` / `chat.session.assigned`, and `mail.sent`
// payloads; SLA display copied from the tickets module's own SLA
// fields (`sla_resolution_due_at`, `sla_resolution_breached`) on
// the thread — the tickets module owns SLA computation, this module
// only surfaces it and sorts overdue threads first; assignment with
// a `notification.requested` to the assignee; snooze with a
// `scheduler.enqueue` attempt; reply as a fire-and-forget
// `inbox.reply.requested` event for the owning module. What is NOT
// built here: the message stores themselves (mail, chat, and
// tickets own their messages; a thread holds only a reference:
// source channel, source id, subject, participants, last activity,
// unread flag); actual sending of any reply (the owning module
// consumes `inbox.reply.requested`); inbound email ingestion from
// the mail module (no `mail.received` event exists in this tree —
// `mail.sent` is ingested defensively and skipped when its payload
// carries no message id, so email threads mostly arrive through
// the manual registration endpoint until the mail module emits an
// inbound event); social mention ingestion (no social module exists
// in this tree yet — social threads are registered manually with
// channel `social`); and automatic merging (merging is explicit
// only, by agent choice).
//
// Public-access note: there are no public routes in this module.
// Every route requires the caller headers and a capability check
// (`inbox.view` for reads, `inbox.manage` for writes); the API
// Manager default-deny still applies on top.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not
// allowed).
//
// Events emitted on the hook bus:
//   - `inbox.thread.assigned`, `inbox.thread.snoozed`,
//     `inbox.thread.read`, `inbox.thread.merged`,
//     `inbox.thread.unmerged`, `inbox.thread.created`,
//     `inbox.thread.updated`, `inbox.thread.unsnoozed`
//   - `inbox.reply.requested` (consumed by the owning module:
//     `{"thread_id", "source_channel", "source_id", "body",
//     "requested_by", "department_id", "assignee_id"}`)
//   - `notification.requested` (assignment notice, in the
//     notifications module's payload shape)
//   - `scheduler.enqueue` with `{"kind": "inbox.unsnooze",
//     "payload": {"thread_id", "unsnooze_at"}}` when a thread is
//     snoozed.
// Events this module listens to:
//   - `ticket.created`, `ticket.updated`, `ticket.commented`,
//     `ticket.assigned` — upsert the ticket thread (full ticket
//     JSON payload: id, title, user_id, department_id, priority,
//     status, SLA due / breached flags).
//   - `chat.message` — upsert the chat thread for the channel
//     (payload: channel_id, message_id, author_id, body).
//   - `chat.session.created`, `chat.session.assigned` — upsert the
//     chat thread for the session (payload: session_id,
//     visitor_id, department_id when present).
//   - `mail.sent` — upsert an email thread when the payload
//     carries a message id (`id` / `message_id`); skipped
//     otherwise (see the HONESTY note above).
//   - `inbox.unsnooze_requested` — the event the scheduler emits
//     when an `inbox.unsnooze` job fires, following its naming
//     rule (`backup.run` -> `backup.run_requested`); clears the
//     snooze on the named thread.
//
// Scheduler note: `inbox.unsnooze` is NOT in the scheduler's
// `KNOWN_KINDS` list yet, so today the scheduler answers
// `{"queued": false, "error": "Unknown job kind: ..."}` and the
// thread records `snooze_scheduler_status: "declined"` instead of
// pretending it was scheduled. The snooze is still stored and the
// thread stays hidden until `snoozed_until` passes (the queue list
// hides snoozed threads by default and re-shows them once the time
// passes, and `POST /threads/{id}/unsnooze` brings one back
// manually); the follow-up is to add `"inbox.unsnooze"` to
// `KNOWN_KINDS` in `scheduler.rs` and arm the dispatch so the job
// emits `inbox.unsnooze_requested` with the thread payload.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_SUBJECT_LEN: usize = 500;
const MAX_BODY_LEN: usize = 10_000;
const MAX_TITLE_LEN: usize = 200;
const MAX_ID_LEN: usize = 255;
const MAX_PARTICIPANTS: usize = 50;

/// Capability names checked through the `role.check` hook.
const PERM_INBOX_VIEW: &str = "inbox.view";
const PERM_INBOX_MANAGE: &str = "inbox.manage";

/// Hook event names this module listens to.
const TICKET_CREATED: &str = "ticket.created";
const TICKET_UPDATED: &str = "ticket.updated";
const TICKET_COMMENTED: &str = "ticket.commented";
const TICKET_ASSIGNED: &str = "ticket.assigned";
const CHAT_MESSAGE: &str = "chat.message";
const CHAT_SESSION_CREATED: &str = "chat.session.created";
const CHAT_SESSION_ASSIGNED: &str = "chat.session.assigned";
const MAIL_SENT: &str = "mail.sent";
const UNSNOOZE_REQUESTED: &str = "inbox.unsnooze_requested";

/// Scheduler job kind used to bring a snoozed thread back.
const SCHEDULER_KIND_UNSNOOZE: &str = "inbox.unsnooze";

/// Source channels a thread can come from. `social` threads are
/// registered manually until a social module emits mention events.
const CHANNELS: [&str; 4] = ["email", "chat", "ticket", "social"];
/// Thread statuses. Ticket statuses map onto these on ingest:
/// `open` / `in_progress` -> `open`, `waiting` -> `pending`,
/// `resolved` -> `resolved`, `closed` -> `closed`.
const STATUSES: [&str; 4] = ["open", "pending", "resolved", "closed"];
/// Thread priorities, matching the tickets module's scale.
const PRIORITIES: [&str; 4] = ["low", "normal", "high", "urgent"];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One unified thread: a lightweight reference to a conversation
/// owned by another module. The inbox stores no message bodies —
/// only the reference (source channel + source id), the subject,
/// participants, assignment, SLA snapshot, and read/snooze state.
/// `source_id` is an opaque string because source modules do not
/// share one id type (tickets use string ids, chat uses UUIDs).
/// `unread` is a coarse queue-level flag: set when new activity
/// arrives, cleared when any agent marks the thread read. The
/// per-agent `unread` value returned by the API is computed from
/// the read receipts in `InboxState::reads` and is authoritative
/// for the caller; the stored flag is only the persisted signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxThread {
    pub id: Uuid,
    pub source_channel: String,
    pub source_id: String,
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<String>,
    pub status: String,
    pub priority: String,
    pub unread: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<DateTime<Utc>>,
    /// `not_scheduled`, `queued`, or `declined` (see the scheduler
    /// note in the module header).
    pub snooze_scheduler_status: String,
    /// SLA resolution deadline copied from the tickets module (only
    /// meaningful for `ticket` threads; `None` otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sla_due_at: Option<DateTime<Utc>>,
    pub sla_breached: bool,
    /// When set, this thread was merged into the target thread and
    /// is hidden from the queue by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_into: Option<Uuid>,
    pub last_activity_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl InboxThread {
    /// True when the thread is overdue right now: the SLA deadline
    /// passed (or the tickets module already flagged a breach)
    /// while the thread is still open or pending.
    fn is_overdue(&self, now: DateTime<Utc>) -> bool {
        if self.status != "open" && self.status != "pending" {
            return false;
        }
        if self.sla_breached {
            return true;
        }
        match self.sla_due_at {
            Some(due) => due < now,
            None => false,
        }
    }

    /// True when the thread is snoozed right now.
    fn is_snoozed(&self, now: DateTime<Utc>) -> bool {
        match self.snoozed_until {
            Some(until) => until > now,
            None => false,
        }
    }
}

/// One assignment event for a thread. Only one row per thread is
/// `active` at a time; older rows are the assignment history. The
/// current assignee is mirrored on `InboxThread::assignee_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxAssignment {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub assignee_id: String,
    pub assigned_by: String,
    pub active: bool,
    pub assigned_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One canned reply. `department_id` scopes the reply to a
/// department; `None` means it is available to every department.
/// The body supports `{customer}`, `{subject}`, and `{thread_id}`
/// placeholders, replaced when a reply is requested.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxCannedReply {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    pub title: String,
    pub body: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RegisterThreadRequest {
    pub source_channel: String,
    pub source_id: String,
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_label: Option<String>,
    #[serde(default)]
    pub participants: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sla_due_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateThreadRequest {
    pub status: Option<String>,
    pub priority: Option<String>,
    pub department_id: Option<Option<String>>,
    pub subject: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AssignRequest {
    pub assignee_id: String,
}

#[derive(Debug, Deserialize)]
pub struct SnoozeRequest {
    pub snoozed_until: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct MergeRequest {
    pub target_thread_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct ReplyRequest {
    /// Reply body. Required unless `canned_reply_id` is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Canned reply to expand into the body (placeholders
    /// `{customer}`, `{subject}`, `{thread_id}` are replaced).
    /// When both are given, `body` wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canned_reply_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCannedReplyRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCannedReplyRequest {
    pub department_id: Option<Option<String>>,
    pub title: Option<String>,
    pub body: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the inbox. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000054_inbox`; until that migration is executed,
/// inbox threads do not survive a restart. `reads` maps
/// `"{thread_id}:{user_id}"` to the time that user last marked the
/// thread read; it backs the per-agent `unread` value and the
/// `unread` queue filter.
#[derive(Clone)]
pub struct InboxState {
    pub threads: Arc<DashMap<Uuid, InboxThread>>,
    pub assignments: Arc<DashMap<Uuid, InboxAssignment>>,
    pub canned_replies: Arc<DashMap<Uuid, InboxCannedReply>>,
    pub reads: Arc<DashMap<String, DateTime<Utc>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for InboxState {
    fn default() -> Self {
        Self {
            threads: Arc::new(DashMap::new()),
            assignments: Arc::new(DashMap::new()),
            canned_replies: Arc::new(DashMap::new()),
            reads: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl InboxState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape.
    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "inbox",
                "category": "inbox",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    /// Read receipt key for one agent on one thread.
    fn read_key(thread_id: Uuid, user_id: &str) -> String {
        format!("{thread_id}:{user_id}")
    }

    /// True when `user_id` has not read the thread since its last
    /// activity (no receipt, or a receipt older than last activity).
    fn unread_for(&self, thread: &InboxThread, user_id: &str) -> bool {
        match self.reads.get(&Self::read_key(thread.id, user_id)) {
            Some(receipt) => *receipt.value() < thread.last_activity_at,
            None => true,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the events module.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the events module's capability check).
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
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

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
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

fn valid_channel(channel: &str) -> UnifiedResult<String> {
    let value = channel.trim().to_lowercase();
    if CHANNELS.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(UnifiedError::bad_request(format!(
            "source_channel must be one of: {}",
            CHANNELS.join(", ")
        )))
    }
}

fn valid_status(status: &str) -> UnifiedResult<String> {
    let value = status.trim().to_lowercase();
    if STATUSES.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(UnifiedError::bad_request(format!(
            "status must be one of: {}",
            STATUSES.join(", ")
        )))
    }
}

fn valid_priority(priority: &str) -> UnifiedResult<String> {
    let value = priority.trim().to_lowercase();
    if PRIORITIES.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(UnifiedError::bad_request(format!(
            "priority must be one of: {}",
            PRIORITIES.join(", ")
        )))
    }
}

/// Map a tickets-module status onto an inbox status.
fn ticket_status_to_inbox(status: &str) -> &'static str {
    match status {
        "waiting" => "pending",
        "resolved" => "resolved",
        "closed" => "closed",
        _ => "open",
    }
}

fn load_thread(state: &InboxState, raw_id: &str) -> UnifiedResult<InboxThread> {
    let id = parse_uuid(raw_id, "thread")?;
    state
        .threads
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Thread not found: {id}")))
}

fn load_canned_reply(state: &InboxState, raw_id: &str) -> UnifiedResult<InboxCannedReply> {
    let id = parse_uuid(raw_id, "canned reply")?;
    state
        .canned_replies
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Canned reply not found: {id}")))
}

/// Find the live (not merged) thread for one source reference.
fn find_thread_for_source(state: &InboxState, channel: &str, source_id: &str) -> Option<InboxThread> {
    state
        .threads
        .iter()
        .map(|entry| entry.value().clone())
        .find(|thread| {
            thread.source_channel == channel
                && thread.source_id == source_id
                && thread.merged_into.is_none()
        })
}

/// Serialize a thread for the API, with per-caller read state and
/// SLA / snooze flags computed at read time.
fn thread_view(state: &InboxState, thread: &InboxThread, caller_id: &str) -> Value {
    let now = Utc::now();
    let mut value = serde_json::to_value(thread).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert(
            "unread".to_string(),
            Value::Bool(state.unread_for(thread, caller_id)),
        );
        map.insert(
            "overdue".to_string(),
            Value::Bool(thread.is_overdue(now)),
        );
        map.insert(
            "snoozed".to_string(),
            Value::Bool(thread.is_snoozed(now)),
        );
        let merged_children: Vec<String> = state
            .threads
            .iter()
            .filter(|entry| entry.value().merged_into == Some(thread.id))
            .map(|entry| entry.key().to_string())
            .collect();
        map.insert(
            "merged_thread_ids".to_string(),
            serde_json::to_value(merged_children).unwrap_or(Value::Null),
        );
    }
    value
}

fn assignments_for(state: &InboxState, thread_id: Uuid) -> Vec<InboxAssignment> {
    let mut items: Vec<InboxAssignment> = state
        .assignments
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|assignment| assignment.thread_id == thread_id)
        .collect();
    items.sort_by_key(|assignment| std::cmp::Reverse(assignment.assigned_at));
    items
}

// ---------------------------------------------------------------------------
// Ingestion — maintain the thread index from other modules' events
// ---------------------------------------------------------------------------

/// Create or refresh the thread for one source reference. Never
/// duplicates message stores: only the reference row is kept. New
/// activity marks the thread unread and refreshes `last_activity_at`.
/// `sla_due_at` / `sla_breached` are only meaningful for ticket
/// threads; other channels pass `None` / `false` and leave a stored
/// SLA value untouched when the caller passes `None`.
#[allow(clippy::too_many_arguments)]
fn upsert_thread(
    state: &InboxState,
    channel: &str,
    source_id: &str,
    subject: &str,
    customer_id: Option<String>,
    customer_label: Option<String>,
    participants: Vec<String>,
    department_id: Option<String>,
    priority: Option<String>,
    status: &str,
    sla_due_at: Option<DateTime<Utc>>,
    sla_breached: bool,
    activity_at: DateTime<Utc>,
) -> Uuid {
    let now = Utc::now();
    let subject = subject.trim();
    let subject = if subject.is_empty() {
        format!("{channel} thread {source_id}")
    } else if subject.len() > MAX_SUBJECT_LEN {
        subject.chars().take(MAX_SUBJECT_LEN).collect()
    } else {
        subject.to_string()
    };
    let mut participants: Vec<String> = participants
        .into_iter()
        .map(|entry| entry.trim().to_string())
        .filter(|entry| !entry.is_empty())
        .collect();
    participants.truncate(MAX_PARTICIPANTS);
    if let Some(existing) = find_thread_for_source(state, channel, source_id) {
        let mut updated = existing.clone();
        updated.subject = subject;
        if let Some(customer) = customer_id {
            updated.customer_id = Some(customer);
        }
        if let Some(label) = customer_label {
            updated.customer_label = Some(label);
        }
        for participant in participants {
            if !updated.participants.contains(&participant) {
                updated.participants.push(participant);
            }
        }
        updated.participants.truncate(MAX_PARTICIPANTS);
        if let Some(department) = department_id {
            updated.department_id = Some(department);
        }
        if let Some(priority) = priority {
            updated.priority = priority;
        }
        updated.status = status.to_string();
        if sla_due_at.is_some() {
            updated.sla_due_at = sla_due_at;
        }
        updated.sla_breached = sla_breached;
        updated.unread = true;
        updated.last_activity_at = activity_at.max(updated.last_activity_at);
        updated.updated_at = now;
        let id = updated.id;
        state.threads.insert(id, updated);
        id
    } else {
        let thread = InboxThread {
            id: Uuid::now_v7(),
            source_channel: channel.to_string(),
            source_id: source_id.to_string(),
            subject,
            customer_id,
            customer_label,
            participants,
            department_id,
            assignee_id: None,
            status: status.to_string(),
            priority: priority.unwrap_or_else(|| "normal".to_string()),
            unread: true,
            snoozed_until: None,
            snooze_scheduler_status: "not_scheduled".to_string(),
            sla_due_at,
            sla_breached,
            merged_into: None,
            last_activity_at: activity_at,
            created_at: now,
            updated_at: now,
        };
        let id = thread.id;
        state.threads.insert(id, thread.clone());
        state.emit(
            "inbox.thread.created",
            json!({
                "thread_id": id.to_string(),
                "source_channel": channel,
                "source_id": source_id,
                "subject": thread.subject,
            }),
        );
        id
    }
}

fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn opt_datetime(data: &Value, key: &str) -> Option<DateTime<Utc>> {
    data.get(key)
        .and_then(Value::as_str)
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|value| value.with_timezone(&Utc))
}

/// Ingest a ticket event payload (full ticket JSON from the tickets
/// module: id, title, user_id, department_id, priority, status,
/// sla_resolution_due_at, sla_resolution_breached, updated_at).
fn on_ticket_event(state: InboxState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(source_id) = opt_string(&data, "id") else {
        return;
    };
    let subject = opt_string(&data, "title").unwrap_or_else(|| format!("Ticket {source_id}"));
    let status = opt_string(&data, "status")
        .map(|value| ticket_status_to_inbox(&value).to_string())
        .unwrap_or_else(|| "open".to_string());
    let priority = opt_string(&data, "priority")
        .filter(|value| PRIORITIES.contains(&value.as_str()));
    let customer_id = opt_string(&data, "user_id");
    let participants = customer_id.clone().into_iter().collect();
    let activity_at = opt_datetime(&data, "updated_at").unwrap_or_else(Utc::now);
    upsert_thread(
        &state,
        "ticket",
        &source_id,
        &subject,
        customer_id,
        None,
        participants,
        opt_string(&data, "department_id"),
        priority,
        &status,
        opt_datetime(&data, "sla_resolution_due_at"),
        data.get("sla_resolution_breached")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        activity_at,
    );
}

/// Ingest a chat message event (payload: channel_id, message_id,
/// author_id, body). The subject is the first line of the latest
/// message when the thread is new; later messages only refresh
/// activity and participants.
fn on_chat_message(state: InboxState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(channel_id) = opt_string(&data, "channel_id") else {
        return;
    };
    let body = opt_string(&data, "body").unwrap_or_default();
    let first_line = body.lines().next().unwrap_or("").trim();
    let subject = if first_line.is_empty() {
        format!("Chat channel {channel_id}")
    } else {
        first_line.to_string()
    };
    let author = opt_string(&data, "author_id");
    let participants = author.into_iter().collect();
    upsert_thread(
        &state,
        "chat",
        &channel_id,
        &subject,
        None,
        None,
        participants,
        None,
        None,
        "open",
        None,
        false,
        Utc::now(),
    );
}

/// Ingest a chat session event (payload: session_id, visitor_id,
/// department_id when present).
fn on_chat_session(state: InboxState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(session_id) = opt_string(&data, "session_id") else {
        return;
    };
    let visitor = opt_string(&data, "visitor_id");
    let subject = match &visitor {
        Some(visitor) => format!("Chat session with {visitor}"),
        None => format!("Chat session {session_id}"),
    };
    let participants = visitor.into_iter().collect();
    upsert_thread(
        &state,
        "chat",
        &session_id,
        &subject,
        None,
        None,
        participants,
        opt_string(&data, "department_id"),
        None,
        "open",
        None,
        false,
        Utc::now(),
    );
}

/// Ingest an outbound mail event. The mail module emits no inbound
/// event in this tree, so this only indexes mail the application
/// sent; the payload shape is read defensively (`id` /
/// `message_id`, `to`, `subject`) and the event is skipped when no
/// message id is present.
fn on_mail_sent(state: InboxState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(source_id) = opt_string(&data, "id").or_else(|| opt_string(&data, "message_id"))
    else {
        return;
    };
    let subject = opt_string(&data, "subject")
        .unwrap_or_else(|| format!("Email {source_id}"));
    let recipient = opt_string(&data, "to");
    let participants = recipient.clone().into_iter().collect();
    upsert_thread(
        &state,
        "email",
        &source_id,
        &subject,
        None,
        recipient,
        participants,
        None,
        None,
        "open",
        None,
        false,
        Utc::now(),
    );
}

/// Clear the snooze on the named thread when the scheduler fires
/// the `inbox.unsnooze` job. Idempotent: a thread that is not
/// snoozed, or is snoozed until later, is left alone.
fn on_unsnooze_requested(state: InboxState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(raw_id) = opt_string(&data, "thread_id") else {
        return;
    };
    let Ok(id) = Uuid::parse_str(&raw_id) else {
        return;
    };
    let Some(mut thread) = state.threads.get_mut(&id) else {
        return;
    };
    if thread.snoozed_until.is_none() {
        return;
    }
    thread.snoozed_until = None;
    thread.snooze_scheduler_status = "not_scheduled".to_string();
    thread.updated_at = Utc::now();
    let thread_id = thread.id;
    drop(thread);
    state.emit(
        "inbox.thread.unsnoozed",
        json!({ "thread_id": thread_id.to_string(), "reason": "scheduler" }),
    );
}

/// Queue the `inbox.unsnooze` scheduler job for a snoozed thread.
/// Mirrors the events module's `schedule_reminder`: the scheduler
/// answers `{"queued": true, "job_id": ...}` when it knows the
/// kind, and `{"queued": false, "error": ...}` today, because
/// `inbox.unsnooze` is not in `KNOWN_KINDS` yet. The snooze is
/// stored either way; the returned status records the outcome
/// honestly (`queued` / `declined`).
fn schedule_unsnooze(state: &InboxState, thread: &mut InboxThread) -> String {
    let Some(bus) = state.bus() else {
        return "declined".to_string();
    };
    let Some(unsnooze_at) = thread.snoozed_until else {
        return "not_scheduled".to_string();
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": SCHEDULER_KIND_UNSNOOZE,
            "payload": {
                "thread_id": thread.id.to_string(),
                "unsnooze_at": unsnooze_at.to_rfc3339(),
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            "queued".to_string()
        }
        _ => "declined".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct InboxModule {
    state: InboxState,
}

impl InboxModule {
    pub fn new() -> Self {
        Self {
            state: InboxState::default(),
        }
    }
}

impl Default for InboxModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for InboxModule {
    fn id(&self) -> &'static str {
        "inbox"
    }

    fn name(&self) -> &'static str {
        "Smart Inbox"
    }

    fn description(&self) -> &'static str {
        "One unified queue over email, chat, tickets, and social mentions: a lightweight thread index (references only — the owning modules keep the messages), assignment, snooze with scheduler re-appearance, per-department canned replies, reply requests emitted to the owning module, SLA surfacing from tickets with overdue threads first, per-agent read tracking, and explicit thread merging."
    }

    fn requires(&self) -> &[&'static str] {
        &["tickets", "chat", "mail", "notifications", "scheduler", "roles", "users"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["tickets", "chat", "mail", "notifications", "scheduler", "roles", "users"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
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
        for event in [TICKET_CREATED, TICKET_UPDATED, TICKET_COMMENTED, TICKET_ASSIGNED] {
            let state = self.state.clone();
            bus.register(
                event,
                "inbox",
                Arc::new(move |ctx: &mut HookContext| on_ticket_event(state.clone(), ctx)),
            );
        }
        let state = self.state.clone();
        bus.register(
            CHAT_MESSAGE,
            "inbox",
            Arc::new(move |ctx: &mut HookContext| on_chat_message(state.clone(), ctx)),
        );
        for event in [CHAT_SESSION_CREATED, CHAT_SESSION_ASSIGNED] {
            let state = self.state.clone();
            bus.register(
                event,
                "inbox",
                Arc::new(move |ctx: &mut HookContext| on_chat_session(state.clone(), ctx)),
            );
        }
        let state = self.state.clone();
        bus.register(
            MAIL_SENT,
            "inbox",
            Arc::new(move |ctx: &mut HookContext| on_mail_sent(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            UNSNOOZE_REQUESTED,
            "inbox",
            Arc::new(move |ctx: &mut HookContext| on_unsnooze_requested(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/inbox")
                .app_data(state)
                .route("/threads", web::get().to(list_threads))
                .route("/threads", web::post().to(register_thread))
                .route("/stats", web::get().to(queue_stats))
                .route("/threads/{id}", web::get().to(get_thread))
                .route("/threads/{id}", web::patch().to(update_thread))
                .route("/threads/{id}/assign", web::post().to(assign_thread))
                .route("/threads/{id}/snooze", web::post().to(snooze_thread))
                .route("/threads/{id}/unsnooze", web::post().to(unsnooze_thread))
                .route("/threads/{id}/read", web::post().to(mark_read))
                .route("/threads/{id}/unread", web::post().to(mark_unread))
                .route("/threads/{id}/merge", web::post().to(merge_thread))
                .route("/threads/{id}/unmerge", web::post().to(unmerge_thread))
                .route("/threads/{id}/reply", web::post().to(request_reply))
                .route("/canned-replies", web::get().to(list_canned_replies))
                .route("/canned-replies", web::post().to(create_canned_reply))
                .route("/canned-replies/{id}", web::get().to(get_canned_reply))
                .route("/canned-replies/{id}", web::patch().to(update_canned_reply))
                .route("/canned-replies/{id}", web::delete().to(delete_canned_reply)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/inbox/threads", owner: "inbox", description: "List the unified queue (paginated; filter by channel, status, assignee, priority, department_id, unread, overdue; overdue first, then most recent activity; snoozed threads hidden unless include_snoozed=true)." },
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads", owner: "inbox", description: "Register a thread reference manually (email / chat / ticket / social; the owning module keeps the messages)." },
            RouteInfo { method: "GET", path: "/api/v1/inbox/stats", owner: "inbox", description: "Queue counts by status and channel, overdue count, unassigned count, snoozed count." },
            RouteInfo { method: "GET", path: "/api/v1/inbox/threads/{id}", owner: "inbox", description: "Get one thread with per-caller read state, overdue / snoozed flags, merged thread ids, and assignment history." },
            RouteInfo { method: "PATCH", path: "/api/v1/inbox/threads/{id}", owner: "inbox", description: "Update a thread's status, priority, department, or subject." },
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads/{id}/assign", owner: "inbox", description: "Assign a thread to an agent (emits inbox.thread.assigned and notifies the assignee)." },
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads/{id}/snooze", owner: "inbox", description: "Snooze a thread until a time (emits inbox.thread.snoozed; queues an inbox.unsnooze scheduler job)." },
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads/{id}/unsnooze", owner: "inbox", description: "Bring a snoozed thread back into the queue now." },
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads/{id}/read", owner: "inbox", description: "Mark a thread read for the caller (emits inbox.thread.read)." },
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads/{id}/unread", owner: "inbox", description: "Mark a thread unread for the caller." },
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads/{id}/merge", owner: "inbox", description: "Merge a thread into another thread (explicit only; the source thread is hidden from the queue)." },
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads/{id}/unmerge", owner: "inbox", description: "Reverse a merge, returning the thread to the queue." },
            RouteInfo { method: "POST", path: "/api/v1/inbox/threads/{id}/reply", owner: "inbox", description: "Request a reply from the owning module (emits inbox.reply.requested; the inbox never sends mail or chat itself). An optional canned reply id expands into the draft body." },
            RouteInfo { method: "GET", path: "/api/v1/inbox/canned-replies", owner: "inbox", description: "List canned replies (paginated, filter by department_id)." },
            RouteInfo { method: "POST", path: "/api/v1/inbox/canned-replies", owner: "inbox", description: "Create a canned reply (per department; null department means every department)." },
            RouteInfo { method: "GET", path: "/api/v1/inbox/canned-replies/{id}", owner: "inbox", description: "Get one canned reply." },
            RouteInfo { method: "PATCH", path: "/api/v1/inbox/canned-replies/{id}", owner: "inbox", description: "Update a canned reply." },
            RouteInfo { method: "DELETE", path: "/api/v1/inbox/canned-replies/{id}", owner: "inbox", description: "Delete a canned reply." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(InboxModule::new()));
}

// ---------------------------------------------------------------------------
// Queue handlers
// ---------------------------------------------------------------------------

fn priority_rank(priority: &str) -> u8 {
    match priority {
        "urgent" => 0,
        "high" => 1,
        "normal" => 2,
        _ => 3,
    }
}

async fn list_threads(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_VIEW)?;
        let params = query.into_inner();
        let channel_filter = params.filters.get("channel").cloned();
        let status_filter = params.filters.get("status").cloned();
        let assignee_filter = params.filters.get("assignee").cloned();
        let priority_filter = params.filters.get("priority").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let unread_filter = params.filters.get("unread").map(|raw| raw == "true");
        let overdue_filter = params.filters.get("overdue").map(|raw| raw == "true");
        let include_snoozed = params.filters.get("include_snoozed").map(|raw| raw == "true").unwrap_or(false);
        let include_merged = params.filters.get("merged").map(|raw| raw == "true").unwrap_or(false);
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let now = Utc::now();
        let mut items: Vec<Value> = state
            .threads
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|thread| {
                if !include_merged && thread.merged_into.is_some() {
                    return false;
                }
                if !include_snoozed && thread.is_snoozed(now) {
                    return false;
                }
                if let Some(channel) = &channel_filter {
                    if thread.source_channel != *channel {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if thread.status != *status {
                        return false;
                    }
                }
                if let Some(assignee) = &assignee_filter {
                    if assignee == "unassigned" {
                        if thread.assignee_id.is_some() {
                            return false;
                        }
                    } else if thread.assignee_id.as_deref() != Some(assignee.as_str()) {
                        return false;
                    }
                }
                if let Some(priority) = &priority_filter {
                    if thread.priority != *priority {
                        return false;
                    }
                }
                if let Some(department) = &department_filter {
                    if thread.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if let Some(want_unread) = unread_filter {
                    if state.unread_for(thread, &caller.actor) != want_unread {
                        return false;
                    }
                }
                if let Some(want_overdue) = overdue_filter {
                    if thread.is_overdue(now) != want_overdue {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {} {}",
                        thread.subject,
                        thread.source_id,
                        thread.customer_id.clone().unwrap_or_default(),
                        thread.customer_label.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|thread| thread_view(&state, &thread, &caller.actor))
            .collect();
        // Default order: overdue threads first, then most recent
        // activity. An explicit ?sort= overrides it.
        let sort = params.sort.clone().unwrap_or_else(|| "overdue".to_string());
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "created_at" => a
                    .get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("created_at").and_then(Value::as_str)),
                "sla_due_at" => a
                    .get("sla_due_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("sla_due_at").and_then(Value::as_str)),
                "priority" => priority_rank(
                    a.get("priority").and_then(Value::as_str).unwrap_or("normal"),
                )
                .cmp(&priority_rank(
                    b.get("priority").and_then(Value::as_str).unwrap_or("normal"),
                )),
                "overdue" => {
                    let overdue_a = a.get("overdue").and_then(Value::as_bool).unwrap_or(false);
                    let overdue_b = b.get("overdue").and_then(Value::as_bool).unwrap_or(false);
                    overdue_b
                        .cmp(&overdue_a)
                        .then(
                            b.get("last_activity_at")
                                .and_then(Value::as_str)
                                .cmp(&a.get("last_activity_at").and_then(Value::as_str)),
                        )
                }
                _ => b
                    .get("last_activity_at")
                    .and_then(Value::as_str)
                    .cmp(&a.get("last_activity_at").and_then(Value::as_str)),
            };
            if sort == "overdue" || sort == "last_activity" {
                ordering
            } else if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn register_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    body: web::Json<RegisterThreadRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let input = body.into_inner();
        let channel = valid_channel(&input.source_channel)?;
        let source_id = require_text(&input.source_id, "source_id", MAX_ID_LEN)?;
        let subject = require_text(&input.subject, "subject", MAX_SUBJECT_LEN)?;
        let mut participants: Vec<String> = Vec::new();
        for raw in &input.participants {
            if participants.len() >= MAX_PARTICIPANTS {
                return Err(UnifiedError::bad_request(format!(
                    "at most {MAX_PARTICIPANTS} participants are allowed"
                )));
            }
            participants.push(require_text(raw, "participant", MAX_ID_LEN)?);
        }
        let status = match &input.status {
            Some(raw) => valid_status(raw)?,
            None => "open".to_string(),
        };
        let priority = match &input.priority {
            Some(raw) => valid_priority(raw)?,
            None => "normal".to_string(),
        };
        let customer_id = optional_text(&input.customer_id, "customer_id", MAX_ID_LEN)?;
        let customer_label = optional_text(&input.customer_label, "customer_label", MAX_ID_LEN)?;
        let department_id = optional_text(&input.department_id, "department_id", MAX_ID_LEN)?;
        let assignee_id = optional_text(&input.assignee_id, "assignee_id", MAX_ID_LEN)?;
        if find_thread_for_source(&state, &channel, &source_id).is_some() {
            return Err(UnifiedError::conflict(format!(
                "A live thread already exists for {channel} source \"{source_id}\""
            )));
        }
        let now = Utc::now();
        let thread = InboxThread {
            id: Uuid::now_v7(),
            source_channel: channel.clone(),
            source_id: source_id.clone(),
            subject,
            customer_id,
            customer_label,
            participants,
            department_id,
            assignee_id,
            status,
            priority,
            unread: true,
            snoozed_until: None,
            snooze_scheduler_status: "not_scheduled".to_string(),
            sla_due_at: input.sla_due_at,
            sla_breached: false,
            merged_into: None,
            last_activity_at: now,
            created_at: now,
            updated_at: now,
        };
        let id = thread.id;
        state.threads.insert(id, thread.clone());
        state.emit(
            "inbox.thread.created",
            json!({
                "thread_id": id.to_string(),
                "source_channel": channel,
                "source_id": source_id,
                "subject": thread.subject,
                "created_by": caller.actor,
            }),
        );
        Ok(thread_view(&state, &thread, &caller.actor))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn queue_stats(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_VIEW)?;
        let now = Utc::now();
        let live: Vec<InboxThread> = state
            .threads
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|thread| thread.merged_into.is_none())
            .collect();
        let mut by_status = serde_json::Map::new();
        for status in STATUSES {
            let count = live.iter().filter(|thread| thread.status == status).count();
            by_status.insert(status.to_string(), Value::from(count as u64));
        }
        let mut by_channel = serde_json::Map::new();
        for channel in CHANNELS {
            let count = live.iter().filter(|thread| thread.source_channel == channel).count();
            by_channel.insert(channel.to_string(), Value::from(count as u64));
        }
        Ok(json!({
            "total": live.len() as u64,
            "by_status": by_status,
            "by_channel": by_channel,
            "overdue": live.iter().filter(|thread| thread.is_overdue(now)).count() as u64,
            "unassigned": live.iter().filter(|thread| thread.assignee_id.is_none() && (thread.status == "open" || thread.status == "pending")).count() as u64,
            "snoozed": live.iter().filter(|thread| thread.is_snoozed(now)).count() as u64,
            "unread_for_caller": live.iter().filter(|thread| state.unread_for(thread, &caller.actor)).count() as u64,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_VIEW)?;
        let thread = load_thread(&state, &path)?;
        let mut view = thread_view(&state, &thread, &caller.actor);
        if let Value::Object(map) = &mut view {
            map.insert(
                "assignments".to_string(),
                serde_json::to_value(assignments_for(&state, thread.id)).unwrap_or(Value::Null),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateThreadRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let input = body.into_inner();
        let mut thread = load_thread(&state, &path)?;
        if thread.merged_into.is_some() {
            return Err(UnifiedError::conflict(
                "A merged thread cannot be updated; unmerge it first",
            ));
        }
        if let Some(raw) = &input.status {
            thread.status = valid_status(raw)?;
        }
        if let Some(raw) = &input.priority {
            thread.priority = valid_priority(raw)?;
        }
        if let Some(department) = &input.department_id {
            thread.department_id = match department {
                Some(raw) => Some(require_text(raw, "department_id", MAX_ID_LEN)?),
                None => None,
            };
        }
        if let Some(subject) = &input.subject {
            thread.subject = require_text(subject, "subject", MAX_SUBJECT_LEN)?;
        }
        thread.updated_at = Utc::now();
        state.threads.insert(thread.id, thread.clone());
        state.emit(
            "inbox.thread.updated",
            json!({
                "thread_id": thread.id.to_string(),
                "status": thread.status,
                "priority": thread.priority,
                "updated_by": caller.actor,
            }),
        );
        Ok(thread_view(&state, &thread, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn assign_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssignRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let assignee = require_text(&body.assignee_id, "assignee_id", MAX_ID_LEN)?;
        let mut thread = load_thread(&state, &path)?;
        if thread.merged_into.is_some() {
            return Err(UnifiedError::conflict(
                "A merged thread cannot be assigned; unmerge it first",
            ));
        }
        let now = Utc::now();
        for mut entry in state.assignments.iter_mut() {
            if entry.value().thread_id == thread.id && entry.value().active {
                entry.value_mut().active = false;
                entry.value_mut().updated_at = now;
            }
        }
        let assignment = InboxAssignment {
            id: Uuid::now_v7(),
            thread_id: thread.id,
            assignee_id: assignee.clone(),
            assigned_by: caller.actor.clone(),
            active: true,
            assigned_at: now,
            created_at: now,
            updated_at: now,
        };
        state.assignments.insert(assignment.id, assignment);
        thread.assignee_id = Some(assignee.clone());
        thread.updated_at = now;
        state.threads.insert(thread.id, thread.clone());
        state.emit(
            "inbox.thread.assigned",
            json!({
                "thread_id": thread.id.to_string(),
                "source_channel": thread.source_channel,
                "source_id": thread.source_id,
                "assignee_id": assignee,
                "assigned_by": caller.actor,
            }),
        );
        state.notify(
            "inbox.thread.assigned",
            &assignee,
            &format!("Thread assigned: {}", thread.subject),
            &format!(
                "You were assigned the {} thread \"{}\" ({}).",
                thread.source_channel, thread.subject, thread.source_id
            ),
            Some(format!("/api/v1/inbox/threads/{}", thread.id)),
        );
        Ok(thread_view(&state, &thread, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn snooze_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SnoozeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let mut thread = load_thread(&state, &path)?;
        if thread.merged_into.is_some() {
            return Err(UnifiedError::conflict(
                "A merged thread cannot be snoozed; unmerge it first",
            ));
        }
        if body.snoozed_until <= Utc::now() {
            return Err(UnifiedError::bad_request(
                "snoozed_until must be in the future",
            ));
        }
        thread.snoozed_until = Some(body.snoozed_until);
        thread.snooze_scheduler_status = schedule_unsnooze(&state, &mut thread);
        thread.updated_at = Utc::now();
        state.threads.insert(thread.id, thread.clone());
        state.emit(
            "inbox.thread.snoozed",
            json!({
                "thread_id": thread.id.to_string(),
                "snoozed_until": body.snoozed_until.to_rfc3339(),
                "snoozed_by": caller.actor,
                "scheduler_status": thread.snooze_scheduler_status,
            }),
        );
        Ok(thread_view(&state, &thread, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn unsnooze_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let mut thread = load_thread(&state, &path)?;
        thread.snoozed_until = None;
        thread.snooze_scheduler_status = "not_scheduled".to_string();
        thread.updated_at = Utc::now();
        state.threads.insert(thread.id, thread.clone());
        state.emit(
            "inbox.thread.unsnoozed",
            json!({
                "thread_id": thread.id.to_string(),
                "reason": "manual",
                "unsnoozed_by": caller.actor,
            }),
        );
        Ok(thread_view(&state, &thread, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn mark_read(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_VIEW)?;
        let mut thread = load_thread(&state, &path)?;
        state.reads.insert(
            InboxState::read_key(thread.id, &caller.actor),
            Utc::now(),
        );
        thread.unread = false;
        thread.updated_at = Utc::now();
        state.threads.insert(thread.id, thread.clone());
        state.emit(
            "inbox.thread.read",
            json!({
                "thread_id": thread.id.to_string(),
                "user_id": caller.actor,
            }),
        );
        Ok(thread_view(&state, &thread, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn mark_unread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_VIEW)?;
        let mut thread = load_thread(&state, &path)?;
        state
            .reads
            .remove(&InboxState::read_key(thread.id, &caller.actor));
        thread.unread = true;
        thread.updated_at = Utc::now();
        state.threads.insert(thread.id, thread.clone());
        Ok(thread_view(&state, &thread, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn merge_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MergeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let mut source = load_thread(&state, &path)?;
        let target = load_thread(&state, &body.target_thread_id.to_string())?;
        if source.id == target.id {
            return Err(UnifiedError::bad_request("A thread cannot be merged into itself"));
        }
        if source.merged_into.is_some() {
            return Err(UnifiedError::conflict("This thread is already merged into another thread"));
        }
        if target.merged_into.is_some() {
            return Err(UnifiedError::conflict(
                "The target thread is itself merged; merge into the live target instead",
            ));
        }
        source.merged_into = Some(target.id);
        source.status = "closed".to_string();
        source.updated_at = Utc::now();
        state.threads.insert(source.id, source.clone());
        // The target inherits the source's participants and takes the
        // latest activity, so the merged view stays complete.
        let mut target = target;
        for participant in &source.participants {
            if !target.participants.contains(participant) {
                target.participants.push(participant);
            }
        }
        target.participants.truncate(MAX_PARTICIPANTS);
        if source.last_activity_at > target.last_activity_at {
            target.last_activity_at = source.last_activity_at;
        }
        target.unread = true;
        target.updated_at = Utc::now();
        state.threads.insert(target.id, target.clone());
        state.emit(
            "inbox.thread.merged",
            json!({
                "thread_id": source.id.to_string(),
                "target_thread_id": target.id.to_string(),
                "merged_by": caller.actor,
            }),
        );
        Ok(thread_view(&state, &target, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn unmerge_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let mut thread = load_thread(&state, &path)?;
        let Some(target_id) = thread.merged_into else {
            return Err(UnifiedError::conflict("This thread is not merged"));
        };
        thread.merged_into = None;
        thread.status = "open".to_string();
        thread.updated_at = Utc::now();
        state.threads.insert(thread.id, thread.clone());
        state.emit(
            "inbox.thread.unmerged",
            json!({
                "thread_id": thread.id.to_string(),
                "target_thread_id": target_id.to_string(),
                "unmerged_by": caller.actor,
            }),
        );
        Ok(thread_view(&state, &thread, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn request_reply(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReplyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let thread = load_thread(&state, &path)?;
        if thread.merged_into.is_some() {
            return Err(UnifiedError::conflict(
                "A merged thread cannot be replied to; reply on the live target thread instead",
            ));
        }
        let mut reply_body: Option<String> = match &body.body {
            Some(raw) => Some(require_text(raw, "body", MAX_BODY_LEN)?),
            None => None,
        };
        if reply_body.is_none() {
            let Some(canned_id) = body.canned_reply_id else {
                return Err(UnifiedError::bad_request(
                    "Provide either body or canned_reply_id",
                ));
            };
            let canned = load_canned_reply(&state, &canned_id.to_string())?;
            let expanded = canned
                .body
                .replace("{customer}", thread.customer_label.as_deref().unwrap_or(""))
                .replace("{subject}", &thread.subject)
                .replace("{thread_id}", &thread.id.to_string());
            reply_body = Some(require_text(&expanded, "body", MAX_BODY_LEN)?);
        }
        let reply_body = reply_body.expect("reply body resolved above");
        // The inbox never sends mail or chat itself: the owning
        // module consumes this event and sends through its own
        // channel. The reply is a request, not a delivery.
        state.emit(
            "inbox.reply.requested",
            json!({
                "thread_id": thread.id.to_string(),
                "source_channel": thread.source_channel,
                "source_id": thread.source_id,
                "body": reply_body,
                "requested_by": caller.actor,
                "department_id": thread.department_id,
                "assignee_id": thread.assignee_id,
            }),
        );
        Ok(json!({
            "requested": true,
            "thread_id": thread.id.to_string(),
            "source_channel": thread.source_channel,
            "source_id": thread.source_id,
            "requested_by": caller.actor,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Canned reply handlers
// ---------------------------------------------------------------------------

async fn list_canned_replies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_VIEW)?;
        let params = query.into_inner();
        let department_filter = params.filters.get("department_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<InboxCannedReply> = state
            .canned_replies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|reply| {
                if let Some(department) = &department_filter {
                    if reply.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {}", reply.title, reply.body).to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["title", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a.title.cmp(&b.title),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_canned_reply(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    body: web::Json<CreateCannedReplyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InboxCannedReply> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let body_text = require_text(&input.body, "body", MAX_BODY_LEN)?;
        let department_id = optional_text(&input.department_id, "department_id", MAX_ID_LEN)?;
        let now = Utc::now();
        let reply = InboxCannedReply {
            id: Uuid::now_v7(),
            department_id,
            title,
            body: body_text,
            created_by: caller.actor,
            created_at: now,
            updated_at: now,
        };
        state.canned_replies.insert(reply.id, reply.clone());
        Ok(reply)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_canned_reply(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InboxCannedReply> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_VIEW)?;
        load_canned_reply(&state, &path)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_canned_reply(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCannedReplyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InboxCannedReply> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let input = body.into_inner();
        let mut reply = load_canned_reply(&state, &path)?;
        if let Some(department) = &input.department_id {
            reply.department_id = match department {
                Some(raw) => Some(require_text(raw, "department_id", MAX_ID_LEN)?),
                None => None,
            };
        }
        if let Some(title) = &input.title {
            reply.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(body_text) = &input.body {
            reply.body = require_text(body_text, "body", MAX_BODY_LEN)?;
        }
        reply.updated_at = Utc::now();
        state.canned_replies.insert(reply.id, reply.clone());
        Ok(reply)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_canned_reply(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let reply = load_canned_reply(&state, &path)?;
        state.canned_replies.remove(&reply.id);
        Ok(json!({ "deleted": true, "id": reply.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
