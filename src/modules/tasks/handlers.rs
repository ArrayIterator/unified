// tasks — HTTP handlers.

use super::types::{PERM_TASKS_MANAGE, PERM_TASKS_READ, TASK_DELETED};
use super::validation::{
    assign_event_for, caller, caller_company, in_company_scope, require_company_match,
    require_company_match_for, require_company_match_uuid, require_company_match_uuid_for,
    require_permission, require_record_in_company_scope, require_tenant,
};
use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json::{error_response, success_response};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

pub(crate) async fn list_tasks(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return error_response(&error, debug),
    };
    let _caller = match caller(&req) {
        Ok(found) => found,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &_caller.role, PERM_TASKS_READ) {
        return error_response(&error, debug);
    }
    let params = query.into_inner();
    let pagination = params.normalized();
    let sort = params.allowed_sort(
        &["created_at", "due_date", "priority", "title", "updated_at"],
        "created_at",
    );
    let descending = pagination.order == "desc";
    let search = pagination
        .search
        .clone()
        .map(|value| value.to_lowercase());

    let status_filter = match params.filters.get("status").map(|value| {
        TaskStatus::parse(value).ok_or_else(|| {
            UnifiedError::bad_request(format!("Unknown task status: \"{value}\""))
        })
    }) {
        Some(Ok(status)) => Some(status),
        Some(Err(error)) => return error_response(&error, debug),
        None => None,
    };
    let priority_filter = match params.filters.get("priority").map(|value| {
        serde_json::from_value::<TaskPriority>(Value::String(value.clone())).map_err(|_| {
            UnifiedError::bad_request(format!("Unknown task priority: \"{value}\""))
        })
    }) {
        Some(Ok(priority)) => Some(priority),
        Some(Err(error)) => return error_response(&error, debug),
        None => None,
    };
    let assignee_filter = match parse_uuid_filter(&params, "assignee_id") {
        Ok(value) => value,
        Err(error) => return error_response(&error, debug),
    };
    let company_filter = match parse_uuid_filter(&params, "company_id") {
        Ok(value) => value,
        Err(error) => return error_response(&error, debug),
    };
    let due_before = match parse_date_filter(&params, "due_before") {
        Ok(value) => value,
        Err(error) => return error_response(&error, debug),
    };
    let due_after = match parse_date_filter(&params, "due_after") {
        Ok(value) => value,
        Err(error) => return error_response(&error, debug),
    };
    let overdue_filter = params
        .filters
        .get("overdue")
        .and_then(|value| value.parse::<bool>().ok());
    let project_filter = params.filters.get("project_name").cloned();
    let now = Utc::now();

    let mut records: Vec<Task> = store
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|task| {
            if !in_company_scope(&task.company_id, &_scope) {
                return false;
            }
            if let Some(status) = status_filter {
                if task.status != status {
                    return false;
                }
            }
            if let Some(priority) = priority_filter {
                if task.priority != priority {
                    return false;
                }
            }
            if let Some(assignee) = assignee_filter {
                if task.assignee_id != Some(assignee) {
                    return false;
                }
            }
            if let Some(company) = company_filter {
                if task.company_id != Some(company) {
                    return false;
                }
            }
            if let Some(before) = due_before {
                match task.due_date {
                    Some(due) if due <= before => {}
                    _ => return false,
                }
            }
            if let Some(after) = due_after {
                match task.due_date {
                    Some(due) if due >= after => {}
                    _ => return false,
                }
            }
            if let Some(want_overdue) = overdue_filter {
                if task.is_overdue(now) != want_overdue {
                    return false;
                }
            }
            if let Some(project) = &project_filter {
                match &task.project_name {
                    Some(name) if name.eq_ignore_ascii_case(project) => {}
                    _ => return false,
                }
            }
            if let Some(needle) = &search {
                let haystack = format!(
                    "{} {} {} {}",
                    task.title,
                    task.description,
                    task.project_name.clone().unwrap_or_default(),
                    task.tags.join(" ")
                )
                .to_lowercase();
                if !haystack.contains(needle.as_str()) {
                    return false;
                }
            }
            true
        })
        .collect();

    records.sort_by(|a, b| match sort.as_str() {
        "due_date" => a.due_date.cmp(&b.due_date),
        "priority" => a.priority.rank().cmp(&b.priority.rank()),
        "title" => a.title.cmp(&b.title),
        "updated_at" => a.updated_at.cmp(&b.updated_at),
        _ => a.created_at.cmp(&b.created_at),
    });
    if descending {
        records.reverse();
    }

    let total = records.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let items: Vec<TaskResponse> = records
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .map(|task| TaskResponse::from(&task))
        .collect();
    success_response(PageResult::new(items, total, &pagination).to_json(), 200)
}

