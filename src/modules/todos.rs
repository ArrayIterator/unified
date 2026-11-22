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
//     "offset_minutes"}}` when a reminder is created. CURRENT LIMIT,
//     stated plainly (same shape as the meetings module): the
//     scheduler's `KNOWN_KINDS` does not contain
//     `todos.reminders.send` yet, so today the scheduler answers
//     `{"queued": false, "error": "Unknown job kind: ..."}` and the
//     reminder is recorded with `scheduler_status: "declined"` instead
//     of pretending it was scheduled. The todo and the reminder are
//     still created and stay usable; the follow-up is the scheduler
//     arm documented in the module report (add the kind to
//     `KNOWN_KINDS`, add a dispatch arm emitting
//     `todos.reminders.send_requested`, and add the
//     `todos.reminders.send_requested` -> `todos.reminders.send`
//     mapping in `on_scheduler_enqueue`).
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

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use dashmap::{DashMap, DashSet};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

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

const MAX_TITLE_LEN: usize = 200;
const MAX_NOTES_LEN: usize = 5000;
const MAX_LIST_NAME_LEN: usize = 100;
const MAX_TAG_LEN: usize = 64;
const MAX_TAGS: usize = 20;
const MAX_SUBTASKS: usize = 50;
const MAX_ATTACHMENTS: usize = 20;
const MAX_REMINDERS_PER_TODO: usize = 10;
const MAX_FILE_NAME_LEN: usize = 255;
const MAX_STORAGE_KEY_LEN: usize = 512;
const MAX_TIMEZONE_LEN: usize = 64;
const MAX_CONTEXT_CHARS: usize = 8000;
const MAX_AI_TODOS: usize = 100;
/// Reminder offsets, in minutes before the due time: 0 (at the due
/// time) up to 30 days.
const MAX_REMINDER_OFFSET_MINUTES: i64 = 43_200;

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
    fn rank(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Normal => 1,
            Self::High => 2,
            Self::Urgent => 3,
        }
    }

    fn as_str(self) -> &'static str {
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
    fn as_str(self) -> &'static str {
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
    fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }

    fn parse(value: &str) -> Option<Self> {
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
    fn as_str(self) -> &'static str {
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
    fn new(todo: &Todo, can_edit: bool) -> Self {
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

// ---------------------------------------------------------------------------
// Shared state.
// ---------------------------------------------------------------------------

/// In-memory stores for todos. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); handlers use `ctx.hooks` directly
/// and tolerate a missing bus only where noted. Durable rows for these
/// types are defined by migration `2026-11-22-000035_todos`; until
/// that migration is wired into a database-backed implementation,
/// todos do not survive a restart.
#[derive(Clone)]
pub struct TodosState {
    pub lists: Arc<DashMap<Uuid, TodoList>>,
    pub todos: Arc<DashMap<Uuid, Todo>>,
    /// Keyed by `"{list_id}:{shared_with_user_id}"`.
    pub shares: Arc<DashMap<String, TodoShare>>,
    pub completion_log: Arc<DashMap<Uuid, TodoCompletionLog>>,
    pub reminders: Arc<DashMap<Uuid, TodoReminder>>,
    /// Overdue notifications already sent, keyed by
    /// `"{todo_id}:{utc-date}"`, so the reminder job emits
    /// `todos.todo.overdue` at most once per todo per day.
    pub overdue_notified: Arc<DashSet<String>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for TodosState {
    fn default() -> Self {
        Self {
            lists: Arc::new(DashMap::new()),
            todos: Arc::new(DashMap::new()),
            shares: Arc::new(DashMap::new()),
            completion_log: Arc::new(DashMap::new()),
            reminders: Arc::new(DashMap::new()),
            overdue_notified: Arc::new(DashSet::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl TodosState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers: caller, capabilities, validation.
// ---------------------------------------------------------------------------

/// The caller, from the headers every module in this codebase uses:
/// `X-User-Id` identifies the actor, `X-Role` carries the role slug
/// the capability check runs against. (Same shape as `ai.rs`.)
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
/// the `role.check` hook. Fails closed: no answer means not allowed.
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

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.chars().count() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => Ok(Some(require_text(text, field, max)?)),
    }
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

fn validate_color(color: &str) -> UnifiedResult<String> {
    let value = color.trim().to_lowercase();
    if LIST_COLORS.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(UnifiedError::bad_request(format!(
            "color must be one of: {}",
            LIST_COLORS.join(", ")
        )))
    }
}

fn validate_icon(icon: &str) -> UnifiedResult<String> {
    let value = icon.trim().to_lowercase();
    if LIST_ICONS.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(UnifiedError::bad_request(format!(
            "icon must be one of: {}",
            LIST_ICONS.join(", ")
        )))
    }
}

fn validate_tags(tags: &[String]) -> UnifiedResult<Vec<String>> {
    if tags.len() > MAX_TAGS {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_TAGS} tags are allowed"
        )));
    }
    let mut cleaned: Vec<String> = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = tag.trim().to_lowercase();
        if tag.is_empty() {
            return Err(UnifiedError::bad_request("Tags must not be empty"));
        }
        if tag.chars().count() > MAX_TAG_LEN {
            return Err(UnifiedError::bad_request(format!(
                "Each tag must be at most {MAX_TAG_LEN} characters long"
            )));
        }
        if !cleaned.contains(&tag) {
            cleaned.push(tag);
        }
    }
    Ok(cleaned)
}

fn validate_reminder_offset(offset: i64) -> UnifiedResult<i64> {
    if offset < 0 || offset > MAX_REMINDER_OFFSET_MINUTES {
        return Err(UnifiedError::bad_request(format!(
            "offset_minutes must be between 0 and {MAX_REMINDER_OFFSET_MINUTES}"
        )));
    }
    Ok(offset)
}

fn validate_recurrence(
    frequency: Option<RecurrenceFrequency>,
    interval: u32,
    count: Option<u32>,
    until: Option<DateTime<Utc>>,
    due_at: Option<DateTime<Utc>>,
) -> UnifiedResult<()> {
    if frequency.is_none() {
        if interval != 1 || count.is_some() || until.is_some() {
            return Err(UnifiedError::bad_request(
                "recurrence_interval, recurrence_count, and recurrence_until require recurrence_frequency",
            ));
        }
        return Ok(());
    }
    if interval == 0 || interval > 365 {
        return Err(UnifiedError::bad_request(
            "recurrence_interval must be between 1 and 365",
        ));
    }
    if let Some(count) = count {
        if count == 0 || count > 366 {
            return Err(UnifiedError::bad_request(
                "recurrence_count must be between 1 and 366",
            ));
        }
    }
    if count.is_some() && until.is_some() {
        return Err(UnifiedError::bad_request(
            "recurrence_count and recurrence_until are mutually exclusive: set at most one",
        ));
    }
    if due_at.is_none() {
        return Err(UnifiedError::bad_request(
            "A recurring todo requires due_at: the next occurrence is computed from it",
        ));
    }
    if let (Some(until), Some(due)) = (until, due_at) {
        if until <= due {
            return Err(UnifiedError::bad_request(
                "recurrence_until must be after due_at",
            ));
        }
    }
    Ok(())
}

/// Advance a due time by one recurrence step. Monthly steps clamp
/// the day to the target month's length (31 January -> 28/29
/// February), computed by hand so no extra date crate is needed.
fn advance_due(
    due: DateTime<Utc>,
    frequency: RecurrenceFrequency,
    interval: u32,
) -> DateTime<Utc> {
    match frequency {
        RecurrenceFrequency::Daily => due + chrono::Duration::days(i64::from(interval)),
        RecurrenceFrequency::Weekly => due + chrono::Duration::days(i64::from(interval) * 7),
        RecurrenceFrequency::Monthly => {
            let total_months = due.month() as i64 + i64::from(interval) - 1;
            let year = due.year() + (total_months.div_euclid(12)) as i32;
            let month = (total_months.rem_euclid(12) + 1) as u32;
            let last_day = last_day_of_month(year, month);
            let day = due.day().min(last_day);
            let date = NaiveDate::from_ymd_opt(year, month, day)
                .unwrap_or_else(|| NaiveDate::from_ymd_opt(year, month, last_day).expect("valid month end"));
            let time = due.time();
            Utc.from_utc_datetime(&date.and_time(time))
        }
    }
}

fn last_day_of_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

// ---------------------------------------------------------------------------
// Helpers: access control (fail closed, 404 — never 403 — for objects
// the caller does not own and has no share for).
// ---------------------------------------------------------------------------

fn share_key(list_id: Uuid, user_id: &str) -> String {
    format!("{list_id}:{user_id}")
}

fn share_for(state: &TodosState, list_id: Uuid, user_id: &str) -> Option<TodoShare> {
    state
        .shares
        .get(&share_key(list_id, user_id))
        .map(|entry| entry.value().clone())
}

/// True when `user_id` may read the list (owner, or any share).
fn can_read_list(state: &TodosState, list: &TodoList, user_id: &str) -> bool {
    list.owner_id == user_id || share_for(state, list.id, user_id).is_some()
}

/// True when `user_id` may change the list and its todos (owner, or
/// an edit share). A read share is read-only.
fn can_edit_list(state: &TodosState, list: &TodoList, user_id: &str) -> bool {
    list.owner_id == user_id
        || share_for(state, list.id, user_id)
            .is_some_and(|share| share.permission == SharePermission::Edit)
}

fn not_found_list(id: Uuid) -> UnifiedError {
    UnifiedError::not_found(format!("Todo list not found: {id}"))
}

fn not_found_todo(id: Uuid) -> UnifiedError {
    UnifiedError::not_found(format!("Todo not found: {id}"))
}

fn load_list(state: &TodosState, id: Uuid) -> UnifiedResult<TodoList> {
    state
        .lists
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| not_found_list(id))
}

fn load_list_for_read(
    state: &TodosState,
    id: Uuid,
    user_id: &str,
) -> UnifiedResult<TodoList> {
    let list = load_list(state, id)?;
    if can_read_list(state, &list, user_id) {
        Ok(list)
    } else {
        Err(not_found_list(id))
    }
}

