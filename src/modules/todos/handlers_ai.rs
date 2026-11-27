// todos - handlers ai.

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
use super::service::*;
use super::handlers_lists::*;
use super::handlers_todos::*;
use super::handlers_subtasks::*;
use super::module::*;

// AI handlers (drafts only).

/// Render the caller's own todos as grounding text for an AI draft
/// request. Only todos the caller OWNS are included - never todos
/// from a list shared with them, and never another user's todos.
pub(crate) fn todos_context_text(state: &TodosState, user_id: &str, list_id: Option<Uuid>, include_completed: bool, max_todos: usize) -> (String, Vec<Uuid>) {
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
            .map(|notes| format!(" - {notes}"))
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
/// answers - a summary is never fabricated locally.
pub(crate) fn request_ai_draft(
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

pub(crate) async fn ai_summarize(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    body: web::Json<AiSummarizeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiDraftResponse> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

pub(crate) async fn ai_plan_day(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    body: web::Json<AiPlanDayRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiDraftResponse> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

// Hook handlers.

/// Answer `todos.context_requested`: return the subject user's own
/// todos (optionally filtered by list, date, or status) so an AI
/// draft flow can ground on them. This mirrors the
/// `ticket.context_requested` / `chat.context_requested` pattern.
/// Hook payloads are server-side only, so the `user_id` here is the
/// subject the trusted in-process requester asked about; shared-list
/// todos are NOT included (only owned todos ground an AI draft).
pub(crate) fn on_todos_context_requested(state: TodosState, ctx: &mut HookContext) {
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
///   todo per UTC day, for open todos past their due time - including
///   todos that never had a reminder, so overdue detection does not
///   depend on reminders existing.
pub(crate) fn on_reminders_send_requested(state: TodosState, ctx: &mut HookContext) {
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