pub(crate) async fn create_task(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    req: HttpRequest,
    body: web::Json<CreateTaskInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return error_response(&error, debug),
    };
    let _caller = match caller(&req) {
        Ok(found) => found,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &_caller.role, PERM_TASKS_MANAGE) {
        return error_response(&error, debug);
    }
    let input = body.into_inner();
    let ip = client_ip(&req);
    let result: UnifiedResult<Task> = (|| {
        let title = validate_title(&input.title)?;
        let description = input.description.unwrap_or_default();
        validate_description(&description)?;
        let tags = validate_tags(&input.tags.unwrap_or_default())?;
        let estimate = match input.estimate_minutes {
            Some(minutes) => Some(validate_minutes(minutes, "estimate_minutes")?),
            None => None,
        };
        let spent = validate_minutes(input.spent_minutes.unwrap_or(0), "spent_minutes")?;
        let status = input.status.unwrap_or_default();
        let now = Utc::now();
        let company_id = require_company_match_uuid(&_tenant, &req, &input.company_id)?;
        let task = Task {
            id: Uuid::now_v7(),
            title,
            description,
            assignee_id: input.assignee_id,
            reporter_id: input.reporter_id,
            priority: input.priority.unwrap_or_default(),
            status,
            due_date: input.due_date,
            company_id,
            project_name: input
                .project_name
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty()),
            tags,
            comments: Vec::new(),
            checklist: Vec::new(),
            estimate_minutes: estimate,
            spent_minutes: spent,
            completed_at: if status == TaskStatus::Done {
                Some(now)
            } else {
                None
            },
            created_at: now,
            updated_at: now,
        };
        store.insert(task.id, task.clone());
        Ok(task)
    })();
    match result {
        Ok(task) => {
            ctx.hooks.emit(TASK_CREATED, task_event_payload(&task, ip.as_deref()));
            if task.assignee_id.is_some() {
                ctx.hooks
                    .emit(TASK_ASSIGNED, task_event_payload(&task, ip.as_deref()));
            }
            if task.status == TaskStatus::Done {
                ctx.hooks
                    .emit(TASK_COMPLETED, task_event_payload(&task, ip.as_deref()));
            }
            success_response(TaskResponse::from(&task), 201)
        }
        Err(error) => error_response(&error, debug),
    }
}

pub(crate) async fn get_task(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return error_response(&error, debug),
    };
    let _caller = match caller(&req) {
        Ok(found) => found,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &_caller.role, PERM_TASKS_READ) {
        return error_response(&error, debug);
    }
    let task_id = id.into_inner();
    match store.get(&task_id) {
        Some(record) => {
            if let Err(error) = require_record_in_company_scope(&record.company_id, &_scope) {
                return error_response(&error, debug);
            }
            success_response(TaskResponse::from(record.value()), 200)
        }
        None => error_response(
            &UnifiedError::not_found(format!("Task {task_id} not found")),
            debug,
        ),
    }
}

pub(crate) async fn update_task(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateTaskInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return error_response(&error, debug),
    };
    let _caller = match caller(&req) {
        Ok(found) => found,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &_caller.role, PERM_TASKS_MANAGE) {
        return error_response(&error, debug);
    }
    let task_id = id.into_inner();
    let input = body.into_inner();
    let result: UnifiedResult<Task> = (|| {
        let mut record = store.get_mut(&task_id).ok_or_else(|| {
            UnifiedError::not_found(format!("Task {task_id} not found"))
        })?;
        require_record_in_company_scope(&record.company_id, &_scope)?;
        if let Some(title) = &input.title {
            record.title = validate_title(title)?;
        }
        if let Some(description) = &input.description {
            validate_description(description)?;
            record.description = description.clone();
        }
        if let Some(assignee) = input.assignee_id {
            record.assignee_id = assignee;
        }
        if let Some(reporter) = input.reporter_id {
            record.reporter_id = reporter;
        }
        if let Some(priority) = input.priority {
            record.priority = priority;
        }
        if let Some(due_date) = input.due_date {
            record.due_date = due_date;
        }
        if let Some(company) = input.company_id {
            record.company_id = require_company_match_uuid_for(&_scope, &company)?;
        }
        if let Some(project) = input.project_name {
            record.project_name = project
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty());
        }
        if let Some(tags) = &input.tags {
            record.tags = validate_tags(tags)?;
        }
        if let Some(estimate) = input.estimate_minutes {
            record.estimate_minutes = match estimate {
                Some(minutes) => Some(validate_minutes(minutes, "estimate_minutes")?),
                None => None,
            };
        }
        if let Some(spent) = input.spent_minutes {
            record.spent_minutes = validate_minutes(spent, "spent_minutes")?;
        }
        record.updated_at = Utc::now();
        Ok(record.clone())
    })();
    match result {
        Ok(task) => success_response(TaskResponse::from(&task), 200),
        Err(error) => error_response(&error, debug),
    }
}

