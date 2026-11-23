// Todos module — personal, per-user todos.
//
// BOUNDARY VS THE TASKS MODULE (`src/modules/tasks.rs`) — READ THIS FIRST.
// Tasks are shared, assigned work items: they have an assignee and a
// reporter, a company / project linkage, a multi-step status workflow
// (`todo -> in_progress -> review -> done`, plus `blocked` and
// `cancelled`), comments, and time tracking. Todos are the opposite:
// strictly personal, lightweight reminders that belong to exactly one
// user. A todo has no assignee (its owner IS the actor), no reporter,
// no company, no project, and only two statuses (`open` / `done`).
// Nothing in this module assigns work to another person, and nothing
// in the tasks module is reused here: the two stores are separate, and
// a todo can never become a task (or the reverse) without an explicit,
// user-driven conversion that does not exist yet. Sharing in this
// module is read/edit access to a LIST, never ownership transfer and
// never assignment.
//
// Ownership rules (enforced server-side on every handler):
//   - Every list and every todo belongs to exactly one user
//     (`owner_id`, taken from the `X-User-Id` header — never from a
//     client-supplied `user_id` field).
//   - A user can only list, read, update, or delete their own lists
//     and todos, plus lists explicitly shared with them (see
//     `TodoShare`). A shared user sees the todos in that list but the
//     todos still belong to the owner: ownership never transfers.
//   - Fail closed: an object the caller neither owns nor has shared
//     access to answers 404 (not 403), so existence never leaks.
//
// Routes live under `/api/v1/todos`:
//   Lists:
//     GET/POST /lists — own lists / create a list.
//     GET/PATCH/DELETE /lists/{id}.
//     POST /lists/{id}/archive, /lists/{id}/unarchive.
//     GET  /lists/{id}/shares — who a list is shared with (owner only).
//     POST /lists/{id}/share — share with a user (read or edit).
//     DELETE /lists/{id}/share/{user_id} — revoke sharing.
//     GET  /shared-with-me — lists shared with the caller.
//   Todos:
//     GET/POST /todos — own + shared todos (paginated, filtered) /
//       create, including quick capture (title only; defaults apply).
//     GET  /todos/completed — completed view (paginated).
//     GET  /todos/overdue — open todos past their due time.
//     GET  /todos/streaks — computed completion streaks.
//     GET/PATCH/DELETE /todos/{id}.
//     POST /todos/{id}/complete, /todos/{id}/reopen.
//     GET  /todos/{id}/history — completion log for one todo.
//     POST /todos/{id}/subtasks — add a subtask (one level only).
//     PATCH /todos/{id}/subtasks/{subtask_id} — rename a subtask.
//     POST /todos/{id}/subtasks/{subtask_id}/toggle — check/uncheck.
//     DELETE /todos/{id}/subtasks/{subtask_id}.
//     POST /todos/{id}/attachments — attach by storage key (metadata
//       only; bytes live in the storage module).
//     DELETE /todos/{id}/attachments/{attachment_id}.
//     POST /todos/{id}/reminders — add a due-date reminder offset.
//     GET  /todos/{id}/reminders — list reminders for a todo.
//     DELETE /todos/{id}/reminders/{reminder_id} — cancel a reminder.
//   AI (drafts only — the AI never creates, completes, reorders, or
//     deletes a todo):
//     POST /ai/summarize — draft summary of the caller's own todos.
//     POST /ai/plan-day — draft day plan from the caller's own todos.
//
// Events emitted on the hook bus:
//   - `todos.list.created`, `todos.list.updated`, `todos.list.deleted`,
//     `todos.list.archived`, `todos.list.unarchived`
//   - `todos.list.shared`, `todos.list.share_revoked`
//   - `todos.todo.created`, `todos.todo.updated`, `todos.todo.deleted`
//   - `todos.todo.completed`, `todos.todo.reopened`
//   - `todos.todo.overdue` (emitted by the reminder job, once per
//     todo per UTC day — see `on_reminders_send_requested`)
//   - `todos.subtask.created`, `todos.subtask.updated`,
//     `todos.subtask.toggled`, `todos.subtask.deleted`
//   - `todos.attachment.added`, `todos.attachment.removed`
//   - `todos.reminder.created`, `todos.reminder.cancelled`,
//     `todos.reminder.sent`
//   - `todos.ai.draft_requested` (see the AI section below)
//   - `notification.requested` (due-date and overdue reminders, same
//     payload shape the notifications module documents:
//     `{"user_id", "title", "body", "kind", "category", "module",
//     "priority", "link"}`)
//   - `scheduler.enqueue` with `{"kind": "todos.reminders.send",
//     "payload": {"todo_id", "user_id", "remind_at",
//     "offset_minutes"}}` when a reminder is created. The kind is
//     registered in the scheduler's `KNOWN_KINDS`
//     (`scheduler/types.rs`) and dispatched in
//     `scheduler/service.rs`, which emits
//     `todos.reminders.send_requested`; the scheduler answers
//     `{"queued": true, "job_id": ...}` when it accepts the job.
//     The todo and the reminder are still created and stay usable
//     even when a job is declined (it can be sent manually).
// Events this module listens to:
//   - `todos.reminders.send_requested` — the event the scheduler
//     emits when a `todos.reminders.send` job fires (naming rule:
//     `backup.run` -> `backup.run_requested`). The handler sends due
//     reminders, marks them sent, and runs the overdue sweep.
//   - `todos.context_requested` — answered with the subject user's
//     own todos, so an AI draft flow can ground on them. This mirrors
//     the `ticket.context_requested` / `chat.context_requested`
//     pattern the AI module already uses: the AI module gathers
//     context through `<module>.context_requested` events and merges
//     whatever those modules return. Hook payloads are server-side
//     only and are never client-supplied, so the `user_id` in the
//     payload is the subject the (trusted, in-process) requester
//     asked about.
//
// AI wiring — READ THIS BEFORE CHANGING IT. The AI module
// (`src/modules/ai.rs`) has NO generic draft-request event and no
// endpoint a module can call to ask for an arbitrary draft: its draft
// endpoints are fixed (`/complete`, `/support-draft`, `/chat-draft`,
// `/content-draft`, `/email-draft`, `/answer`) and its context events
// are `ai.context_requested` / `site.context_requested` (answered by
// the AI module itself with site facts) plus the per-module
// `ticket.context_requested` / `chat.context_requested` pattern. So
// this module follows that exact pattern instead of inventing a
// direct call into `ai.rs` (which would also violate the
// hook-bus-only interconnection rule):
//   1. This module answers `todos.context_requested` (above), so any
//      AI draft flow can pull the user's todos as grounding context.
//   2. The `/ai/summarize` and `/ai/plan-day` endpoints build the
//      context text themselves from the caller's OWN todos only, then
//      emit `todos.ai.draft_requested` with
//      `{"user_id", "role", "kind", "todo_ids", "context_text",
//      "requested_at"}` and return the first hook result carrying a
//      `"draft"` (or `"text"`) string as a draft
//      (`{"kind", "draft", "ai_generated": true, "status": "draft",
//      "todo_ids", "user_id"}`). The AI allowlists are NOT
//      re-implemented here: the caller is passed through and the AI
//      module enforces its own authorization (its `ai:use`
//      capability, its company / allowed-users settings, and its
//      personal-vs-company key rules) when it answers the event.
//   3. Fail closed: when no handler answers the event (the AI module
//      has not wired a `todos.ai.draft_requested` handler yet), the
//      endpoint answers 503 instead of fabricating a summary.
//   EXPECTED AI-SIDE HANDLER (for the integrator / AI module owner):
//   register a handler for `todos.ai.draft_requested` that runs the
//   request through the AI module's normal completion path
//   (capability `ai:use`, configured provider, usage recording),
//   with a system prompt for `kind: "summarize"` ("Summarize these
//   personal todos: group by list and priority, call out overdue and
//   due-today items, return Markdown only") or `kind: "plan-day"`
//   ("Turn these todos into a day plan ordered by due time and
//   priority, return Markdown only"), using `context_text` as the
//   only grounding, and contributes
//   `{"draft": "<markdown>", "model": "<model>", "provider": "<kind>"}`.
//   The handler must never create, complete, reorder, or delete a
//   todo, and must never include another user's todos: `context_text`
//   already contains only the requesting user's own todos.
//
// Persistence: todos live in in-memory DashMap stores (the same
// approach the tasks module uses). The Diesel tables for durable
// persistence are defined by migration
// `2026-11-22-000035_todos` (tables `todo_lists`, `todos`,
// `todo_subtasks`, `todo_shares`, `todo_completion_log`,
// `todo_reminders`); the `schema.rs` table blocks and the `models.rs`
// structs are supplied to the integrator in the module report and
// must be added before a database-backed implementation can be wired
// in. Nothing in this file references a table that does not exist
// yet, so it compiles against the current schema. The in-memory
// stores stay authoritative until that wiring lands.

