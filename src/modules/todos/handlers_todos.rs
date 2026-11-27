// todos - handlers todos.

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
use super::handlers_subtasks::*;
use super::handlers_ai::*;
use super::module::*;

// Todo handlers.

/// Recompute `recurrence_remaining` from a (possibly new) count:
/// the count minus the occurrences already created in this series
/// (every todo sharing the series' `recurrence_group_id`, including
/// this one). This keeps the invariant from `create_todo`, where
/// the first occurrence leaves `count - 1` remaining.
pub(crate) fn recompute_recurrence_remaining(
    state: &TodosState,
    todo: &Todo,
    count: u32,
) -> u32 {
    let created: u32 = state
        .todos
        .iter()
        .filter(|entry| {
            let other = entry.value();
            other.id == todo.id
                || (todo.recurrence_group_id.is_some()
                    && other.recurrence_group_id == todo.recurrence_group_id)
        })
        .count() as u32;
    count.saturating_sub(created)
}

/// Cancel every pending reminder of one todo (used when a todo is
/// completed, whether through POST /complete or a PATCH status flip
/// to done: a done todo needs no due-date reminder).
pub(crate) fn cancel_pending_todo_reminders(state: &TodosState, todo_id: Uuid) {
    let now = Utc::now();
    for mut entry in state.reminders.iter_mut() {
        if entry.value().todo_id == todo_id && entry.value().status == "pending" {
            entry.value_mut().status = "cancelled".to_string();
            entry.value_mut().updated_at = now;
        }
    }
}

/// Load the list of one todo for a list response. Returns `None`
/// when the list is gone (a concurrent list deletion between the
/// filter and this load), so callers skip the todo instead of
/// panicking on a re-load that can no longer succeed.
fn todo_response_for_list(
    state: &TodosState,
    todo: &Todo,
    actor: &str,
) -> Option<TodoResponse> {
    let list = load_list(state, todo.list_id).ok()?;
    let can_edit = can_edit_list(state, &list, actor);
    Some(TodoResponse::new(todo, can_edit))
}