fn load_list_for_edit(
    state: &TodosState,
    id: Uuid,
    user_id: &str,
) -> UnifiedResult<TodoList> {
    let list = load_list(state, id)?;
    if can_edit_list(state, &list, user_id) {
        Ok(list)
    } else {
        Err(not_found_list(id))
    }
}

fn load_list_owned(state: &TodosState, id: Uuid, user_id: &str) -> UnifiedResult<TodoList> {
    let list = load_list(state, id)?;
    if list.owner_id == user_id {
        Ok(list)
    } else {
        Err(not_found_list(id))
    }
}

fn load_todo(state: &TodosState, id: Uuid) -> UnifiedResult<Todo> {
    state
        .todos
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| not_found_todo(id))
}

/// Load a todo the caller may read (owner, or a share on its list).
/// Returns the todo plus whether the caller may edit it.
fn load_todo_for_read(
    state: &TodosState,
    id: Uuid,
    user_id: &str,
) -> UnifiedResult<(Todo, bool)> {
    let todo = load_todo(state, id)?;
    let list = load_list(state, todo.list_id)?;
    if can_read_list(state, &list, user_id) {
        Ok((todo, can_edit_list(state, &list, user_id)))
    } else {
        Err(not_found_todo(id))
    }
}

/// Load a todo the caller may change. The todo still belongs to its
/// owner: a shared editor changes the owner's todo, never their own
/// copy (there is no copy).
fn load_todo_for_edit(
    state: &TodosState,
    id: Uuid,
    user_id: &str,
) -> UnifiedResult<Todo> {
    let todo = load_todo(state, id)?;
    let list = load_list(state, todo.list_id)?;
    if can_edit_list(state, &list, user_id) {
        Ok(todo)
    } else {
        Err(not_found_todo(id))
    }
}

/// Lists the caller may read: their own plus lists shared with them.
fn readable_lists(state: &TodosState, user_id: &str) -> Vec<TodoList> {
    state
        .lists
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|list| can_read_list(state, list, user_id))
        .collect()
}

/// The caller's default ("Inbox") list, creating it lazily when the
/// user has none. The default list is the quick-capture target.
fn ensure_default_list(state: &TodosState, user_id: &str) -> TodoList {
    if let Some(list) = state
        .lists
        .iter()
        .map(|entry| entry.value().clone())
        .find(|list| list.owner_id == user_id && list.is_default)
    {
        return list;
    }
    let now = Utc::now();
    let list = TodoList {
        id: Uuid::now_v7(),
        owner_id: user_id.to_string(),
        name: DEFAULT_LIST_NAME.to_string(),
        color: "blue".to_string(),
        icon: "inbox".to_string(),
        archived: false,
        sort_order: 0,
        is_default: true,
        created_at: now,
        updated_at: now,
    };
    state.lists.insert(list.id, list.clone());
    state.emit(
        EVENT_LIST_CREATED,
        json!({
            "list_id": list.id.to_string(),
            "owner_id": user_id,
            "name": list.name,
            "is_default": true,
            "actor_id": user_id,
            "resource": format!("todo_list:{}", list.id),
        }),
    );
    list
}

fn next_position(state: &TodosState, list_id: Uuid) -> i32 {
    state
        .todos
        .iter()
        .filter(|entry| entry.value().list_id == list_id)
        .map(|entry| entry.value().position)
        .max()
        .map_or(0, |max| max + 1)
}

fn log_completion(
    state: &TodosState,
    todo_id: Uuid,
    user_id: &str,
    action: &str,
) -> TodoCompletionLog {
    let now = Utc::now();
    let entry = TodoCompletionLog {
        id: Uuid::now_v7(),
        todo_id,
        user_id: user_id.to_string(),
        action: action.to_string(),
        created_at: now,
        updated_at: now,
    };
    state.completion_log.insert(entry.id, entry.clone());
    entry
}

fn todo_event_payload(todo: &Todo, actor: &str) -> Value {
    json!({
        "todo_id": todo.id.to_string(),
        "id": todo.id.to_string(),
        "list_id": todo.list_id.to_string(),
        "owner_id": todo.owner_id,
        "title": todo.title,
        "status": todo.status.as_str(),
        "priority": todo.priority.as_str(),
        "due_at": todo.due_at,
        "actor_id": actor,
        "entity_type": "todo",
        "entity_id": todo.id.to_string(),
        "resource": format!("todo:{}", todo.id),
    })
}

/// Compute streaks from the completion log: distinct UTC dates with
/// at least one `completed` entry by this user.
fn compute_streaks(state: &TodosState, user_id: &str) -> StreaksView {
    let mut days: HashSet<NaiveDate> = HashSet::new();
    let mut total = 0u64;
    let mut last: Option<NaiveDate> = None;
    for entry in state.completion_log.iter() {
        let log = entry.value();
        if log.user_id != user_id || log.action != "completed" {
            continue;
        }
        total += 1;
        let date = log.created_at.date_naive();
        days.insert(date);
        if last.is_none_or(|current| date > current) {
            last = Some(date);
        }
    }
    let today = Utc::now().date_naive();
    let mut current: u32 = 0;
    // A streak ending today counts from today; when nothing was
    // completed today yet, it counts from yesterday (the streak is
    // still alive until the day ends).
    let mut cursor = if days.contains(&today) {
        today
    } else {
        today - chrono::Duration::days(1)
    };
    while days.contains(&cursor) {
        current += 1;
        cursor -= chrono::Duration::days(1);
    }
    let mut longest: u32 = 0;
    let mut sorted: Vec<NaiveDate> = days.into_iter().collect();
    sorted.sort();
    let mut run: u32 = 0;
    let mut previous: Option<NaiveDate> = None;
    for date in sorted {
        if previous.is_some_and(|prev| date == prev + chrono::Duration::days(1)) {
            run += 1;
        } else {
            run = 1;
        }
        longest = longest.max(run);
        previous = Some(date);
    }
    StreaksView {
        user_id: user_id.to_string(),
        current_streak_days: current,
        longest_streak_days: longest,
        total_completions: total,
        last_completion_date: last.map(|date| date.to_string()),
        computed_at: Utc::now(),
    }
}

// ---------------------------------------------------------------------------
// Helpers: notifications and scheduler linkage.
// ---------------------------------------------------------------------------

/// Ask the notifications module for one notification, using the exact
/// payload shape `on_notification_requested` reads in
/// `notifications.rs`: `user_id` (or `user_ids` / `audience`),
/// `title`, `body`, `kind`, `category`, `module`, `priority`, `link`.
/// `category` must be one of `NOTIFICATION_CATEGORIES` there; there
/// is no todos category, so reminders use `system`.
fn request_notification(bus: &HookBus, user_id: &str, title: String, body: String, kind: &str, priority: &str, link: String) {
    bus.emit(
        "notification.requested",
        json!({
            "user_id": user_id,
            "title": title,
            "body": body,
            "kind": kind,
            "category": "system",
            "module": "todos",
            "priority": priority,
            "link": link,
            "actor_id": user_id,
        }),
    );
}

/// Link one reminder to the scheduler. The scheduler listens to the
/// `scheduler.enqueue` event with the shape `{"kind", "payload"}`.
/// The result is read back from the first hook result:
/// `{"queued": true, "job_id": ...}` means the scheduler accepted
/// the job. See the module header: the kind is not in the scheduler's
/// `KNOWN_KINDS` yet, so today this records `declined` instead of
/// pretending the reminder was scheduled.
fn schedule_reminder(ctx: &AppContext, reminder: &mut TodoReminder) {
    let answer = ctx.hooks.emit(
        "scheduler.enqueue",
        json!({
            "kind": SCHEDULER_JOB_KIND_REMINDERS,
            "payload": {
                "todo_id": reminder.todo_id.to_string(),
                "reminder_id": reminder.id.to_string(),
                "user_id": reminder.owner_id,
                "remind_at": reminder.remind_at.to_rfc3339(),
                "offset_minutes": reminder.offset_minutes,
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            reminder.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            reminder.scheduler_status = "queued".to_string();
        }
        _ => {
            reminder.scheduler_job_id = None;
            reminder.scheduler_status = "declined".to_string();
        }
    }
}

/// Create one reminder row for a todo that has a due date, enqueue
/// it with the scheduler, store it, and emit the creation event.
/// A reminder without a due date is refused by the caller (and by
/// the migration CHECK on the denormalized `due_at`).
fn create_reminder(
    ctx: &AppContext,
    state: &TodosState,
    todo: &Todo,
    offset_minutes: i64,
) -> UnifiedResult<TodoReminder> {
    validate_reminder_offset(offset_minutes)?;
    let due_at = todo.due_at.ok_or_else(|| {
        UnifiedError::bad_request("A reminder requires the todo to have a due date (due_at)")
    })?;
    if state
        .reminders
        .iter()
        .filter(|entry| entry.value().todo_id == todo.id && entry.value().status == "pending")
        .count()
        >= MAX_REMINDERS_PER_TODO
    {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_REMINDERS_PER_TODO} pending reminders per todo"
        )));
    }
    let now = Utc::now();
    let mut reminder = TodoReminder {
        id: Uuid::now_v7(),
        todo_id: todo.id,
        owner_id: todo.owner_id.clone(),
        offset_minutes,
        remind_at: due_at - chrono::Duration::minutes(offset_minutes),
        due_at,
        status: "pending".to_string(),
        scheduler_job_id: None,
        scheduler_status: "not_scheduled".to_string(),
        sent_at: None,
        created_at: now,
        updated_at: now,
    };
    schedule_reminder(ctx, &mut reminder);
    state.reminders.insert(reminder.id, reminder.clone());
    state.emit(
        EVENT_REMINDER_CREATED,
        json!({
            "reminder_id": reminder.id.to_string(),
            "todo_id": todo.id.to_string(),
            "user_id": todo.owner_id,
            "offset_minutes": offset_minutes,
            "remind_at": reminder.remind_at.to_rfc3339(),
            "scheduler_job_id": reminder.scheduler_job_id,
            "scheduler_status": reminder.scheduler_status,
            "actor_id": todo.owner_id,
            "resource": format!("todo:{}", todo.id),
        }),
    );
    Ok(reminder)
}

