// Inbox - Thread, assignment and canned-reply types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_SUBJECT_LEN: usize = 500;

pub(crate) const MAX_BODY_LEN: usize = 10_000;

pub(crate) const MAX_TITLE_LEN: usize = 200;

pub(crate) const MAX_ID_LEN: usize = 255;

pub(crate) const MAX_PARTICIPANTS: usize = 50;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_INBOX_VIEW: &str = "inbox.view";

pub(crate) const PERM_INBOX_MANAGE: &str = "inbox.manage";

/// Hook event names this module listens to.
pub(crate) const TICKET_CREATED: &str = "ticket.created";

pub(crate) const TICKET_UPDATED: &str = "ticket.updated";

pub(crate) const TICKET_COMMENTED: &str = "ticket.commented";

pub(crate) const TICKET_ASSIGNED: &str = "ticket.assigned";

pub(crate) const CHAT_MESSAGE: &str = "chat.message";

pub(crate) const CHAT_SESSION_CREATED: &str = "chat.session.created";

pub(crate) const CHAT_SESSION_ASSIGNED: &str = "chat.session.assigned";

pub(crate) const MAIL_SENT: &str = "mail.sent";

pub(crate) const UNSNOOZE_REQUESTED: &str = "inbox.unsnooze_requested";

/// Scheduler job kind used to bring a snoozed thread back.
pub(crate) const SCHEDULER_KIND_UNSNOOZE: &str = "inbox.unsnooze";

/// Source channels a thread can come from. `social` threads are
/// registered manually until a social module emits mention events.
pub(crate) const CHANNELS: [&str; 4] = ["email", "chat", "ticket", "social"];

/// Thread statuses. Ticket statuses map onto these on ingest:
/// `open` / `in_progress` -> `open`, `waiting` -> `pending`,
/// `resolved` -> `resolved`, `closed` -> `closed`.
pub(crate) const STATUSES: [&str; 4] = ["open", "pending", "resolved", "closed"];

/// Thread priorities, matching the tickets module's scale.
pub(crate) const PRIORITIES: [&str; 4] = ["low", "normal", "high", "urgent"];

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
    /// Owning tenant (uuidv7), taken from the resolved tenant
    /// context at creation. Handlers only serve a thread whose
    /// tenant matches the caller's tenant; a nil UUID means the
    /// thread was ingested from an event payload that carried no
    /// tenant id (unscoped ingestion) and is never served via the
    /// API — see the module header. Deserialization defaults to
    /// nil so pre-tenancy payloads still parse.
    #[serde(default)]
    pub tenant_id: Uuid,
    /// Company scope from the resolved tenant context at creation
    /// (`None` for unscoped ingestion). A record is visible only
    /// when its company matches the caller's company scope, when
    /// both carry one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
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
    pub(crate) fn is_overdue(&self, now: DateTime<Utc>) -> bool {
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
    pub(crate) fn is_snoozed(&self, now: DateTime<Utc>) -> bool {
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
    /// Owning tenant (uuidv7), copied from the thread's tenant at
    /// assignment time. Only assignments of the caller's tenant
    /// are ever listed.
    #[serde(default)]
    pub tenant_id: Uuid,
    /// Company scope from the resolved tenant context at
    /// assignment time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
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
    /// Owning tenant (uuidv7), taken from the resolved tenant
    /// context at creation. Handlers only serve replies whose
    /// tenant matches the caller's tenant.
    #[serde(default)]
    pub tenant_id: Uuid,
    /// Company scope from the resolved tenant context at creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
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