// todos — types.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use dashmap::{DashMap, DashSet};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::store::*;
use super::common::*;
use super::validation::*;
use super::access::*;
use super::service::*;
use super::handlers_lists::*;
use super::handlers_todos::*;
use super::handlers_subtasks::*;
use super::handlers_ai::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------

pub const EVENT_LIST_CREATED: &str = "todos.list.created";

pub const EVENT_LIST_UPDATED: &str = "todos.list.updated";

pub const EVENT_LIST_DELETED: &str = "todos.list.deleted";

pub const EVENT_LIST_ARCHIVED: &str = "todos.list.archived";

pub const EVENT_LIST_UNARCHIVED: &str = "todos.list.unarchived";

pub const EVENT_LIST_SHARED: &str = "todos.list.shared";

pub const EVENT_LIST_SHARE_REVOKED: &str = "todos.list.share_revoked";

pub const EVENT_TODO_CREATED: &str = "todos.todo.created";

pub const EVENT_TODO_UPDATED: &str = "todos.todo.updated";

pub const EVENT_TODO_DELETED: &str = "todos.todo.deleted";

pub const EVENT_TODO_COMPLETED: &str = "todos.todo.completed";

pub const EVENT_TODO_REOPENED: &str = "todos.todo.reopened";

pub const EVENT_TODO_OVERDUE: &str = "todos.todo.overdue";