pub(crate) async fn list_todos(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TodoResponse>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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
            .filter_map(|todo| todo_response_for_list(&state, &todo, &caller.actor))
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

pub(crate) async fn create_todo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    body: web::Json<CreateTodoRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<(TodoResponse, Vec<TodoReminder>)> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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
        // hold a new todo only with edit access - and the todo still
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
            // the caller - for an owned list those are the same user.
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

pub(crate) async fn get_todo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoResponse> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

pub(crate) async fn update_todo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateTodoRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoResponse> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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
        } else if let Some(count) = next_count {
            // Raising (or lowering) the count, or adding a frequency
            // with a count to a non-recurring todo, must move
            // `recurrence_remaining` too: it is the new count minus
            // the occurrences already created in this series.
            todo.recurrence_remaining =
                Some(recompute_recurrence_remaining(&state, &todo, count));
        } else {
            todo.recurrence_remaining = None;
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
                    // A done todo needs no due-date reminder, exactly
                    // like POST /complete.
                    cancel_pending_todo_reminders(&state, todo.id);
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

pub(crate) async fn delete_todo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

pub(crate) async fn complete_todo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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
        cancel_pending_todo_reminders(&state, todo_id);
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

pub(crate) async fn reopen_todo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoResponse> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

pub(crate) async fn list_completed_todos(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TodoResponse>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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
            .filter_map(|todo| todo_response_for_list(&state, &todo, &caller.actor))
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

pub(crate) async fn list_overdue_todos(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TodoResponse>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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
            .filter_map(|todo| todo_response_for_list(&state, &todo, &caller.actor))
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

pub(crate) async fn get_streaks(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StreaksView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        Ok(compute_streaks(&state, &caller.actor))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_todo_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TodoCompletionLog>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn list(owner: &str) -> TodoList {
        let now = Utc::now();
        TodoList {
            id: Uuid::new_v4(),
            owner_id: owner.to_string(),
            name: "Inbox".to_string(),
            color: "blue".to_string(),
            icon: "inbox".to_string(),
            archived: false,
            sort_order: 0,
            is_default: true,
            created_at: now,
            updated_at: now,
        }
    }

    fn todo(list_id: Uuid, group_id: Option<Uuid>) -> Todo {
        let now = Utc::now();
        Todo {
            id: Uuid::new_v4(),
            list_id,
            owner_id: "user-1".to_string(),
            title: "Recurring task".to_string(),
            notes: None,
            due_at: Some(now + chrono::Duration::days(1)),
            timezone: "UTC".to_string(),
            priority: TodoPriority::Normal,
            status: TodoStatus::Open,
            tags: Vec::new(),
            subtasks: Vec::new(),
            attachments: Vec::new(),
            recurrence_frequency: None,
            recurrence_interval: 1,
            recurrence_count: None,
            recurrence_until: None,
            recurrence_remaining: None,
            recurrence_group_id: group_id,
            position: 0,
            completed_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn reminder(todo_id: Uuid, status: &str) -> TodoReminder {
        let now = Utc::now();
        TodoReminder {
            id: Uuid::new_v4(),
            todo_id,
            owner_id: "user-1".to_string(),
            offset_minutes: 60,
            remind_at: now,
            due_at: now + chrono::Duration::hours(1),
            status: status.to_string(),
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            sent_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn recurrence_remaining_recomputes_from_the_new_count() {
        let state = TodosState::default();
        let owner_list = list("user-1");
        state.lists.insert(owner_list.id, owner_list.clone());
        let group_id = Uuid::new_v4();
        // Three occurrences already created in one series.
        let mut target = todo(owner_list.id, Some(group_id));
        state.todos.insert(target.id, target.clone());
        for _ in 0..2 {
            let other = todo(owner_list.id, Some(group_id));
            state.todos.insert(other.id, other);
        }
        // Raising the count from 3 to 10 leaves 7 further occurrences.
        assert_eq!(recompute_recurrence_remaining(&state, &target, 10), 7);
        // Keeping the count at 3 leaves none.
        assert_eq!(recompute_recurrence_remaining(&state, &target, 3), 0);
        // A count below the series size saturates at zero.
        assert_eq!(recompute_recurrence_remaining(&state, &target, 2), 0);
        // Adding a frequency with a count to a non-recurring todo:
        // only this occurrence exists, so remaining is count - 1.
        target.recurrence_group_id = Some(Uuid::new_v4());
        assert_eq!(recompute_recurrence_remaining(&state, &target, 10), 9);
    }

    #[test]
    fn completing_cancels_only_pending_reminders_of_that_todo() {
        let state = TodosState::default();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let pending = reminder(first, "pending");
        let sent = reminder(first, "sent");
        let other_pending = reminder(second, "pending");
        state.reminders.insert(pending.id, pending.clone());
        state.reminders.insert(sent.id, sent.clone());
        state.reminders.insert(other_pending.id, other_pending.clone());
        cancel_pending_todo_reminders(&state, first);
        assert_eq!(
            state.reminders.get(&pending.id).unwrap().status,
            "cancelled"
        );
        assert_eq!(state.reminders.get(&sent.id).unwrap().status, "sent");
        assert_eq!(
            state.reminders.get(&other_pending.id).unwrap().status,
            "pending"
        );
    }

    #[test]
    fn todo_response_skips_a_todo_whose_list_is_gone() {
        let state = TodosState::default();
        let owner_list = list("user-1");
        state.lists.insert(owner_list.id, owner_list.clone());
        let item = todo(owner_list.id, None);
        state.todos.insert(item.id, item.clone());
        let shown = todo_response_for_list(&state, &item, "user-1");
        assert!(shown.is_some_and(|response| response.can_edit));
        // A concurrent list deletion between filter and load must
        // skip the todo, never panic.
        state.lists.remove(&owner_list.id);
        assert!(todo_response_for_list(&state, &item, "user-1").is_none());
    }
}