/// Create the next occurrence of a recurring todo after the current
/// one was completed. Returns `None` when the series is finished
/// (count exhausted or past `until`). The new occurrence is always
/// `open` and is never auto-completed.
fn create_next_occurrence(state: &TodosState, done: &Todo) -> Option<Todo> {
    let frequency = done.recurrence_frequency?;
    let due_at = done.due_at?;
    // `None` means "repeat until `recurrence_until`" (unbounded by
    // count); only an explicit `Some(0)` ends the series.
    if done.recurrence_remaining == Some(0) {
        return None;
    }
    let next_due = advance_due(due_at, frequency, done.recurrence_interval.max(1));
    if let Some(until) = done.recurrence_until {
        if next_due > until {
            return None;
        }
    }
    let now = Utc::now();
    let next = Todo {
        id: Uuid::now_v7(),
        list_id: done.list_id,
        owner_id: done.owner_id.clone(),
        title: done.title.clone(),
        notes: done.notes.clone(),
        due_at: Some(next_due),
        timezone: done.timezone.clone(),
        priority: done.priority,
        status: TodoStatus::Open,
        tags: done.tags.clone(),
        subtasks: Vec::new(),
        attachments: Vec::new(),
        recurrence_frequency: Some(frequency),
        recurrence_interval: done.recurrence_interval,
        recurrence_count: done.recurrence_count,
        recurrence_until: done.recurrence_until,
        recurrence_remaining: done.recurrence_remaining.map(|left| left - 1),
        recurrence_group_id: done
            .recurrence_group_id
            .or(Some(done.id)),
        position: next_position(state, done.list_id),
        completed_at: None,
        created_at: now,
        updated_at: now,
    };
    state.todos.insert(next.id, next.clone());
    state.emit(
        EVENT_TODO_CREATED,
        json!({
            "todo_id": next.id.to_string(),
            "id": next.id.to_string(),
            "list_id": next.list_id.to_string(),
            "owner_id": next.owner_id,
            "title": next.title,
            "status": "open",
            "priority": next.priority.as_str(),
            "due_at": next.due_at,
            "recurrence_group_id": next.recurrence_group_id.map(|id| id.to_string()),
            "recurs_from": done.id.to_string(),
            "actor_id": done.owner_id,
            "entity_type": "todo",
            "entity_id": next.id.to_string(),
            "resource": format!("todo:{}", next.id),
        }),
    );
    Some(next)
}

// ---------------------------------------------------------------------------
// List handlers.
// ---------------------------------------------------------------------------

async fn list_lists(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TodoList>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        let params = query.into_inner();
        let mut items = readable_lists(&state, &caller.actor);
        if let Some(archived) = params.filters.get("archived") {
            let want = archived == "true" || archived == "1";
            items.retain(|list| list.archived == want);
        }
        if let Some(owner) = params.filters.get("owner_id") {
            items.retain(|list| &list.owner_id == owner);
        }
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        if !search.is_empty() {
            items.retain(|list| list.name.to_lowercase().contains(&search));
        }
        let sort = params.allowed_sort(&["name", "sort_order", "created_at"], "sort_order");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.cmp(&b.name),
                "created_at" => a.created_at.cmp(&b.created_at),
                _ => a.sort_order.cmp(&b.sort_order),
            };
            if descending { ordering.reverse() } else { ordering }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn create_list(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    body: web::Json<CreateListRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoList> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_LIST_NAME_LEN)?;
        let color = match &input.color {
            Some(raw) => validate_color(raw)?,
            None => "blue".to_string(),
        };
        let icon = match &input.icon {
            Some(raw) => validate_icon(raw)?,
            None => "folder".to_string(),
        };
        let now = Utc::now();
        let list = TodoList {
            id: Uuid::now_v7(),
            owner_id: caller.actor.clone(),
            name,
            color,
            icon,
            archived: false,
            sort_order: input.sort_order.unwrap_or(0),
            is_default: false,
            created_at: now,
            updated_at: now,
        };
        state.lists.insert(list.id, list.clone());
        state.emit(
            EVENT_LIST_CREATED,
            json!({
                "list_id": list.id.to_string(),
                "owner_id": caller.actor,
                "name": list.name,
                "color": list.color,
                "icon": list.icon,
                "actor_id": caller.actor,
                "resource": format!("todo_list:{}", list.id),
            }),
        );
        Ok(list)
    })();
    match result {
        Ok(list) => created(list),
        Err(error) => err(error, debug),
    }
}

async fn get_list(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoList> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        load_list_for_read(&state, id.into_inner(), &caller.actor)
    })();
    match result {
        Ok(list) => ok(list),
        Err(error) => err(error, debug),
    }
}

async fn update_list(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateListRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoList> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let list_id = id.into_inner();
        let input = body.into_inner();
        let mut list = load_list_owned(&state, list_id, &caller.actor)?;
        if let Some(name) = &input.name {
            list.name = require_text(name, "name", MAX_LIST_NAME_LEN)?;
        }
        if let Some(color) = &input.color {
            list.color = validate_color(color)?;
        }
        if let Some(icon) = &input.icon {
            list.icon = validate_icon(icon)?;
        }
        if let Some(order) = input.sort_order {
            list.sort_order = order;
        }
        list.updated_at = Utc::now();
        state.lists.insert(list.id, list.clone());
        state.emit(
            EVENT_LIST_UPDATED,
            json!({
                "list_id": list.id.to_string(),
                "owner_id": list.owner_id,
                "name": list.name,
                "actor_id": caller.actor,
                "resource": format!("todo_list:{}", list.id),
            }),
        );
        Ok(list)
    })();
    match result {
        Ok(list) => ok(list),
        Err(error) => err(error, debug),
    }
}

async fn delete_list(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let list_id = id.into_inner();
        let list = load_list_owned(&state, list_id, &caller.actor)?;
        if list.is_default {
            return Err(UnifiedError::bad_request(
                "The default list cannot be deleted: move its todos to another list first",
            ));
        }
        if state.todos.iter().any(|entry| entry.value().list_id == list_id) {
            return Err(UnifiedError::bad_request(
                "The list still contains todos: move or delete them first",
            ));
        }
        state.lists.remove(&list_id);
        state
            .shares
            .retain(|_, share| share.list_id != list_id);
        state.emit(
            EVENT_LIST_DELETED,
            json!({
                "list_id": list_id.to_string(),
                "owner_id": list.owner_id,
                "actor_id": caller.actor,
                "resource": format!("todo_list:{list_id}"),
            }),
        );
        Ok(json!({ "deleted": true, "id": list_id }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

async fn set_list_archived(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    archived: bool,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoList> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let list_id = id.into_inner();
        let mut list = load_list_owned(&state, list_id, &caller.actor)?;
        list.archived = archived;
        list.updated_at = Utc::now();
        state.lists.insert(list.id, list.clone());
        state.emit(
            if archived { EVENT_LIST_ARCHIVED } else { EVENT_LIST_UNARCHIVED },
            json!({
                "list_id": list.id.to_string(),
                "owner_id": list.owner_id,
                "archived": archived,
                "actor_id": caller.actor,
                "resource": format!("todo_list:{}", list.id),
            }),
        );
        Ok(list)
    })();
    match result {
        Ok(list) => ok(list),
        Err(error) => err(error, debug),
    }
}

async fn archive_list(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    set_list_archived(ctx, state, req, id, true).await
}

async fn unarchive_list(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    set_list_archived(ctx, state, req, id, false).await
}

async fn list_shares(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<TodoShare>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        let list_id = id.into_inner();
        let _list = load_list_owned(&state, list_id, &caller.actor)?;
        let mut shares: Vec<TodoShare> = state
            .shares
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|share| share.list_id == list_id)
            .collect();
        shares.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(shares)
    })();
    match result {
        Ok(shares) => ok(shares),
        Err(error) => err(error, debug),
    }
}

async fn share_list(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<ShareListRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoShare> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_SHARE)?;
        let list_id = id.into_inner();
        let input = body.into_inner();
        let list = load_list_owned(&state, list_id, &caller.actor)?;
        let target = input.user_id.trim().to_string();
        if target.is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        if target == caller.actor {
            return Err(UnifiedError::bad_request(
                "A list cannot be shared with its own owner",
            ));
        }
        let permission = input.permission.unwrap_or(SharePermission::Read);
        let now = Utc::now();
        let share = TodoShare {
            id: Uuid::now_v7(),
            list_id,
            owner_id: list.owner_id.clone(),
            shared_with_user_id: target.clone(),
            permission,
            created_at: now,
            updated_at: now,
        };
        state
            .shares
            .insert(share_key(list_id, &target), share.clone());
        state.emit(
            EVENT_LIST_SHARED,
            json!({
                "list_id": list_id.to_string(),
                "owner_id": list.owner_id,
                "shared_with_user_id": target,
                "permission": permission.as_str(),
                "actor_id": caller.actor,
                "resource": format!("todo_list:{list_id}"),
            }),
        );
        Ok(share)
    })();
    match result {
        Ok(share) => created(share),
        Err(error) => err(error, debug),
    }
}

async fn revoke_share(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_SHARE)?;
        let (list_id, target) = path.into_inner();
        let list = load_list_owned(&state, list_id, &caller.actor)?;
        let key = share_key(list_id, target.trim());
        match state.shares.remove(&key) {
            Some(_) => {
                state.emit(
                    EVENT_LIST_SHARE_REVOKED,
                    json!({
                        "list_id": list_id.to_string(),
                        "owner_id": list.owner_id,
                        "shared_with_user_id": target,
                        "actor_id": caller.actor,
                        "resource": format!("todo_list:{list_id}"),
                    }),
                );
                Ok(json!({ "revoked": true, "list_id": list_id, "user_id": target }))
            }
            None => Err(not_found_list(list_id)),
        }
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

