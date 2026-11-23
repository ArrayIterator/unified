// todos — service.

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
use super::types::*;
use super::store::*;
use super::common::*;
use super::validation::*;
use super::access::*;
use super::handlers_lists::*;
use super::handlers_todos::*;
use super::handlers_subtasks::*;
use super::handlers_ai::*;
use super::module::*;

/// Compute streaks from the completion log: distinct UTC dates with
/// at least one `completed` entry by this user.
pub(crate) fn compute_streaks(state: &TodosState, user_id: &str) -> StreaksView {
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
pub(crate) fn request_notification(bus: &HookBus, user_id: &str, title: String, body: String, kind: &str, priority: &str, link: String) {
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
/// The kind (`todos.reminders.send`) is registered in the
/// scheduler's `KNOWN_KINDS` (`scheduler/types.rs`) and dispatched
/// in `scheduler/service.rs`, which emits
/// `todos.reminders.send_requested`. The result is read back from
/// the first hook result: `{"queued": true, "job_id": ...}` means
/// the scheduler accepted the job; anything else records
/// `declined` instead of pretending the reminder was scheduled.
pub(crate) fn schedule_reminder(ctx: &AppContext, reminder: &mut TodoReminder) {
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
pub(crate) fn create_reminder(
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
pub(crate) fn create_next_occurrence(state: &TodosState, done: &Todo) -> Option<Todo> {
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