pub(crate) async fn delete_task(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return error_response(&error, debug),
    };
    let caller = match caller(&req) {
        Ok(found) => found,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_TASKS_MANAGE) {
        return error_response(&error, debug);
    }
    let task_id = id.into_inner();
    let ip = client_ip(&req);
    match store.remove(&task_id) {
        Some((_, task)) => {
            // The payload carries the deleted task; the actor is the
            // caller who deleted it, not the task's reporter.
            let mut payload = task_event_payload(&task, ip.as_deref());
            if let Value::Object(map) = &mut payload {
                map.insert(
                    "actor_id".to_string(),
                    Value::String(caller.actor.clone()),
                );
            }
            ctx.hooks.emit(TASK_DELETED, payload);
            success_response(
                serde_json::json!({ "deleted": true, "id": task_id }),
                200,
            )
        }
        None => error_response(
            &UnifiedError::not_found(format!("Task {task_id} not found")),
            debug,
        ),
    }
}

pub(crate) async fn change_task_status(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<ChangeStatusInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return error_response(&error, debug),
    };
    let _caller = match caller(&req) {
        Ok(found) => found,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &_caller.role, PERM_TASKS_MANAGE) {
        return error_response(&error, debug);
    }
    let task_id = id.into_inner();
    let target = body.status;
    let ip = client_ip(&req);
    let result: UnifiedResult<(Task, TaskStatus)> = (|| {
        let mut record = store.get_mut(&task_id).ok_or_else(|| {
            UnifiedError::not_found(format!("Task {task_id} not found"))
        })?;
        require_record_in_company_scope(&record.company_id, &_scope)?;
        let from = record.status;
        if from == target {
            return Err(UnifiedError::unprocessable(format!(
                "Task is already in status \"{}\"",
                from.as_str()
            ))
            .with_metadata("from", Value::String(from.as_str().to_string()))
            .with_metadata("to", Value::String(target.as_str().to_string())));
        }
        if !from.allowed_next().contains(&target) {
            let allowed: Vec<&str> = from
                .allowed_next()
                .iter()
                .map(|status| status.as_str())
                .collect();
            return Err(UnifiedError::unprocessable(format!(
                "Cannot move task from \"{}\" to \"{}\"",
                from.as_str(),
                target.as_str()
            ))
            .with_metadata("from", Value::String(from.as_str().to_string()))
            .with_metadata("to", Value::String(target.as_str().to_string()))
            .with_metadata("allowed", Value::from(allowed)));
        }
        record.status = target;
        record.updated_at = Utc::now();
        // `done` is terminal (`Done.allowed_next()` is empty), so a
        // transition out of it is rejected above; there is no branch
        // here that clears `completed_at`.
        if target == TaskStatus::Done {
            record.completed_at = Some(record.updated_at);
        }
        Ok((record.clone(), from))
    })();
    match result {
        Ok((task, from)) => {
            // Emit the status change with the target status and the
            // previous status; the completion event fires only when the
            // task reaches done.
            let mut payload = task_event_payload(&task, ip.as_deref());
            if let Value::Object(map) = &mut payload {
                map.insert(
                    "from_status".to_string(),
                    Value::String(from.as_str().to_string()),
                );
            }
            ctx.hooks.emit(TASK_STATUS_CHANGED, payload);
            if task.status == TaskStatus::Done {
                ctx.hooks
                    .emit(TASK_COMPLETED, task_event_payload(&task, ip.as_deref()));
            }
            success_response(TaskResponse::from(&task), 200)
        }
        Err(error) => error_response(&error, debug),
    }
}

pub(crate) async fn assign_task(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<AssignInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return error_response(&error, debug),
    };
    let _caller = match caller(&req) {
        Ok(found) => found,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &_caller.role, PERM_TASKS_MANAGE) {
        return error_response(&error, debug);
    }
    let task_id = id.into_inner();
    let assignee_id = body.assignee_id;
    let ip = client_ip(&req);
    let result: UnifiedResult<(Task, Option<Uuid>)> = (|| {
        let mut record = store.get_mut(&task_id).ok_or_else(|| {
            UnifiedError::not_found(format!("Task {task_id} not found"))
        })?;
        require_record_in_company_scope(&record.company_id, &_scope)?;
        let old_assignee = record.assignee_id;
        record.assignee_id = assignee_id;
        record.updated_at = Utc::now();
        Ok((record.clone(), old_assignee))
    })();
    match result {
        Ok((task, old_assignee)) => {
            // Only a real assignment change emits: a new assignee
            // different from the old one emits `task.assigned`,
            // unassigning emits `task.unassigned`, and re-assigning
            // the same person (or null onto null) emits nothing.
            if let Some(event) = assign_event_for(old_assignee, task.assignee_id) {
                ctx.hooks
                    .emit(event, task_event_payload(&task, ip.as_deref()));
            }
            success_response(TaskResponse::from(&task), 200)
        }
        Err(error) => error_response(&error, debug),
    }
}