async fn list_shared_with_me(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .shares
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|share| share.shared_with_user_id == caller.actor)
            .filter_map(|share| {
                state.lists.get(&share.list_id).map(|entry| {
                    let list = entry.value().clone();
                    json!({
                        "list": list,
                        "permission": share.permission.as_str(),
                        "owner_id": share.owner_id,
                        "shared_at": share.created_at,
                    })
                })
            })
            .collect();
        items.sort_by(|a, b| {
            a.get("list")
                .and_then(|list| list.get("name"))
                .and_then(Value::as_str)
                .cmp(
                    &b.get("list")
                        .and_then(|list| list.get("name"))
                        .and_then(Value::as_str),
                )
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Todo handlers.
// ---------------------------------------------------------------------------

async fn list_todos(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TodoResponse>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        let params = query.into_inner();
        let now = Utc::now();
        // A list filter names exactly one list: it must be readable,
        // and a stranger's list answers 404 rather than an empty page.
        let list_filter = match params.filters.get("list_id") {
            Some(raw) => {
                let list_id = Uuid::parse_str(raw).map_err(|_| {
                    UnifiedError::bad_request(format!("Invalid list_id: {raw}"))
                })?;
                Some(load_list_for_read(&state, list_id, &caller.actor)?)
            }
            None => None,
        };
        let status_filter = match params.filters.get("status") {
            Some(raw) => Some(match raw.as_str() {
                "open" => TodoStatus::Open,
                "done" => TodoStatus::Done,
                _ => {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown todo status: \"{raw}\" (open or done)"
                    )))
                }
            }),
            None => None,
        };
        let priority_filter = match params.filters.get("priority") {
            Some(raw) => Some(match raw.as_str() {
                "low" => TodoPriority::Low,
                "normal" => TodoPriority::Normal,
                "high" => TodoPriority::High,
                "urgent" => TodoPriority::Urgent,
                _ => {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown todo priority: \"{raw}\""
                    )))
                }
            }),
            None => None,
        };
        let tag_filter = params.filters.get("tag").cloned();
        let overdue_filter = params
            .filters
            .get("overdue")
            .and_then(|value| value.parse::<bool>().ok());
        let due_before = match params.filters.get("due_before") {
            Some(raw) => Some(raw.parse::<DateTime<Utc>>().map_err(|_| {
                UnifiedError::bad_request(format!(
                    "Filter \"due_before\" must be an RFC 3339 date-time, got \"{raw}\""
                ))
            })?),
            None => None,
        };
        let due_after = match params.filters.get("due_after") {
            Some(raw) => Some(raw.parse::<DateTime<Utc>>().map_err(|_| {
                UnifiedError::bad_request(format!(
                    "Filter \"due_after\" must be an RFC 3339 date-time, got \"{raw}\""
                ))
            })?),
            None => None,
        };
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<TodoResponse> = state
            .todos
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|todo| {
                // Only own todos and todos in lists shared with the
                // caller are ever visible here.
                let Ok(list) = load_list(&state, todo.list_id) else {
                    return false;
                };
                if !can_read_list(&state, &list, &caller.actor) {
                    return false;
                }
                if let Some(list) = &list_filter {
                    if todo.list_id != list.id {
                        return false;
                    }
                }
                if let Some(status) = status_filter {
                    if todo.status != status {
                        return false;
                    }
                }
                if let Some(priority) = priority_filter {
                    if todo.priority != priority {
                        return false;
                    }
                }
                if let Some(tag) = &tag_filter {
                    if !todo.tags.iter().any(|item| item == tag) {
                        return false;
                    }
                }
                if let Some(want_overdue) = overdue_filter {
                    if todo.is_overdue(now) != want_overdue {
                        return false;
                    }
                }
                if let Some(before) = due_before {
                    match todo.due_at {
                        Some(due) if due <= before => {}
                        _ => return false,
                    }
                }
                if let Some(after) = due_after {
                    match todo.due_at {
                        Some(due) if due >= after => {}
                        _ => return false,
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        todo.title,
                        todo.notes.clone().unwrap_or_default(),
                        todo.tags.join(" ")
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|todo| {
                let list = load_list(&state, todo.list_id)
                    .expect("list exists: filtered above");
                let can_edit = can_edit_list(&state, &list, &caller.actor);
                TodoResponse::new(&todo, can_edit)
            })
            .collect();
        let sort = params.allowed_sort(
            &["title", "due_at", "priority", "position", "created_at"],
            "position",
        );
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a.todo.title.cmp(&b.todo.title),
                "due_at" => a.todo.due_at.cmp(&b.todo.due_at),
                "priority" => a.todo.priority.rank().cmp(&b.todo.priority.rank()),
                "created_at" => a.todo.created_at.cmp(&b.todo.created_at),
                _ => a.todo.position.cmp(&b.todo.position),
            };
            if descending { ordering.reverse() } else { ordering }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn create_todo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    body: web::Json<CreateTodoRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<(TodoResponse, Vec<TodoReminder>)> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        let timezone = match &input.timezone {
            Some(raw) => {
                let value = require_text(raw, "timezone", MAX_TIMEZONE_LEN)?;
                if !valid_timezone(&value) {
                    return Err(UnifiedError::bad_request(format!(
                        "Invalid timezone: {value}. Use an IANA name such as \"Asia/Jakarta\" or \"UTC\""
                    )));
                }
                value
            }
            None => "UTC".to_string(),
        };
        let tags = validate_tags(&input.tags.unwrap_or_default())?;
        // Quick capture: no list given means the default Inbox list.
        // A list that is only shared with the caller (not owned) can
        // hold a new todo only with edit access — and the todo still
        // belongs to the list owner, never to the caller.
        let list = match input.list_id {
            Some(list_id) => load_list_for_edit(&state, list_id, &caller.actor)?,
            None => ensure_default_list(&state, &caller.actor),
        };
        let frequency = input.recurrence_frequency;
        let interval = input.recurrence_interval.unwrap_or(1);
        validate_recurrence(
            frequency,
            interval,
            input.recurrence_count,
            input.recurrence_until,
            input.due_at,
        )?;
        if let Some(due) = input.due_at {
            if let Some(until) = input.recurrence_until {
                if until <= due {
                    return Err(UnifiedError::bad_request(
                        "recurrence_until must be after due_at",
                    ));
                }
            }
        }
        let offsets = input.reminder_offsets_minutes.unwrap_or_default();
        for offset in &offsets {
            validate_reminder_offset(*offset)?;
        }
        if !offsets.is_empty() && input.due_at.is_none() {
            return Err(UnifiedError::bad_request(
                "reminder_offsets_minutes requires due_at: a reminder needs a due date",
            ));
        }
        if offsets.len() > MAX_REMINDERS_PER_TODO {
            return Err(UnifiedError::bad_request(format!(
                "At most {MAX_REMINDERS_PER_TODO} reminders per todo"
            )));
        }
        let now = Utc::now();
        let remaining = match (frequency, input.recurrence_count) {
            (Some(_), Some(count)) => Some(count.saturating_sub(1)),
            (Some(_), None) => None,
            (None, _) => None,
        };
        let group_id = frequency.map(|_| Uuid::now_v7());
        let todo = Todo {
            id: Uuid::now_v7(),
            list_id: list.id,
            // Ownership: the todo belongs to the LIST owner, not to
            // the caller — for an owned list those are the same user.
            owner_id: list.owner_id.clone(),
            title,
            notes,
            due_at: input.due_at,
            timezone,
            priority: input.priority.unwrap_or_default(),
            status: TodoStatus::Open,
            tags,
            subtasks: Vec::new(),
            attachments: Vec::new(),
            recurrence_frequency: frequency,
            recurrence_interval: interval,
            recurrence_count: input.recurrence_count,
            recurrence_until: input.recurrence_until,
            recurrence_remaining: remaining,
            recurrence_group_id: group_id,
            position: input
                .position
                .unwrap_or_else(|| next_position(&state, list.id)),
            completed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.todos.insert(todo.id, todo.clone());
        let mut reminders = Vec::new();
        for offset in offsets {
            reminders.push(create_reminder(&ctx, &state, &todo, offset)?);
        }
        state.emit(EVENT_TODO_CREATED, todo_event_payload(&todo, &caller.actor));
        Ok((TodoResponse::new(&todo, true), reminders))
    })();
    match result {
        Ok((todo, _reminders)) => created(todo),
        Err(error) => err(error, debug),
    }
}

async fn get_todo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoResponse> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        let (todo, can_edit) = load_todo_for_read(&state, id.into_inner(), &caller.actor)?;
        Ok(TodoResponse::new(&todo, can_edit))
    })();
    match result {
        Ok(todo) => ok(todo),
        Err(error) => err(error, debug),
    }
}