pub const EVENT_SUBTASK_CREATED: &str = "todos.subtask.created";

pub const EVENT_SUBTASK_UPDATED: &str = "todos.subtask.updated";

pub const EVENT_SUBTASK_TOGGLED: &str = "todos.subtask.toggled";

pub const EVENT_SUBTASK_DELETED: &str = "todos.subtask.deleted";

pub const EVENT_ATTACHMENT_ADDED: &str = "todos.attachment.added";

pub const EVENT_ATTACHMENT_REMOVED: &str = "todos.attachment.removed";

pub const EVENT_REMINDER_CREATED: &str = "todos.reminder.created";

pub const EVENT_REMINDER_CANCELLED: &str = "todos.reminder.cancelled";

pub const EVENT_REMINDER_SENT: &str = "todos.reminder.sent";

/// Draft request to the AI module (see the module header for the
/// expected AI-side handler contract).
pub const EVENT_AI_DRAFT_REQUESTED: &str = "todos.ai.draft_requested";

/// Answered by this module with the subject user's own todos.
pub const EVENT_TODOS_CONTEXT_REQUESTED: &str = "todos.context_requested";

/// Emitted by the scheduler when a `todos.reminders.send` job fires.
pub const EVENT_REMINDERS_SEND_REQUESTED: &str = "todos.reminders.send_requested";

/// Scheduler job kind for due-date reminders (not yet in the
/// scheduler's `KNOWN_KINDS` — see the module header).
pub const SCHEDULER_JOB_KIND_REMINDERS: &str = "todos.reminders.send";

// ---------------------------------------------------------------------------
// Permissions (checked through the `role.check` hook; fail closed).
// ---------------------------------------------------------------------------

/// May read own lists and todos (and lists shared with the caller).
pub const PERM_TODOS_READ: &str = "todos:read";

/// May create, update, complete, and delete own lists and todos.
pub const PERM_TODOS_WRITE: &str = "todos:write";

/// May share own lists with other users and revoke sharing.
pub const PERM_TODOS_SHARE: &str = "todos:share";