pub(crate) async fn log_time(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<LogTimeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return error_response(&error, debug),
    };
    let _caller = match caller(&req) {
        Ok(found) => found,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &_caller.role, PERM_TASKS_MANAGE) {
        return error_response(&error, debug);
    }
    let task_id = id.into_inner();
    let minutes = body.minutes;
    let result: UnifiedResult<Task> = (|| {
        if minutes <= 0 {
            return Err(UnifiedError::bad_request(
                "Logged time must be a positive number of minutes",
            ));
        }
        let mut record = store.get_mut(&task_id).ok_or_else(|| {
            UnifiedError::not_found(format!("Task {task_id} not found"))
        })?;
        require_record_in_company_scope(&record.company_id, &_scope)?;
        record.spent_minutes = record
            .spent_minutes
            .checked_add(minutes)
            .ok_or_else(|| UnifiedError::bad_request("Time spent would overflow"))?;
        record.updated_at = Utc::now();
        Ok(record.clone())
    })();
    match result {
        Ok(task) => success_response(TaskResponse::from(&task), 200),
        Err(error) => error_response(&error, debug),
    }
}

pub(crate) async fn list_comments(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return error_response(&error, debug),
    };
    let _caller = match caller(&req) {
        Ok(found) => found,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &_caller.role, PERM_TASKS_READ) {
        return error_response(&error, debug);
    }
    let task_id = id.into_inner();
    let params = query.into_inner();
    let pagination = params.normalized();
    let descending = pagination.order == "desc";
    let Some(record) = store.get(&task_id) else {
        return error_response(
            &UnifiedError::not_found(format!("Task {task_id} not found")),
            debug,
        );
    };
    if let Err(error) = require_record_in_company_scope(&record.company_id, &_scope) {
        return error_response(&error, debug);
    }
    let mut comments = record.comments.clone();
    if let Some(needle) = &pagination.search {
        let needle = needle.to_lowercase();
        comments.retain(|comment| comment.body.to_lowercase().contains(&needle));
    }
    comments.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if descending {
        comments.reverse();
    }
    let total = comments.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let items: Vec<TaskComment> = comments
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    success_response(PageResult::new(items, total, &pagination).to_json(), 200)
}

pub(crate) async fn add_comment(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<AddCommentInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return error_response(&error, debug),
    };
    let _caller = match caller(&req) {
        Ok(found) => found,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &_caller.role, PERM_TASKS_MANAGE) {
        return error_response(&error, debug);
    }
    let task_id = id.into_inner();
    let input = body.into_inner();
    let ip = client_ip(&req);
    let result: UnifiedResult<TaskComment> = (|| {
        let body_text = input.body.trim();
        if body_text.is_empty() {
            return Err(UnifiedError::bad_request("Comment body must not be empty"));
        }
        if body_text.chars().count() > 5_000 {
            return Err(UnifiedError::bad_request(
                "Comment body must be at most 5000 characters long",
            ));
        }
        let mut record = store.get_mut(&task_id).ok_or_else(|| {
            UnifiedError::not_found(format!("Task {task_id} not found"))
        })?;
        require_record_in_company_scope(&record.company_id, &_scope)?;
        let comment = TaskComment {
            id: Uuid::now_v7(),
            task_id,
            author_id: input.author_id,
            body: body_text.to_string(),
            created_at: Utc::now(),
        };
        record.comments.push(comment.clone());
        record.updated_at = Utc::now();
        Ok(comment)
    })();
    match result {
        Ok(comment) => {
            let mut payload = serde_json::json!({
                "id": comment.id,
                "comment_id": comment.id,
                "task_id": task_id,
                "author_id": comment.author_id,
                "entity_type": "task",
                "entity_id": task_id,
                "resource": format!("task:{task_id}"),
                "summary": "Comment added to task",
            });
            if let Some(actor) = comment.author_id {
                if let Value::Object(map) = &mut payload {
                    map.insert("actor_id".to_string(), Value::String(actor.to_string()));
                }
            }
            if let Some(ip) = ip {
                if let Value::Object(map) = &mut payload {
                    map.insert("ip".to_string(), Value::String(ip));
                }
            }
            ctx.hooks.emit(TASK_COMMENT_ADDED, payload);
            success_response(comment, 201)
        }
        Err(error) => error_response(&error, debug),
    }
}

