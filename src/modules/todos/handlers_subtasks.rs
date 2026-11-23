// todos — handlers subtasks.

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
use super::handlers_ai::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Subtask handlers (one level only).
// ---------------------------------------------------------------------------

pub(crate) async fn add_subtask(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<CreateSubtaskRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoSubtask> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

pub(crate) async fn update_subtask(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<UpdateSubtaskRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoSubtask> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

pub(crate) async fn toggle_subtask(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoSubtask> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

pub(crate) async fn delete_subtask(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

pub(crate) async fn add_attachment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<AddAttachmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoAttachment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

pub(crate) async fn delete_attachment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

pub(crate) async fn create_todo_reminder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<CreateReminderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoReminder> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

pub(crate) async fn list_todo_reminders(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TodoReminder>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

pub(crate) async fn cancel_todo_reminder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoReminder> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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