/// May ask the AI module for todo drafts (enforced again by the AI
/// module itself when it answers `todos.ai.draft_requested`).
pub const PERM_AI_USE: &str = "ai:use";

// ---------------------------------------------------------------------------
// Limits and allowlists.
// ---------------------------------------------------------------------------

pub(crate) const MAX_TITLE_LEN: usize = 200;

pub(crate) const MAX_NOTES_LEN: usize = 5000;

pub(crate) const MAX_LIST_NAME_LEN: usize = 100;

pub(crate) const MAX_TAG_LEN: usize = 64;

pub(crate) const MAX_TAGS: usize = 20;

pub(crate) const MAX_SUBTASKS: usize = 50;

pub(crate) const MAX_ATTACHMENTS: usize = 20;

pub(crate) const MAX_REMINDERS_PER_TODO: usize = 10;

pub(crate) const MAX_FILE_NAME_LEN: usize = 255;

pub(crate) const MAX_STORAGE_KEY_LEN: usize = 512;

pub(crate) const MAX_TIMEZONE_LEN: usize = 64;

pub(crate) const MAX_CONTEXT_CHARS: usize = 8000;

pub(crate) const MAX_AI_TODOS: usize = 100;

/// Reminder offsets, in minutes before the due time: 0 (at the due
/// time) up to 30 days.
pub(crate) const MAX_REMINDER_OFFSET_MINUTES: i64 = 43_200;

/// List colours. Plain string fields, validated against this list —
/// never a free-form colour value, so clients cannot inject arbitrary
/// styles through a list.
pub const LIST_COLORS: &[&str] = &[
    "red", "orange", "amber", "green", "teal", "blue", "indigo", "purple", "pink", "gray",
    "brown",
];

/// List icons. Plain string fields, validated against this list.
pub const LIST_ICONS: &[&str] = &[
    "inbox", "star", "briefcase", "home", "shopping", "health", "study", "travel", "finance",
    "calendar", "flag", "check", "folder", "tag", "bell", "gift", "music", "sport", "book",
    "heart",
];

/// Default list name, created lazily for a user on first use.
pub const DEFAULT_LIST_NAME: &str = "Inbox";

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

/// Todo priority. `normal` is the default; there is no `medium` here
/// (unlike tasks, which use `low/medium/high/urgent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoPriority {
    Low,
    Normal,
    High,
    Urgent,
}

impl Default for TodoPriority {
    fn default() -> Self {
        Self::Normal
    }
}

impl TodoPriority {
    pub(crate) fn rank(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Normal => 1,
            Self::High => 2,
            Self::Urgent => 3,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
            Self::Urgent => "urgent",
        }
    }
}

/// Todo status. Only two states: `open` or `done`. There is no
/// in-progress / review / blocked workflow — that is the tasks
/// module's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Open,
    Done,
}

impl Default for TodoStatus {
    fn default() -> Self {
        Self::Open
    }
}

impl TodoStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Done => "done",
        }
    }
}

/// How often a recurring todo repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecurrenceFrequency {
    Daily,
    Weekly,
    Monthly,
}

impl RecurrenceFrequency {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "daily" => Some(Self::Daily),
            "weekly" => Some(Self::Weekly),
            "monthly" => Some(Self::Monthly),
            _ => None,
        }
    }
}

/// Permission a shared user holds on a list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharePermission {
    Read,
    Edit,
}

impl SharePermission {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Edit => "edit",
        }
    }
}

/// One todo list. `owner_id` is the only owner; sharing is recorded
/// separately in `TodoShare` and never changes ownership.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoList {
    pub id: Uuid,
    pub owner_id: String,
    pub name: String,
    pub color: String,
    pub icon: String,
    pub archived: bool,
    pub sort_order: i32,
    /// True for the lazily created default ("Inbox") list.
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One checkable subtask. Exactly one level: a subtask has no
/// children of its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoSubtask {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub title: String,
    pub done: bool,
    pub position: i32,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One attachment reference. Metadata only: the bytes live in the
/// storage module and are referenced by `storage_target` +
/// `storage_key`. No second storage system is built here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoAttachment {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub file_name: Option<String>,
    pub size_bytes: Option<u64>,
    pub created_at: DateTime<Utc>,
}