async fn update_todo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateTodoRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoResponse> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let todo_id = id.into_inner();
        let input = body.into_inner();
        let mut todo = load_todo_for_edit(&state, todo_id, &caller.actor)?;
        if let Some(title) = &input.title {
            todo.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(notes) = &input.notes {
            todo.notes = match notes {
                Some(raw) => Some(require_text(raw, "notes", MAX_NOTES_LEN)?),
                None => None,
            };
        }
        if let Some(list_id) = input.list_id {
            // Moving a todo to another list: the target must be
            // editable by the caller, and the todo keeps its owner
            // only when the target list has the same owner; moving
            // into a list owned by someone else is refused, because
            // ownership never transfers.
            let target = load_list_for_edit(&state, list_id, &caller.actor)?;
            if target.owner_id != todo.owner_id {
                return Err(UnifiedError::bad_request(
                    "A todo cannot be moved into a list owned by another user: ownership never transfers",
                ));
            }
            todo.list_id = list_id;
        }
        if let Some(due_at) = input.due_at {
            todo.due_at = due_at;
        }
        if let Some(timezone) = &input.timezone {
            let value = require_text(timezone, "timezone", MAX_TIMEZONE_LEN)?;
            if !valid_timezone(&value) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid timezone: {value}. Use an IANA name such as \"Asia/Jakarta\" or \"UTC\""
                )));
            }
            todo.timezone = value;
        }
        if let Some(priority) = input.priority {
            todo.priority = priority;
        }
        if let Some(tags) = &input.tags {
            todo.tags = validate_tags(tags)?;
        }
        if let Some(position) = input.position {
            todo.position = position;
        }
        // Recurrence edits re-validate against the (possibly new)
        // due date; clearing the frequency clears the rule.
        let next_frequency = match input.recurrence_frequency {
            Some(value) => value,
            None => todo.recurrence_frequency,
        };
        let next_interval = input
            .recurrence_interval
            .unwrap_or(todo.recurrence_interval);
        let next_count = match input.recurrence_count {
            Some(value) => value,
            None => todo.recurrence_count,
        };
        let next_until = match input.recurrence_until {
            Some(value) => value,
            None => todo.recurrence_until,
        };
        validate_recurrence(
            next_frequency,
            next_interval,
            next_count,
            next_until,
            todo.due_at,
        )?;
        todo.recurrence_frequency = next_frequency;
        todo.recurrence_interval = next_interval;
        todo.recurrence_count = next_count;
        todo.recurrence_until = next_until;
        if next_frequency.is_some() && todo.recurrence_group_id.is_none() {
            todo.recurrence_group_id = Some(Uuid::now_v7());
        }
        if next_frequency.is_none() {
            todo.recurrence_remaining = None;
            todo.recurrence_group_id = None;
        }
        // A direct status change through PATCH is allowed only as a
        // plain flip (open <-> done) and still writes the completion
        // log, so history and streaks stay true. Recurrence is NOT
        // advanced here: only POST /complete creates the next
        // occurrence.
        if let Some(status) = input.status {
            if status != todo.status {
                if status == TodoStatus::Done {
                    todo.completed_at = Some(Utc::now());
                    log_completion(&state, todo.id, &caller.actor, "completed");
                } else {
                    todo.completed_at = None;
                    log_completion(&state, todo.id, &caller.actor, "reopened");
                }
                todo.status = status;
            }
        }
        todo.updated_at = Utc::now();
        state.todos.insert(todo.id, todo.clone());
        state.emit(EVENT_TODO_UPDATED, todo_event_payload(&todo, &caller.actor));
        Ok(TodoResponse::new(&todo, true))
    })();
    match result {
        Ok(todo) => ok(todo),
        Err(error) => err(error, debug),
    }
}

async fn delete_todo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let todo_id = id.into_inner();
        let todo = load_todo_for_edit(&state, todo_id, &caller.actor)?;
        state.todos.remove(&todo_id);
        state
            .reminders
            .retain(|_, reminder| reminder.todo_id != todo_id);
        state.emit(EVENT_TODO_DELETED, todo_event_payload(&todo, &caller.actor));
        Ok(json!({ "deleted": true, "id": todo_id }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

async fn complete_todo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let todo_id = id.into_inner();
        let mut todo = load_todo_for_edit(&state, todo_id, &caller.actor)?;
        if todo.status == TodoStatus::Done {
            return Err(UnifiedError::unprocessable(format!(
                "Todo {todo_id} is already done"
            )));
        }
        todo.status = TodoStatus::Done;
        todo.completed_at = Some(Utc::now());
        todo.updated_at = Utc::now();
        state.todos.insert(todo.id, todo.clone());
        log_completion(&state, todo.id, &caller.actor, "completed");
        // Completing one occurrence creates the next one (when the
        // series allows it). The future occurrence stays open: it is
        // never auto-completed.
        let next = create_next_occurrence(&state, &todo);
        // Pending reminders of the completed occurrence no longer
        // fire: a done todo needs no due-date reminder.
        for mut entry in state.reminders.iter_mut() {
            if entry.value().todo_id == todo_id && entry.value().status == "pending" {
                entry.value_mut().status = "cancelled".to_string();
                entry.value_mut().updated_at = Utc::now();
            }
        }
        let mut payload = todo_event_payload(&todo, &caller.actor);
        if let Value::Object(map) = &mut payload {
            map.insert(
                "next_occurrence_id".to_string(),
                next.as_ref()
                    .map(|item| Value::String(item.id.to_string()))
                    .unwrap_or(Value::Null),
            );
        }
        state.emit(EVENT_TODO_COMPLETED, payload);
        Ok(json!({
            "todo": TodoResponse::new(&todo, true),
            "next_occurrence": next.map(|item| TodoResponse::new(&item, true)),
        }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

async fn reopen_todo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoResponse> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let todo_id = id.into_inner();
        let mut todo = load_todo_for_edit(&state, todo_id, &caller.actor)?;
        if todo.status == TodoStatus::Open {
            return Err(UnifiedError::unprocessable(format!(
                "Todo {todo_id} is already open"
            )));
        }
        todo.status = TodoStatus::Open;
        todo.completed_at = None;
        todo.updated_at = Utc::now();
        state.todos.insert(todo.id, todo.clone());
        log_completion(&state, todo.id, &caller.actor, "reopened");
        state.emit(EVENT_TODO_REOPENED, todo_event_payload(&todo, &caller.actor));
        Ok(TodoResponse::new(&todo, true))
    })();
    match result {
        Ok(todo) => ok(todo),
        Err(error) => err(error, debug),
    }
}

async fn list_completed_todos(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TodoResponse>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        let params = query.into_inner();
        let list_filter = match params.filters.get("list_id") {
            Some(raw) => {
                let list_id = Uuid::parse_str(raw).map_err(|_| {
                    UnifiedError::bad_request(format!("Invalid list_id: {raw}"))
                })?;
                Some(load_list_for_read(&state, list_id, &caller.actor)?)
            }
            None => None,
        };
        let mut items: Vec<TodoResponse> = state
            .todos
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|todo| todo.status == TodoStatus::Done)
            .filter(|todo| {
                let Ok(list) = load_list(&state, todo.list_id) else {
                    return false;
                };
                if !can_read_list(&state, &list, &caller.actor) {
                    return false;
                }
                if let Some(list) = &list_filter {
                    if todo.list_id != list.id {
                        return false;
                    }
                }
                true
            })
            .map(|todo| {
                let list = load_list(&state, todo.list_id)
                    .expect("list exists: filtered above");
                let can_edit = can_edit_list(&state, &list, &caller.actor);
                TodoResponse::new(&todo, can_edit)
            })
            .collect();
        // Most recently completed first.
        items.sort_by(|a, b| {
            b.todo
                .completed_at
                .cmp(&a.todo.completed_at)
                .then(b.todo.updated_at.cmp(&a.todo.updated_at))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn list_overdue_todos(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TodoResponse>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        let params = query.into_inner();
        let now = Utc::now();
        let mut items: Vec<TodoResponse> = state
            .todos
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|todo| todo.is_overdue(now))
            .filter(|todo| {
                let Ok(list) = load_list(&state, todo.list_id) else {
                    return false;
                };
                can_read_list(&state, &list, &caller.actor)
            })
            .map(|todo| {
                let list = load_list(&state, todo.list_id)
                    .expect("list exists: filtered above");
                let can_edit = can_edit_list(&state, &list, &caller.actor);
                TodoResponse::new(&todo, can_edit)
            })
            .collect();
        // Most overdue first.
        items.sort_by(|a, b| a.todo.due_at.cmp(&b.todo.due_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn get_streaks(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StreaksView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        Ok(compute_streaks(&state, &caller.actor))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn list_todo_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TodoCompletionLog>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        let todo_id = id.into_inner();
        let (todo, _) = load_todo_for_read(&state, todo_id, &caller.actor)?;
        let params = query.into_inner();
        let mut items: Vec<TodoCompletionLog> = state
            .completion_log
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|log| log.todo_id == todo.id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Subtask handlers (one level only).
// ---------------------------------------------------------------------------

async fn add_subtask(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<CreateSubtaskRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoSubtask> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let todo_id = id.into_inner();
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let mut todo = load_todo_for_edit(&state, todo_id, &caller.actor)?;
        if todo.subtasks.len() >= MAX_SUBTASKS {
            return Err(UnifiedError::bad_request(format!(
                "At most {MAX_SUBTASKS} subtasks per todo"
            )));
        }
        let now = Utc::now();
        let position = input.position.unwrap_or_else(|| {
            todo.subtasks
                .iter()
                .map(|item| item.position)
                .max()
                .map_or(0, |max| max + 1)
        });
        let subtask = TodoSubtask {
            id: Uuid::now_v7(),
            todo_id,
            title,
            done: false,
            position,
            completed_at: None,
            created_at: now,
            updated_at: now,
        };
        todo.subtasks.push(subtask.clone());
        todo.updated_at = now;
        state.todos.insert(todo.id, todo.clone());
        state.emit(
            EVENT_SUBTASK_CREATED,
            json!({
                "subtask_id": subtask.id.to_string(),
                "todo_id": todo_id.to_string(),
                "owner_id": todo.owner_id,
                "title": subtask.title,
                "actor_id": caller.actor,
                "resource": format!("todo:{todo_id}"),
            }),
        );
        Ok(subtask)
    })();
    match result {
        Ok(subtask) => created(subtask),
        Err(error) => err(error, debug),
    }
}

async fn update_subtask(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<UpdateSubtaskRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoSubtask> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let (todo_id, subtask_id) = path.into_inner();
        let input = body.into_inner();
        let mut todo = load_todo_for_edit(&state, todo_id, &caller.actor)?;
        let subtask = todo
            .subtasks
            .iter_mut()
            .find(|item| item.id == subtask_id)
            .ok_or_else(|| UnifiedError::not_found(format!("Subtask not found: {subtask_id}")))?;
        if let Some(title) = &input.title {
            subtask.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        subtask.updated_at = Utc::now();
        let updated = subtask.clone();
        todo.updated_at = Utc::now();
        state.todos.insert(todo.id, todo.clone());
        state.emit(
            EVENT_SUBTASK_UPDATED,
            json!({
                "subtask_id": subtask_id.to_string(),
                "todo_id": todo_id.to_string(),
                "owner_id": todo.owner_id,
                "title": updated.title,
                "actor_id": caller.actor,
                "resource": format!("todo:{todo_id}"),
            }),
        );
        Ok(updated)
    })();
    match result {
        Ok(subtask) => ok(subtask),
        Err(error) => err(error, debug),
    }
}

async fn toggle_subtask(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoSubtask> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let (todo_id, subtask_id) = path.into_inner();
        let mut todo = load_todo_for_edit(&state, todo_id, &caller.actor)?;
        let subtask = todo
            .subtasks
            .iter_mut()
            .find(|item| item.id == subtask_id)
            .ok_or_else(|| UnifiedError::not_found(format!("Subtask not found: {subtask_id}")))?;
        subtask.done = !subtask.done;
        subtask.completed_at = if subtask.done { Some(Utc::now()) } else { None };
        subtask.updated_at = Utc::now();
        let updated = subtask.clone();
        todo.updated_at = Utc::now();
        state.todos.insert(todo.id, todo.clone());
        state.emit(
            EVENT_SUBTASK_TOGGLED,
            json!({
                "subtask_id": subtask_id.to_string(),
                "todo_id": todo_id.to_string(),
                "owner_id": todo.owner_id,
                "done": updated.done,
                "actor_id": caller.actor,
                "resource": format!("todo:{todo_id}"),
            }),
        );
        Ok(updated)
    })();
    match result {
        Ok(subtask) => ok(subtask),
        Err(error) => err(error, debug),
    }
}

async fn delete_subtask(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let (todo_id, subtask_id) = path.into_inner();
        let mut todo = load_todo_for_edit(&state, todo_id, &caller.actor)?;
        let before = todo.subtasks.len();
        todo.subtasks.retain(|item| item.id != subtask_id);
        if todo.subtasks.len() == before {
            return Err(UnifiedError::not_found(format!(
                "Subtask not found: {subtask_id}"
            )));
        }
        todo.updated_at = Utc::now();
        state.todos.insert(todo.id, todo.clone());
        state.emit(
            EVENT_SUBTASK_DELETED,
            json!({
                "subtask_id": subtask_id.to_string(),
                "todo_id": todo_id.to_string(),
                "owner_id": todo.owner_id,
                "actor_id": caller.actor,
                "resource": format!("todo:{todo_id}"),
            }),
        );
        Ok(json!({ "deleted": true, "id": subtask_id }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Attachment handlers (metadata only — bytes live in storage).
// ---------------------------------------------------------------------------

async fn add_attachment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<AddAttachmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoAttachment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let todo_id = id.into_inner();
        let input = body.into_inner();
        let storage_target = require_text(&input.storage_target, "storage_target", 128)?;
        let storage_key = require_text(&input.storage_key, "storage_key", MAX_STORAGE_KEY_LEN)?;
        let file_name = match &input.file_name {
            Some(raw) => Some(require_text(raw, "file_name", MAX_FILE_NAME_LEN)?),
            None => None,
        };
        let mut todo = load_todo_for_edit(&state, todo_id, &caller.actor)?;
        if todo.attachments.len() >= MAX_ATTACHMENTS {
            return Err(UnifiedError::bad_request(format!(
                "At most {MAX_ATTACHMENTS} attachments per todo"
            )));
        }
        let attachment = TodoAttachment {
            id: Uuid::now_v7(),
            todo_id,
            storage_target,
            storage_key,
            file_name,
            size_bytes: input.size_bytes,
            created_at: Utc::now(),
        };
        todo.attachments.push(attachment.clone());
        todo.updated_at = Utc::now();
        state.todos.insert(todo.id, todo.clone());
        state.emit(
            EVENT_ATTACHMENT_ADDED,
            json!({
                "attachment_id": attachment.id.to_string(),
                "todo_id": todo_id.to_string(),
                "owner_id": todo.owner_id,
                "storage_target": attachment.storage_target,
                "storage_key": attachment.storage_key,
                "actor_id": caller.actor,
                "resource": format!("todo:{todo_id}"),
            }),
        );
        Ok(attachment)
    })();
    match result {
        Ok(attachment) => created(attachment),
        Err(error) => err(error, debug),
    }
}

async fn delete_attachment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let (todo_id, attachment_id) = path.into_inner();
        let mut todo = load_todo_for_edit(&state, todo_id, &caller.actor)?;
        let before = todo.attachments.len();
        todo.attachments.retain(|item| item.id != attachment_id);
        if todo.attachments.len() == before {
            return Err(UnifiedError::not_found(format!(
                "Attachment not found: {attachment_id}"
            )));
        }
        todo.updated_at = Utc::now();
        state.todos.insert(todo.id, todo.clone());
        state.emit(
            EVENT_ATTACHMENT_REMOVED,
            json!({
                "attachment_id": attachment_id.to_string(),
                "todo_id": todo_id.to_string(),
                "owner_id": todo.owner_id,
                "actor_id": caller.actor,
                "resource": format!("todo:{todo_id}"),
            }),
        );
        Ok(json!({ "deleted": true, "id": attachment_id }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Reminder handlers.
// ---------------------------------------------------------------------------

async fn create_todo_reminder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<CreateReminderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoReminder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let todo_id = id.into_inner();
        let todo = load_todo_for_edit(&state, todo_id, &caller.actor)?;
        create_reminder(&ctx, &state, &todo, body.offset_minutes)
    })();
    match result {
        Ok(reminder) => created(reminder),
        Err(error) => err(error, debug),
    }
}