pub(crate) async fn add_checklist_item(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<AddChecklistInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return error_response(&error, debug),
    };
    let _caller = match caller(&req) {
        Ok(found) => found,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &_caller.role, PERM_TASKS_MANAGE) {
        return error_response(&error, debug);
    }
    let task_id = id.into_inner();
    let input = body.into_inner();
    let result: UnifiedResult<ChecklistItem> = (|| {
        let title = input.title.trim();
        if title.is_empty() {
            return Err(UnifiedError::bad_request(
                "Checklist item title must not be empty",
            ));
        }
        if title.chars().count() > 255 {
            return Err(UnifiedError::bad_request(
                "Checklist item title must be at most 255 characters long",
            ));
        }
        let mut record = store.get_mut(&task_id).ok_or_else(|| {
            UnifiedError::not_found(format!("Task {task_id} not found"))
        })?;
        require_record_in_company_scope(&record.company_id, &_scope)?;
        let item = ChecklistItem {
            id: Uuid::now_v7(),
            task_id,
            title: title.to_string(),
            done: false,
            completed_at: None,
            created_at: Utc::now(),
        };
        record.checklist.push(item.clone());
        record.updated_at = Utc::now();
        Ok(item)
    })();
    match result {
        Ok(item) => success_response(item, 201),
        Err(error) => error_response(&error, debug),
    }
}

pub(crate) async fn toggle_checklist_item(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return error_response(&error, debug),
    };
    let _caller = match caller(&req) {
        Ok(found) => found,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &_caller.role, PERM_TASKS_MANAGE) {
        return error_response(&error, debug);
    }
    let (task_id, item_id) = path.into_inner();
    let result: UnifiedResult<ChecklistItem> = (|| {
        let mut record = store.get_mut(&task_id).ok_or_else(|| {
            UnifiedError::not_found(format!("Task {task_id} not found"))
        })?;
        require_record_in_company_scope(&record.company_id, &_scope)?;
        let item = record
            .checklist
            .iter_mut()
            .find(|item| item.id == item_id)
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Checklist item {item_id} not found"))
            })?;
        item.done = !item.done;
        item.completed_at = if item.done { Some(Utc::now()) } else { None };
        let updated = item.clone();
        record.updated_at = Utc::now();
        Ok(updated)
    })();
    match result {
        Ok(item) => success_response(item, 200),
        Err(error) => error_response(&error, debug),
    }
}