/// One todo. `owner_id` is the single owner; a shared-list viewer
/// never becomes an owner. `overdue` is computed, never stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Todo {
    pub id: Uuid,
    pub list_id: Uuid,
    pub owner_id: String,
    pub title: String,
    pub notes: Option<String>,
    /// Due date + time, stored as UTC. `timezone` is the IANA name
    /// the user entered it in (format-checked only, never used to
    /// shift the stored UTC time).
    pub due_at: Option<DateTime<Utc>>,
    pub timezone: String,
    pub priority: TodoPriority,
    pub status: TodoStatus,
    pub tags: Vec<String>,
    pub subtasks: Vec<TodoSubtask>,
    pub attachments: Vec<TodoAttachment>,
    /// Recurrence rule, when set. `recurrence_remaining` counts how
    /// many further occurrences may still be created (including the
    /// next one); `None` means "repeat until `recurrence_until`".
    /// Completing one occurrence creates the next one and never
    /// auto-completes a future occurrence.
    pub recurrence_frequency: Option<RecurrenceFrequency>,
    pub recurrence_interval: u32,
    pub recurrence_count: Option<u32>,
    pub recurrence_until: Option<DateTime<Utc>>,
    pub recurrence_remaining: Option<u32>,
    /// Shared by every occurrence of one recurring series, so the
    /// series can be listed together. `None` for non-recurring todos.
    pub recurrence_group_id: Option<Uuid>,
    pub position: i32,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Todo {
    /// True when the due time has passed and the todo is still open.
    pub fn is_overdue(&self, now: DateTime<Utc>) -> bool {
        match self.due_at {
            Some(due) => due < now && self.status == TodoStatus::Open,
            None => false,
        }
    }
}

/// Public todo representation, with the computed `overdue` flag and
/// the caller's effective access (`can_edit` is false for a
/// read-shared viewer).
#[derive(Debug, Clone, Serialize)]
pub struct TodoResponse {
    #[serde(flatten)]
    pub todo: Todo,
    pub overdue: bool,
    pub can_edit: bool,
}

impl TodoResponse {
    pub(crate) fn new(todo: &Todo, can_edit: bool) -> Self {
        Self {
            overdue: todo.is_overdue(Utc::now()),
            todo: todo.clone(),
            can_edit,
        }
    }
}

/// One share grant: `shared_with_user_id` may read (or read + edit)
/// the list. Ownership stays with `owner_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoShare {
    pub id: Uuid,
    pub list_id: Uuid,
    pub owner_id: String,
    pub shared_with_user_id: String,
    pub permission: SharePermission,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One completion-log entry: who completed or reopened a todo, and
/// when. This is the source for streaks and for the per-todo history
/// view; it is append-only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoCompletionLog {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub user_id: String,
    /// `completed` or `reopened`.
    pub action: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One due-date reminder. `remind_at` is `due_at` minus
/// `offset_minutes`; `due_at` is denormalized here so the migration
/// can enforce "a reminder always carries a due date" with a plain
/// `NOT NULL` + `remind_at <= due_at` CHECK. `status` is `pending`,
/// `sent`, or `cancelled`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoReminder {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub owner_id: String,
    pub offset_minutes: i64,
    pub remind_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub status: String,
    /// Scheduler linkage, filled when the reminder is created: the
    /// queue item id when the scheduler accepted the job, and one of
    /// `not_scheduled`, `queued`, `declined`.
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub sent_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Computed streaks for one user. Computed from the completion log,
/// never stored as a claim: a streak is only as true as the log
/// entries behind it.
#[derive(Debug, Clone, Serialize)]
pub struct StreaksView {
    pub user_id: String,
    /// Consecutive UTC days ending today (or yesterday, when nothing
    /// was completed today yet) with at least one completion.
    pub current_streak_days: u32,
    /// Longest run of consecutive completion days on record.
    pub longest_streak_days: u32,
    /// Total completions on record.
    pub total_completions: u64,
    pub last_completion_date: Option<String>,
    pub computed_at: DateTime<Utc>,
}