async fn list_todo_reminders(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TodoReminder>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        let todo_id = id.into_inner();
        let (todo, _) = load_todo_for_read(&state, todo_id, &caller.actor)?;
        let params = query.into_inner();
        let mut items: Vec<TodoReminder> = state
            .reminders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|reminder| reminder.todo_id == todo.id)
            .collect();
        if let Some(status) = params.filters.get("status") {
            items.retain(|reminder| &reminder.status == status);
        }
        items.sort_by(|a, b| a.remind_at.cmp(&b.remind_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn cancel_todo_reminder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoReminder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let (todo_id, reminder_id) = path.into_inner();
        let _todo = load_todo_for_edit(&state, todo_id, &caller.actor)?;
        let mut reminder = state
            .reminders
            .get_mut(&reminder_id)
            .ok_or_else(|| UnifiedError::not_found(format!("Reminder not found: {reminder_id}")))?;
        if reminder.todo_id != todo_id {
            return Err(UnifiedError::not_found(format!(
                "Reminder not found: {reminder_id}"
            )));
        }
        if reminder.status == "sent" {
            return Err(UnifiedError::unprocessable(
                "A reminder that was already sent cannot be cancelled",
            ));
        }
        reminder.status = "cancelled".to_string();
        reminder.updated_at = Utc::now();
        let updated = reminder.clone();
        drop(reminder);
        state.emit(
            EVENT_REMINDER_CANCELLED,
            json!({
                "reminder_id": reminder_id.to_string(),
                "todo_id": todo_id.to_string(),
                "user_id": updated.owner_id,
                "actor_id": caller.actor,
                "resource": format!("todo:{todo_id}"),
            }),
        );
        Ok(updated)
    })();
    match result {
        Ok(reminder) => ok(reminder),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// AI handlers (drafts only).
// ---------------------------------------------------------------------------

/// Render the caller's own todos as grounding text for an AI draft
/// request. Only todos the caller OWNS are included — never todos
/// from a list shared with them, and never another user's todos.
fn todos_context_text(state: &TodosState, user_id: &str, list_id: Option<Uuid>, include_completed: bool, max_todos: usize) -> (String, Vec<Uuid>) {
    let mut owned: Vec<Todo> = state
        .todos
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|todo| todo.owner_id == user_id)
        .filter(|todo| include_completed || todo.status == TodoStatus::Open)
        .filter(|todo| list_id.is_none_or(|list| todo.list_id == list))
        .collect();
    owned.sort_by(|a, b| {
        a.due_at
            .cmp(&b.due_at)
            .then(b.priority.rank().cmp(&a.priority.rank()))
            .then(a.created_at.cmp(&b.created_at))
    });
    owned.truncate(max_todos.max(1).min(MAX_AI_TODOS));
    let mut lines: Vec<String> = Vec::with_capacity(owned.len());
    for todo in &owned {
        let due = todo
            .due_at
            .map(|due| due.to_rfc3339())
            .unwrap_or_else(|| "no due date".to_string());
        let notes = todo
            .notes
            .as_ref()
            .map(|notes| format!(" — {notes}"))
            .unwrap_or_default();
        let subtasks: Vec<String> = todo
            .subtasks
            .iter()
            .map(|item| {
                format!(
                    "[{}] {}",
                    if item.done { "x" } else { " " },
                    item.title
                )
            })
            .collect();
        let subtask_text = if subtasks.is_empty() {
            String::new()
        } else {
            format!(" (subtasks: {})", subtasks.join("; "))
        };
        lines.push(format!(
            "- [{}] {} (priority: {}, status: {}, due: {}, tags: {}){}{}",
            todo.id,
            todo.title,
            todo.priority.as_str(),
            todo.status.as_str(),
            due,
            todo.tags.join(", "),
            notes,
            subtask_text,
        ));
    }
    let text = lines.join("\n");
    let ids: Vec<Uuid> = owned.iter().map(|todo| todo.id).collect();
    (text, ids)
}