pub(crate) async fn delete_checklist_item(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return error_response(&error, debug),
    };
    let _caller = match caller(&req) {
        Ok(found) => found,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &_caller.role, PERM_TASKS_MANAGE) {
        return error_response(&error, debug);
    }
    let (task_id, item_id) = path.into_inner();
    let result: UnifiedResult<()> = (|| {
        let mut record = store.get_mut(&task_id).ok_or_else(|| {
            UnifiedError::not_found(format!("Task {task_id} not found"))
        })?;
        require_record_in_company_scope(&record.company_id, &_scope)?;
        let before = record.checklist.len();
        record.checklist.retain(|item| item.id != item_id);
        if record.checklist.len() == before {
            return Err(UnifiedError::not_found(format!(
                "Checklist item {item_id} not found"
            )));
        }
        record.updated_at = Utc::now();
        Ok(())
    })();
    match result {
        Ok(()) => success_response(
            serde_json::json!({ "deleted": true, "id": item_id }),
            200,
        ),
        Err(error) => error_response(&error, debug),
    }
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            },
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            },
        );
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::super::types::{PERM_TASKS_MANAGE, PERM_TASKS_READ};
    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;
    use dashmap::DashMap;
    use std::sync::{Arc, Mutex};

    /// Test environment: an `AppContext` whose `role.check` hook
    /// allows only role `admin` to hold the tasks permissions, plus
    /// capture lists for the assignment and deletion events so tests
    /// can assert exactly which events a handler emitted.
    struct TestEnv {
        ctx: Arc<AppContext>,
        assigned: Arc<Mutex<Vec<Value>>>,
        unassigned: Arc<Mutex<Vec<Value>>>,
        deleted: Arc<Mutex<Vec<Value>>>,
    }

    impl TestEnv {
        fn new() -> Self {
            let bus = Arc::new(HookBus::new());
            bus.register(
                "role.check",
                "tasks-test",
                Arc::new(|context| {
                    let role = context
                        .get("role")
                        .and_then(|value| value.as_str())
                        .unwrap_or("");
                    let permission = context
                        .get("permission")
                        .and_then(|value| value.as_str())
                        .unwrap_or("");
                    let allowed = role == "admin"
                        && (permission == PERM_TASKS_READ
                            || permission == PERM_TASKS_MANAGE);
                    context.set("allowed", Value::Bool(allowed));
                }),
            );
            let assigned = Arc::new(Mutex::new(Vec::new()));
            let unassigned = Arc::new(Mutex::new(Vec::new()));
            let deleted = Arc::new(Mutex::new(Vec::new()));
            for (event, sink) in [
                (TASK_ASSIGNED, Arc::clone(&assigned)),
                (TASK_UNASSIGNED, Arc::clone(&unassigned)),
                (TASK_DELETED, Arc::clone(&deleted)),
            ] {
                bus.register(
                    event,
                    "tasks-test",
                    Arc::new(move |context| {
                        sink.lock()
                            .expect("event sink lock poisoned")
                            .push(context.data.clone());
                    }),
                );
            }
            let ctx = Arc::new(AppContext::new(
                AppConfig::default(),
                bus,
                Arc::new(ModuleRegistry::new()),
            ));
            Self {
                ctx,
                assigned,
                unassigned,
                deleted,
            }
        }

        fn data(&self) -> web::Data<Arc<AppContext>> {
            web::Data::new(Arc::clone(&self.ctx))
        }
    }

    fn store() -> web::Data<TasksStore> {
        web::Data::new(Arc::new(DashMap::new()))
    }

    fn anonymous() -> HttpRequest {
        TestRequest::default().to_http_request()
    }

    fn authed(role: &str) -> HttpRequest {
        verified_request("user-1", role)
    }

    fn seed_task(
        store: &web::Data<TasksStore>,
        status: TaskStatus,
        assignee: Option<Uuid>,
    ) -> Task {
        let now = Utc::now();
        let task = Task {
            id: Uuid::now_v7(),
            title: "Seeded task".to_string(),
            description: String::new(),
            assignee_id: assignee,
            reporter_id: None,
            priority: TaskPriority::Medium,
            status,
            due_date: None,
            company_id: None,
            project_name: None,
            tags: Vec::new(),
            comments: Vec::new(),
            checklist: Vec::new(),
            estimate_minutes: None,
            spent_minutes: 0,
            completed_at: if status == TaskStatus::Done {
                Some(now)
            } else {
                None
            },
            created_at: now,
            updated_at: now,
        };
        store.insert(task.id, task.clone());
        task
    }

    fn create_input() -> web::Json<CreateTaskInput> {
        web::Json(CreateTaskInput {
            title: "New task".to_string(),
            description: None,
            assignee_id: None,
            reporter_id: None,
            priority: None,
            status: None,
            due_date: None,
            company_id: None,
            project_name: None,
            tags: None,
            estimate_minutes: None,
            spent_minutes: None,
        })
    }

    #[tokio::test]
    async fn list_tasks_rejects_anonymous_and_member_allows_admin() {
        let env = TestEnv::new();
        let denied = list_tasks(
            env.data(),
            store(),
            anonymous(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let forbidden = list_tasks(
            env.data(),
            store(),
            authed("member"),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
        let allowed = list_tasks(
            env.data(),
            store(),
            authed("admin"),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(allowed.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn create_task_rejects_anonymous_and_member_allows_admin() {
        let env = TestEnv::new();
        let denied = create_task(env.data(), store(), anonymous(), create_input()).await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let forbidden =
            create_task(env.data(), store(), authed("member"), create_input()).await;
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
        let allowed =
            create_task(env.data(), store(), authed("admin"), create_input()).await;
        assert_eq!(allowed.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn get_task_rejects_anonymous_and_member_allows_admin() {
        let env = TestEnv::new();
        let tasks = store();
        let task = seed_task(&tasks, TaskStatus::Todo, None);
        let denied = get_task(env.data(), tasks.clone(), anonymous(), web::Path::from(task.id)).await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let forbidden = get_task(
            env.data(),
            tasks.clone(),
            authed("member"),
            web::Path::from(task.id),
        )
        .await;
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
        let allowed = get_task(
            env.data(),
            tasks.clone(),
            authed("admin"),
            web::Path::from(task.id),
        )
        .await;
        assert_eq!(allowed.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn update_task_rejects_anonymous_and_member() {
        let env = TestEnv::new();
        let tasks = store();
        let task = seed_task(&tasks, TaskStatus::Todo, None);
        let input = web::Json(UpdateTaskInput {
            title: Some("Renamed".to_string()),
            description: None,
            assignee_id: None,
            reporter_id: None,
            priority: None,
            due_date: None,
            company_id: None,
            project_name: None,
            tags: None,
            estimate_minutes: None,
            spent_minutes: None,
        });
        let denied = update_task(
            env.data(),
            tasks.clone(),
            anonymous(),
            web::Path::from(task.id),
            input,
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let input = web::Json(UpdateTaskInput {
            title: Some("Renamed".to_string()),
            description: None,
            assignee_id: None,
            reporter_id: None,
            priority: None,
            due_date: None,
            company_id: None,
            project_name: None,
            tags: None,
            estimate_minutes: None,
            spent_minutes: None,
        });
        let forbidden = update_task(
            env.data(),
            tasks.clone(),
            authed("member"),
            web::Path::from(task.id),
            input,
        )
        .await;
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn delete_task_rejects_anonymous_and_member() {
        let env = TestEnv::new();
        let tasks = store();
        let task = seed_task(&tasks, TaskStatus::Todo, None);
        let denied = delete_task(
            env.data(),
            tasks.clone(),
            anonymous(),
            web::Path::from(task.id),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let forbidden = delete_task(
            env.data(),
            tasks.clone(),
            authed("member"),
            web::Path::from(task.id),
        )
        .await;
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
        assert!(tasks.get(&task.id).is_some());
    }

    #[tokio::test]
    async fn delete_task_allows_admin_and_emits_deleted_with_actor() {
        let env = TestEnv::new();
        let tasks = store();
        let task = seed_task(&tasks, TaskStatus::Todo, None);
        let response = delete_task(
            env.data(),
            tasks.clone(),
            authed("admin"),
            web::Path::from(task.id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(tasks.get(&task.id).is_none());
        let events = env.deleted.lock().expect("event sink lock poisoned");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("task_id").and_then(|value| value.as_str()),
            Some(task.id.to_string()).as_deref(),
        );
        assert_eq!(
            events[0].get("actor_id").and_then(|value| value.as_str()),
            Some("user-1")
        );
    }

    #[tokio::test]
    async fn change_task_status_rejects_anonymous_and_member() {
        let env = TestEnv::new();
        let tasks = store();
        let task = seed_task(&tasks, TaskStatus::Todo, None);
        let denied = change_task_status(
            env.data(),
            tasks.clone(),
            anonymous(),
            web::Path::from(task.id),
            web::Json(ChangeStatusInput {
                status: TaskStatus::InProgress,
            }),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let forbidden = change_task_status(
            env.data(),
            tasks.clone(),
            authed("member"),
            web::Path::from(task.id),
            web::Json(ChangeStatusInput {
                status: TaskStatus::InProgress,
            }),
        )
        .await;
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn change_task_status_done_is_terminal_and_keeps_completed_at() {
        let env = TestEnv::new();
        let tasks = store();
        let task = seed_task(&tasks, TaskStatus::Done, None);
        assert!(task.completed_at.is_some());
        let response = change_task_status(
            env.data(),
            tasks.clone(),
            authed("admin"),
            web::Path::from(task.id),
            web::Json(ChangeStatusInput {
                status: TaskStatus::Todo,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let kept = tasks.get(&task.id).expect("task still stored");
        assert_eq!(kept.status, TaskStatus::Done);
        assert!(kept.completed_at.is_some());
    }

    #[tokio::test]
    async fn change_task_status_review_to_done_sets_completed_at() {
        let env = TestEnv::new();
        let tasks = store();
        let task = seed_task(&tasks, TaskStatus::Review, None);
        assert!(task.completed_at.is_none());
        let response = change_task_status(
            env.data(),
            tasks.clone(),
            authed("admin"),
            web::Path::from(task.id),
            web::Json(ChangeStatusInput {
                status: TaskStatus::Done,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let done = tasks.get(&task.id).expect("task still stored");
        assert_eq!(done.status, TaskStatus::Done);
        assert!(done.completed_at.is_some());
    }

    #[tokio::test]
    async fn assign_task_rejects_anonymous_and_member() {
        let env = TestEnv::new();
        let tasks = store();
        let task = seed_task(&tasks, TaskStatus::Todo, None);
        let denied = assign_task(
            env.data(),
            tasks.clone(),
            anonymous(),
            web::Path::from(task.id),
            web::Json(AssignInput { assignee_id: None }),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let forbidden = assign_task(
            env.data(),
            tasks.clone(),
            authed("member"),
            web::Path::from(task.id),
            web::Json(AssignInput { assignee_id: None }),
        )
        .await;
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn assign_task_emits_only_on_real_assignment_change() {
        let env = TestEnv::new();
        let tasks = store();
        let task = seed_task(&tasks, TaskStatus::Todo, None);
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let assign = |assignee: Option<Uuid>| {
            assign_task(
                env.data(),
                tasks.clone(),
                authed("admin"),
                web::Path::from(task.id),
                web::Json(AssignInput {
                    assignee_id: assignee,
                }),
            )
        };
        // Fresh assignment emits task.assigned.
        assert_eq!(assign(Some(alice)).await.status(), StatusCode::OK);
        assert_eq!(env.assigned.lock().unwrap().len(), 1);
        assert_eq!(env.unassigned.lock().unwrap().len(), 0);
        // Re-assigning the same person emits nothing.
        assert_eq!(assign(Some(alice)).await.status(), StatusCode::OK);
        assert_eq!(env.assigned.lock().unwrap().len(), 1);
        assert_eq!(env.unassigned.lock().unwrap().len(), 0);
        // Re-assigning someone else emits task.assigned again.
        assert_eq!(assign(Some(bob)).await.status(), StatusCode::OK);
        assert_eq!(env.assigned.lock().unwrap().len(), 2);
        // Unassigning emits task.unassigned, not task.assigned.
        assert_eq!(assign(None).await.status(), StatusCode::OK);
        assert_eq!(env.assigned.lock().unwrap().len(), 2);
        assert_eq!(env.unassigned.lock().unwrap().len(), 1);
        // Unassigning an already unassigned task emits nothing.
        assert_eq!(assign(None).await.status(), StatusCode::OK);
        assert_eq!(env.assigned.lock().unwrap().len(), 2);
        assert_eq!(env.unassigned.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn log_time_rejects_anonymous_and_member() {
        let env = TestEnv::new();
        let tasks = store();
        let task = seed_task(&tasks, TaskStatus::Todo, None);
        let denied = log_time(
            env.data(),
            tasks.clone(),
            anonymous(),
            web::Path::from(task.id),
            web::Json(LogTimeInput { minutes: 30 }),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let forbidden = log_time(
            env.data(),
            tasks.clone(),
            authed("member"),
            web::Path::from(task.id),
            web::Json(LogTimeInput { minutes: 30 }),
        )
        .await;
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn list_comments_rejects_anonymous_and_member_allows_admin() {
        let env = TestEnv::new();
        let tasks = store();
        let task = seed_task(&tasks, TaskStatus::Todo, None);
        let denied = list_comments(
            env.data(),
            tasks.clone(),
            anonymous(),
            web::Path::from(task.id),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let forbidden = list_comments(
            env.data(),
            tasks.clone(),
            authed("member"),
            web::Path::from(task.id),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
        let allowed = list_comments(
            env.data(),
            tasks.clone(),
            authed("admin"),
            web::Path::from(task.id),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(allowed.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn add_comment_rejects_anonymous_and_member() {
        let env = TestEnv::new();
        let tasks = store();
        let task = seed_task(&tasks, TaskStatus::Todo, None);
        let denied = add_comment(
            env.data(),
            tasks.clone(),
            anonymous(),
            web::Path::from(task.id),
            web::Json(AddCommentInput {
                author_id: None,
                body: "Hello".to_string(),
            }),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let forbidden = add_comment(
            env.data(),
            tasks.clone(),
            authed("member"),
            web::Path::from(task.id),
            web::Json(AddCommentInput {
                author_id: None,
                body: "Hello".to_string(),
            }),
        )
        .await;
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn add_checklist_item_rejects_anonymous_and_member() {
        let env = TestEnv::new();
        let tasks = store();
        let task = seed_task(&tasks, TaskStatus::Todo, None);
        let denied = add_checklist_item(
            env.data(),
            tasks.clone(),
            anonymous(),
            web::Path::from(task.id),
            web::Json(AddChecklistInput {
                title: "Step one".to_string(),
            }),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let forbidden = add_checklist_item(
            env.data(),
            tasks.clone(),
            authed("member"),
            web::Path::from(task.id),
            web::Json(AddChecklistInput {
                title: "Step one".to_string(),
            }),
        )
        .await;
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn toggle_checklist_item_rejects_anonymous_and_member() {
        let env = TestEnv::new();
        let tasks = store();
        let mut task = seed_task(&tasks, TaskStatus::Todo, None);
        let item = ChecklistItem {
            id: Uuid::now_v7(),
            task_id: task.id,
            title: "Step one".to_string(),
            done: false,
            completed_at: None,
            created_at: Utc::now(),
        };
        task.checklist.push(item.clone());
        tasks.insert(task.id, task.clone());
        let denied = toggle_checklist_item(
            env.data(),
            tasks.clone(),
            anonymous(),
            web::Path::from((task.id, item.id)),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let forbidden = toggle_checklist_item(
            env.data(),
            tasks.clone(),
            authed("member"),
            web::Path::from((task.id, item.id)),
        )
        .await;
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn delete_checklist_item_rejects_anonymous_and_member() {
        let env = TestEnv::new();
        let tasks = store();
        let mut task = seed_task(&tasks, TaskStatus::Todo, None);
        let item = ChecklistItem {
            id: Uuid::now_v7(),
            task_id: task.id,
            title: "Step one".to_string(),
            done: false,
            completed_at: None,
            created_at: Utc::now(),
        };
        task.checklist.push(item.clone());
        tasks.insert(task.id, task.clone());
        let denied = delete_checklist_item(
            env.data(),
            tasks.clone(),
            anonymous(),
            web::Path::from((task.id, item.id)),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let forbidden = delete_checklist_item(
            env.data(),
            tasks.clone(),
            authed("member"),
            web::Path::from((task.id, item.id)),
        )
        .await;
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    }
}