/// One AI draft produced from the caller's own todos. Drafts only:
/// the response is a draft the user may save as a note.
#[derive(Debug, Clone, Serialize)]
pub struct AiDraftResponse {
    pub kind: String,
    pub draft: String,
    pub ai_generated: bool,
    pub status: String,
    pub todo_ids: Vec<Uuid>,
    pub user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub requested_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateListRequest {
    pub name: String,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub sort_order: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateListRequest {
    pub name: Option<String>,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub sort_order: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct ShareListRequest {
    pub user_id: String,
    pub permission: Option<SharePermission>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTodoRequest {
    pub title: String,
    pub list_id: Option<Uuid>,
    pub notes: Option<String>,
    pub due_at: Option<DateTime<Utc>>,
    pub timezone: Option<String>,
    pub priority: Option<TodoPriority>,
    pub tags: Option<Vec<String>>,
    pub position: Option<i32>,
    pub recurrence_frequency: Option<RecurrenceFrequency>,
    pub recurrence_interval: Option<u32>,
    pub recurrence_count: Option<u32>,
    pub recurrence_until: Option<DateTime<Utc>>,
    /// Minutes before the due time at which to remind the owner.
    /// Each entry creates one `TodoReminder` (and one scheduler job
    /// attempt). Requires `due_at`.
    pub reminder_offsets_minutes: Option<Vec<i64>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTodoRequest {
    pub title: Option<String>,
    pub list_id: Option<Uuid>,
    pub notes: Option<Option<String>>,
    pub due_at: Option<Option<DateTime<Utc>>>,
    pub timezone: Option<String>,
    pub priority: Option<TodoPriority>,
    pub status: Option<TodoStatus>,
    pub tags: Option<Vec<String>>,
    pub position: Option<i32>,
    pub recurrence_frequency: Option<Option<RecurrenceFrequency>>,
    pub recurrence_interval: Option<u32>,
    pub recurrence_count: Option<Option<u32>>,
    pub recurrence_until: Option<Option<DateTime<Utc>>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSubtaskRequest {
    pub title: String,
    pub position: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSubtaskRequest {
    pub title: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddAttachmentRequest {
    pub storage_target: String,
    pub storage_key: String,
    pub file_name: Option<String>,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateReminderRequest {
    /// Minutes before the due time. Requires the todo to have a due
    /// date (enforced here and by the migration CHECK).
    pub offset_minutes: i64,
}

#[derive(Debug, Deserialize)]
pub struct AiSummarizeRequest {
    pub list_id: Option<Uuid>,
    #[serde(default)]
    pub include_completed: bool,
    pub max_todos: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct AiPlanDayRequest {
    /// Date to plan, `YYYY-MM-DD` in the caller's timezone; defaults
    /// to today (UTC).
    pub date: Option<String>,
    pub list_id: Option<Uuid>,
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_ranks_and_defaults_match() {
        assert_eq!(TodoPriority::default(), TodoPriority::Normal);
        assert!(TodoPriority::Urgent.rank() > TodoPriority::High.rank());
        assert!(TodoPriority::High.rank() > TodoPriority::Normal.rank());
        assert!(TodoPriority::Normal.rank() > TodoPriority::Low.rank());
        assert_eq!(TodoPriority::Low.as_str(), "low");
        assert_eq!(TodoStatus::default(), TodoStatus::Open);
        assert_eq!(TodoStatus::Done.as_str(), "done");
        assert_eq!(SharePermission::Read.as_str(), "read");
        assert_eq!(SharePermission::Edit.as_str(), "edit");
    }

    #[test]
    fn recurrence_frequencies_parse_from_strings() {
        assert_eq!(
            RecurrenceFrequency::parse("daily"),
            Some(RecurrenceFrequency::Daily)
        );
        assert_eq!(
            RecurrenceFrequency::parse("weekly"),
            Some(RecurrenceFrequency::Weekly)
        );
        assert_eq!(
            RecurrenceFrequency::parse("monthly"),
            Some(RecurrenceFrequency::Monthly)
        );
        assert_eq!(RecurrenceFrequency::parse("yearly"), None);
        assert_eq!(RecurrenceFrequency::Daily.as_str(), "daily");
    }
}