/// Shared draft-request flow for `/ai/summarize` and `/ai/plan-day`:
/// build context from the caller's own todos, emit
/// `todos.ai.draft_requested`, and return the first hook result that
/// carries a draft string. Fails closed (503) when no AI handler
/// answers — a summary is never fabricated locally.
fn request_ai_draft(
    ctx: &AppContext,
    caller: &Caller,
    kind: &str,
    context_text: String,
    todo_ids: Vec<Uuid>,
) -> UnifiedResult<AiDraftResponse> {
    let answer = ctx.hooks.emit(
        EVENT_AI_DRAFT_REQUESTED,
        json!({
            "user_id": caller.actor,
            "role": caller.role,
            "kind": kind,
            "todo_ids": todo_ids.iter().map(|id| id.to_string()).collect::<Vec<String>>(),
            "context_text": context_text,
            "requested_at": Utc::now().to_rfc3339(),
        }),
    );
    for result in &answer.results {
        let draft = result
            .get("draft")
            .and_then(Value::as_str)
            .or_else(|| result.get("text").and_then(Value::as_str));
        if let Some(draft) = draft {
            if draft.trim().is_empty() {
                continue;
            }
            return Ok(AiDraftResponse {
                kind: kind.to_string(),
                draft: draft.to_string(),
                ai_generated: true,
                status: "draft".to_string(),
                todo_ids,
                user_id: caller.actor.clone(),
                model: result
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                provider: result
                    .get("provider")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                requested_at: Utc::now(),
            });
        }
        if let Some(error) = result.get("error").and_then(Value::as_str) {
            return Err(UnifiedError::new(
                format!("AI draft request failed: {error}"),
                502,
            ));
        }
    }
    Err(UnifiedError::new(
        "AI draft service did not answer todos.ai.draft_requested: no handler is registered for todo drafts",
        503,
    ))
}

async fn ai_summarize(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    body: web::Json<AiSummarizeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiDraftResponse> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        // The AI capability is checked here AND enforced again by the
        // AI module when it answers the event; this module never
        // re-implements the AI allowlists.
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let input = body.into_inner();
        if let Some(list_id) = input.list_id {
            load_list_for_read(&state, list_id, &caller.actor)?;
        }
        let max_todos = input.max_todos.unwrap_or(50);
        let (context_text, todo_ids) = todos_context_text(
            &state,
            &caller.actor,
            input.list_id,
            input.include_completed,
            max_todos,
        );
        if todo_ids.is_empty() {
            return Err(UnifiedError::bad_request(
                "Nothing to summarize: the caller has no todos matching the request",
            ));
        }
        let context_text = context_text
            .chars()
            .take(MAX_CONTEXT_CHARS)
            .collect::<String>();
        request_ai_draft(
            &ctx,
            &caller,
            "summarize",
            context_text,
            todo_ids,
        )
    })();
    match result {
        Ok(draft) => ok(draft),
        Err(error) => err(error, debug),
    }
}

async fn ai_plan_day(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    body: web::Json<AiPlanDayRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiDraftResponse> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let input = body.into_inner();
        if let Some(list_id) = input.list_id {
            load_list_for_read(&state, list_id, &caller.actor)?;
        }
        let date = match &input.date {
            Some(raw) => NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| {
                UnifiedError::bad_request(format!(
                    "date must be YYYY-MM-DD, got \"{raw}\""
                ))
            })?,
            None => Utc::now().date_naive(),
        };
        let day_start = Utc
            .from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight is valid"));
        let day_end = day_start + chrono::Duration::days(1);
        // The plan covers todos due that day plus overdue open todos
        // (they still need a slot in the day) plus open todos with
        // no due date only when they are high/urgent.
        let mut owned: Vec<Todo> = state
            .todos
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|todo| todo.owner_id == caller.actor)
            .filter(|todo| todo.status == TodoStatus::Open)
            .filter(|todo| input.list_id.is_none_or(|list| todo.list_id == list))
            .filter(|todo| match todo.due_at {
                Some(due) => (due >= day_start && due < day_end) || due < day_start,
                None => todo.priority == TodoPriority::High || todo.priority == TodoPriority::Urgent,
            })
            .collect();
        owned.sort_by(|a, b| {
            a.due_at
                .cmp(&b.due_at)
                .then(b.priority.rank().cmp(&a.priority.rank()))
        });
        owned.truncate(MAX_AI_TODOS);
        if owned.is_empty() {
            return Err(UnifiedError::bad_request(
                "Nothing to plan: the caller has no open todos due on that day",
            ));
        }
        let todo_ids: Vec<Uuid> = owned.iter().map(|todo| todo.id).collect();
        let mut lines: Vec<String> = Vec::with_capacity(owned.len());
        for todo in &owned {
            let due = todo
                .due_at
                .map(|due| due.to_rfc3339())
                .unwrap_or_else(|| "no due date".to_string());
            lines.push(format!(
                "- [{}] {} (priority: {}, due: {}, tags: {})",
                todo.id,
                todo.title,
                todo.priority.as_str(),
                due,
                todo.tags.join(", "),
            ));
        }
        let mut context_text = format!("Day plan for {date}:\n{}", lines.join("\n"));
        context_text.truncate(MAX_CONTEXT_CHARS);
        request_ai_draft(
            &ctx,
            &caller,
            "plan-day",
            context_text,
            todo_ids,
        )
    })();
    match result {
        Ok(draft) => ok(draft),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Hook handlers.
// ---------------------------------------------------------------------------

/// Answer `todos.context_requested`: return the subject user's own
/// todos (optionally filtered by list, date, or status) so an AI
/// draft flow can ground on them. This mirrors the
/// `ticket.context_requested` / `chat.context_requested` pattern.
/// Hook payloads are server-side only, so the `user_id` here is the
/// subject the trusted in-process requester asked about; shared-list
/// todos are NOT included (only owned todos ground an AI draft).
fn on_todos_context_requested(state: TodosState, ctx: &mut HookContext) {
    let Some(user_id) = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        ctx.add_result(json!({ "error": "todos.context_requested requires 'user_id'" }));
        return;
    };
    let list_id = ctx
        .get("list_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let date = ctx
        .get("date")
        .and_then(|value| value.as_str())
        .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok());
    let status = ctx.get("status").and_then(|value| value.as_str());
    let mut todos: Vec<Value> = state
        .todos
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|todo| todo.owner_id == user_id)
        .filter(|todo| list_id.is_none_or(|list| todo.list_id == list))
        .filter(|todo| match (date, todo.due_at) {
            (Some(day), Some(due)) => due.date_naive() == day,
            (Some(_), None) => false,
            (None, _) => true,
        })
        .filter(|todo| match status {
            Some("open") => todo.status == TodoStatus::Open,
            Some("done") => todo.status == TodoStatus::Done,
            _ => true,
        })
        .map(|todo| {
            json!({
                "id": todo.id.to_string(),
                "todo_id": todo.id.to_string(),
                "list_id": todo.list_id.to_string(),
                "title": todo.title,
                "notes": todo.notes,
                "due_at": todo.due_at,
                "priority": todo.priority.as_str(),
                "status": todo.status.as_str(),
                "tags": todo.tags,
            })
        })
        .collect();
    todos.sort_by(|a, b| {
        a.get("due_at")
            .and_then(|value| value.as_str())
            .cmp(&b.get("due_at").and_then(|value| value.as_str()))
    });
    let count = todos.len();
    ctx.add_result(json!({ "user_id": user_id, "todos": todos, "count": count }));
}

/// Answer `todos.reminders.send_requested` (the event the scheduler
/// emits when a `todos.reminders.send` job fires): send every pending
/// reminder whose time has arrived, then run the overdue sweep.
///
/// - One reminder sends at most one notification, to the todo owner
///   only, through `notification.requested` (kind `todo.due_soon`).
/// - The overdue sweep emits `todos.todo.overdue` plus a
///   `notification.requested` (kind `todo.overdue`) at most once per
///   todo per UTC day, for open todos past their due time — including
///   todos that never had a reminder, so overdue detection does not
///   depend on reminders existing.
fn on_reminders_send_requested(state: TodosState, ctx: &mut HookContext) {
    let Some(bus) = state.bus() else {
        ctx.add_result(json!({ "error": "No hook bus is attached: reminders cannot be sent" }));
        return;
    };
    let now = Utc::now();
    let today = now.date_naive().to_string();
    let mut sent = 0u64;
    let mut overdue = 0u64;
    // Due reminders first. A reminder whose todo was deleted or
    // completed since it was queued is cancelled, never sent.
    let due_reminders: Vec<TodoReminder> = state
        .reminders
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|reminder| reminder.status == "pending" && reminder.remind_at <= now)
        .collect();
    for reminder in due_reminders {
        let Some(todo) = state.todos.get(&reminder.todo_id).map(|entry| entry.value().clone()) else {
            if let Some(mut entry) = state.reminders.get_mut(&reminder.id) {
                entry.value_mut().status = "cancelled".to_string();
                entry.value_mut().updated_at = now;
            }
            continue;
        };
        if todo.status != TodoStatus::Open {
            if let Some(mut entry) = state.reminders.get_mut(&reminder.id) {
                entry.value_mut().status = "cancelled".to_string();
                entry.value_mut().updated_at = now;
            }
            continue;
        }
        let due_text = todo
            .due_at
            .map(|due| due.to_rfc3339())
            .unwrap_or_else(|| "no due date".to_string());
        request_notification(
            &bus,
            &todo.owner_id,
            format!("Reminder: {}", todo.title),
            format!("\"{}\" is due at {due_text}.", todo.title),
            "todo.due_soon",
            todo.priority.as_str(),
            format!("/api/v1/todos/{}", todo.id),
        );
        if let Some(mut entry) = state.reminders.get_mut(&reminder.id) {
            entry.value_mut().status = "sent".to_string();
            entry.value_mut().sent_at = Some(now);
            entry.value_mut().updated_at = now;
        }
        sent += 1;
        state.emit(
            EVENT_REMINDER_SENT,
            json!({
                "reminder_id": reminder.id.to_string(),
                "todo_id": todo.id.to_string(),
                "user_id": todo.owner_id,
                "sent_at": now.to_rfc3339(),
                "resource": format!("todo:{}", todo.id),
            }),
        );
    }
    // Overdue sweep: open todos past their due time, once per day.
    let overdue_todos: Vec<Todo> = state
        .todos
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|todo| todo.is_overdue(now))
        .collect();
    for todo in overdue_todos {
        let key = format!("{}:{today}", todo.id);
        if !state.overdue_notified.insert(key) {
            continue;
        }
        let due_text = todo
            .due_at
            .map(|due| due.to_rfc3339())
            .unwrap_or_else(|| "no due date".to_string());
        state.emit(
            EVENT_TODO_OVERDUE,
            json!({
                "todo_id": todo.id.to_string(),
                "list_id": todo.list_id.to_string(),
                "owner_id": todo.owner_id,
                "title": todo.title,
                "due_at": due_text,
                "overdue_since": due_text,
                "actor_id": todo.owner_id,
                "resource": format!("todo:{}", todo.id),
            }),
        );
        request_notification(
            &bus,
            &todo.owner_id,
            format!("Overdue: {}", todo.title),
            format!("\"{}\" was due at {due_text} and is still open.", todo.title),
            "todo.overdue",
            "high",
            format!("/api/v1/todos/{}", todo.id),
        );
        overdue += 1;
    }
    ctx.add_result(json!({
        "reminders_sent": sent,
        "overdue_notified": overdue,
        "checked_at": now.to_rfc3339(),
    }));
}

// ---------------------------------------------------------------------------
// Module.
// ---------------------------------------------------------------------------

pub struct TodosModule {
    state: TodosState,
}

impl TodosModule {
    pub fn new() -> Self {
        Self {
            state: TodosState::default(),
        }
    }

    /// Product declaration: Todos are never a product. The `Module`
    /// trait defaults (`available_as_product() -> false`,
    /// `product_category() -> None`) already express this, so no
    /// override is needed here.
}

impl Default for TodosModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for TodosModule {
    fn id(&self) -> &'static str {
        "todos"
    }

    fn name(&self) -> &'static str {
        "Todos"
    }

    fn description(&self) -> &'static str {
        "Personal per-user todos: lists (including a default Inbox), quick capture, due dates with timezone-aware reminders, priorities, tags, one-level subtasks, storage-key attachments, list sharing (read/edit, ownership never transfers), recurring todos, completion history with computed streaks, and AI draft summaries and day plans. Personal utility, not a product. Strictly separate from the shared/assigned tasks module."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "scheduler"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "scheduler"]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
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
            EVENT_TODOS_CONTEXT_REQUESTED,
            "todos",
            Arc::new(move |ctx: &mut HookContext| {
                on_todos_context_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_REMINDERS_SEND_REQUESTED,
            "todos",
            Arc::new(move |ctx: &mut HookContext| {
                on_reminders_send_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/todos")
                .app_data(state)
                .route("/lists", web::get().to(list_lists))
                .route("/lists", web::post().to(create_list))
                .route("/shared-with-me", web::get().to(list_shared_with_me))
                .route("/lists/{id}", web::get().to(get_list))
                .route("/lists/{id}", web::patch().to(update_list))
                .route("/lists/{id}", web::delete().to(delete_list))
                .route("/lists/{id}/archive", web::post().to(archive_list))
                .route("/lists/{id}/unarchive", web::post().to(unarchive_list))
                .route("/lists/{id}/shares", web::get().to(list_shares))
                .route("/lists/{id}/share", web::post().to(share_list))
                .route(
                    "/lists/{id}/share/{user_id}",
                    web::delete().to(revoke_share),
                )
                .route("", web::get().to(list_todos))
                .route("", web::post().to(create_todo))
                .route("/completed", web::get().to(list_completed_todos))
                .route("/overdue", web::get().to(list_overdue_todos))
                .route("/streaks", web::get().to(get_streaks))
                .route("/{id}", web::get().to(get_todo))
                .route("/{id}", web::patch().to(update_todo))
                .route("/{id}", web::delete().to(delete_todo))
                .route("/{id}/complete", web::post().to(complete_todo))
                .route("/{id}/reopen", web::post().to(reopen_todo))
                .route("/{id}/history", web::get().to(list_todo_history))
                .route("/{id}/subtasks", web::post().to(add_subtask))
                .route(
                    "/{id}/subtasks/{subtask_id}",
                    web::patch().to(update_subtask),
                )
                .route(
                    "/{id}/subtasks/{subtask_id}/toggle",
                    web::post().to(toggle_subtask),
                )
                .route(
                    "/{id}/subtasks/{subtask_id}",
                    web::delete().to(delete_subtask),
                )
                .route("/{id}/attachments", web::post().to(add_attachment))
                .route(
                    "/{id}/attachments/{attachment_id}",
                    web::delete().to(delete_attachment),
                )
                .route("/{id}/reminders", web::post().to(create_todo_reminder))
                .route("/{id}/reminders", web::get().to(list_todo_reminders))
                .route(
                    "/{id}/reminders/{reminder_id}",
                    web::delete().to(cancel_todo_reminder),
                )
                .route("/ai/summarize", web::post().to(ai_summarize))
                .route("/ai/plan-day", web::post().to(ai_plan_day)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/todos/lists", owner: "todos", description: "List the caller's own lists plus lists shared with them (paginated, filter by archived, owner_id)." },
            RouteInfo { method: "POST", path: "/api/v1/todos/lists", owner: "todos", description: "Create a list (name, allowlisted color/icon, sort order)." },
            RouteInfo { method: "GET", path: "/api/v1/todos/shared-with-me", owner: "todos", description: "Lists shared with the caller, with the granted permission." },
            RouteInfo { method: "GET", path: "/api/v1/todos/lists/{id}", owner: "todos", description: "Get one list the caller owns or has shared access to." },
            RouteInfo { method: "PATCH", path: "/api/v1/todos/lists/{id}", owner: "todos", description: "Update a list (owner only)." },
            RouteInfo { method: "DELETE", path: "/api/v1/todos/lists/{id}", owner: "todos", description: "Delete an empty, non-default list (owner only)." },
            RouteInfo { method: "POST", path: "/api/v1/todos/lists/{id}/archive", owner: "todos", description: "Archive a list (owner only)." },
            RouteInfo { method: "POST", path: "/api/v1/todos/lists/{id}/unarchive", owner: "todos", description: "Unarchive a list (owner only)." },
            RouteInfo { method: "GET", path: "/api/v1/todos/lists/{id}/shares", owner: "todos", description: "Who a list is shared with (owner only)." },
            RouteInfo { method: "POST", path: "/api/v1/todos/lists/{id}/share", owner: "todos", description: "Share a list with a user (read or edit permission; owner only)." },
            RouteInfo { method: "DELETE", path: "/api/v1/todos/lists/{id}/share/{user_id}", owner: "todos", description: "Revoke sharing of a list with a user (owner only)." },
            RouteInfo { method: "GET", path: "/api/v1/todos", owner: "todos", description: "List own + shared todos (paginated; filters: list_id, status, priority, tag, overdue, due_before, due_after, search)." },
            RouteInfo { method: "POST", path: "/api/v1/todos", owner: "todos", description: "Create a todo; quick capture with title only (defaults to the Inbox list, normal priority, open)." },
            RouteInfo { method: "GET", path: "/api/v1/todos/completed", owner: "todos", description: "Completed todos, most recently completed first (paginated, optional list_id)." },
            RouteInfo { method: "GET", path: "/api/v1/todos/overdue", owner: "todos", description: "Open todos past their due time, most overdue first (paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/todos/streaks", owner: "todos", description: "Computed completion streaks for the caller (current/longest days, total completions)." },
            RouteInfo { method: "GET", path: "/api/v1/todos/{id}", owner: "todos", description: "Get one todo (owner or shared-list viewer)." },
            RouteInfo { method: "PATCH", path: "/api/v1/todos/{id}", owner: "todos", description: "Update a todo (owner or edit-shared user; moving across owners is refused)." },
            RouteInfo { method: "DELETE", path: "/api/v1/todos/{id}", owner: "todos", description: "Delete a todo (owner or edit-shared user)." },
            RouteInfo { method: "POST", path: "/api/v1/todos/{id}/complete", owner: "todos", description: "Complete a todo; creates the next occurrence when it recurs." },
            RouteInfo { method: "POST", path: "/api/v1/todos/{id}/reopen", owner: "todos", description: "Reopen a completed todo." },
            RouteInfo { method: "GET", path: "/api/v1/todos/{id}/history", owner: "todos", description: "Completion log for one todo (who completed/reopened, when; paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/todos/{id}/subtasks", owner: "todos", description: "Add a one-level subtask to a todo." },
            RouteInfo { method: "PATCH", path: "/api/v1/todos/{id}/subtasks/{subtask_id}", owner: "todos", description: "Rename a subtask." },
            RouteInfo { method: "POST", path: "/api/v1/todos/{id}/subtasks/{subtask_id}/toggle", owner: "todos", description: "Check or uncheck a subtask." },
            RouteInfo { method: "DELETE", path: "/api/v1/todos/{id}/subtasks/{subtask_id}", owner: "todos", description: "Delete a subtask." },
            RouteInfo { method: "POST", path: "/api/v1/todos/{id}/attachments", owner: "todos", description: "Attach a stored object by storage target + key (metadata only)." },
            RouteInfo { method: "DELETE", path: "/api/v1/todos/{id}/attachments/{attachment_id}", owner: "todos", description: "Remove an attachment reference (stored bytes are untouched)." },
            RouteInfo { method: "POST", path: "/api/v1/todos/{id}/reminders", owner: "todos", description: "Add a due-date reminder offset in minutes (requires due_at)." },
            RouteInfo { method: "GET", path: "/api/v1/todos/{id}/reminders", owner: "todos", description: "List reminders for a todo (paginated, filter by status)." },
            RouteInfo { method: "DELETE", path: "/api/v1/todos/{id}/reminders/{reminder_id}", owner: "todos", description: "Cancel a pending reminder." },
            RouteInfo { method: "POST", path: "/api/v1/todos/ai/summarize", owner: "todos", description: "AI draft summary of the caller's own todos (draft only; 503 when no AI handler answers)." },
            RouteInfo { method: "POST", path: "/api/v1/todos/ai/plan-day", owner: "todos", description: "AI draft day plan from the caller's own todos due that day (draft only; 503 when no AI handler answers)." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(TodosModule::new()));
}
