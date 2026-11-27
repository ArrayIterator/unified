// Projects module - subtask, dependency, comment, watcher, and attachment handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;

// Subtasks

pub(crate) async fn list_task_subtasks(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<ProjectTask>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let task = state.get_task(path.as_str())?;
        let project = state.get_project(&task.project_id)?;
        let (viewer_id, viewer_role) = viewer_of(&req);
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        let sort = params.allowed_sort(
            &["created_at", "updated_at", "due_date", "priority"],
            "created_at",
        );
        let mut items: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|child| {
                child.parent_id.as_deref() == Some(task.id.as_str())
                    && matches_search(&format!("{} {}", child.title, child.description), &params)
            })
            .collect();
        s3_sort_tasks(&mut items, &sort, params.normalized().order == "desc");
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

// Dependencies

pub(crate) async fn list_task_dependencies(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let (viewer_id, viewer_role) = viewer_of(&req);
        let task = state.get_task(path.as_str())?;
        let project = state.get_project(&task.project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        let mut outgoing: Vec<ProjectTaskDependency> = state
            .dependencies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|dependency| dependency.task_id == task.id)
            .collect();
        let mut incoming: Vec<ProjectTaskDependency> = state
            .dependencies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|dependency| dependency.depends_on_task_id == task.id)
            .collect();
        outgoing.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        incoming.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(json!({
            "task_id": task.id,
            "outgoing": outgoing,
            "incoming": incoming,
        }))
    })();
    json::respond(result, false)
}

pub(crate) async fn add_task_dependency(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<AddTaskDependency>,
) -> HttpResponse {
    let result: UnifiedResult<(ProjectTaskDependency, bool)> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let task_id = path.into_inner();
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        let depends_on_id = input.depends_on_task_id.trim().to_string();
        if depends_on_id.is_empty() {
            return Err(UnifiedError::bad_request(
                "depends_on_task_id must not be empty",
            ));
        }
        if depends_on_id == task_id {
            return Err(UnifiedError::bad_request("A task cannot depend on itself"));
        }
        let kind = input.kind.unwrap_or_else(|| "finish_start".to_string());
        if !s3_valid_dependency_kind(&kind) {
            return Err(UnifiedError::bad_request(
                "kind must be one of: finish_start, start_start, finish_finish, start_finish, blocks, related",
            ));
        }
        let depends_on = state.get_task(&depends_on_id)?;
        if depends_on.project_id != project.id {
            return Err(UnifiedError::bad_request(
                "depends_on_task_id must reference a task in the same project",
            ));
        }
        // Cycle rejection: walk from the dependency target following
        // depends_on edges; reaching this task would close a loop.
        let mut visited = std::collections::HashSet::new();
        let mut stack = vec![depends_on_id.clone()];
        while let Some(current) = stack.pop() {
            if current == task_id {
                return Err(UnifiedError::bad_request(
                    "This dependency would create a dependency cycle",
                ));
            }
            if !visited.insert(current.clone()) {
                continue;
            }
            for entry in state.dependencies.iter() {
                if entry.value().task_id == current {
                    stack.push(entry.value().depends_on_task_id.clone());
                }
            }
        }
        if let Some(existing) = state
            .dependencies
            .iter()
            .map(|entry| entry.value().clone())
            .find(|dependency| {
                dependency.task_id == task_id
                    && dependency.depends_on_task_id == depends_on_id
                    && dependency.kind == kind
            })
        {
            return Ok((existing, false));
        }
        let dependency = ProjectTaskDependency {
            id: new_id(),
            task_id: task_id.clone(),
            depends_on_task_id: depends_on_id,
            kind,
            lag_days: input.lag_days.unwrap_or(0),
            created_at: now(),
        };
        state
            .dependencies
            .insert(dependency.id.clone(), dependency.clone());
        let actor = viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "dependency.added",
            "dependency",
            Some(dependency.id.clone()),
            json!({
                "task_id": task_id,
                "depends_on_task_id": dependency.depends_on_task_id,
                "kind": dependency.kind,
            }),
        );
        Ok((dependency, true))
    })();
    match result {
        Ok((dependency, is_new)) => {
            json::success_response(dependency, if is_new { 201 } else { 200 })
        }
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn remove_task_dependency(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let (viewer_id, viewer_role) = viewer_of(&req);
        let (task_id, dependency_id) = path.into_inner();
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        let dependency = state
            .dependencies
            .get(dependency_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Dependency not found"))?;
        if dependency.task_id != task_id {
            return Err(UnifiedError::not_found("Dependency not found"));
        }
        state.dependencies.remove(dependency_id.as_str());
        let actor = viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "dependency.removed",
            "dependency",
            Some(dependency_id.clone()),
            json!({ "task_id": task_id }),
        );
        Ok(json!({ "deleted": true, "id": dependency_id }))
    })();
    json::respond(result, false)
}

// Comments

pub(crate) async fn list_task_comments(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<ProjectTaskComment>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let task = state.get_task(path.as_str())?;
        let project = state.get_project(&task.project_id)?;
        let (viewer_id, viewer_role) = viewer_of(&req);
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        let sort = params.allowed_sort(&["created_at", "updated_at"], "created_at");
        let mut items: Vec<ProjectTaskComment> = state
            .comments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|comment| comment.task_id == task.id && matches_search(&comment.body, &params))
            .collect();
        match sort.as_str() {
            "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

pub(crate) async fn add_task_comment(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<AddTaskComment>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTaskComment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let task_id = path.into_inner();
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        let body_text = input.body.trim().to_string();
        if body_text.is_empty() {
            return Err(UnifiedError::bad_request("Comment body must not be empty"));
        }
        // The comment author is the header caller: a body
        // `author_id` naming someone else is forgery unless the
        // caller holds `projects:manage` (admin/on-behalf flow).
        let author_id = resolve_body_actor(
            &state,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &input.author_id,
            "author_id",
        )?;
        let comment = ProjectTaskComment {
            id: new_id(),
            task_id: task_id.clone(),
            author_id: author_id.clone(),
            body: body_text,
            created_at: now(),
            updated_at: now(),
        };
        state.comments.insert(comment.id.clone(), comment.clone());
        let actor = viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.commented",
            "comment",
            Some(comment.id.clone()),
            json!({ "task_id": task_id }),
        );
        state.emit(
            "project.task.commented",
            json!({
                "task": serde_json::to_value(&task).unwrap_or(Value::Null),
                "comment": serde_json::to_value(&comment).unwrap_or(Value::Null),
            }),
        );
        state.notify_task_watchers(
            &task,
            format!("New comment on: {}", task.title),
            format!(
                "{} commented on \"{}\" in project {}.",
                author_id, task.title, project.name
            ),
        );
        if let Some(assignee_id) = &task.assignee_id {
            if assignee_id != &author_id {
                state.notify(
                    assignee_id,
                    format!("New comment on: {}", task.title),
                    format!(
                        "{} commented on \"{}\" in project {}.",
                        author_id, task.title, project.name
                    ),
                );
            }
        }
        Ok(comment)
    })();
    match result {
        Ok(comment) => json::success_response(comment, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn update_task_comment(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateTaskComment>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTaskComment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let (task_id, comment_id) = path.into_inner();
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        let comment = state
            .comments
            .get(comment_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Comment not found"))?;
        if comment.task_id != task_id {
            return Err(UnifiedError::not_found("Comment not found"));
        }
        // Only the author may edit, verified against the viewer id.
        match viewer_id.as_deref() {
            Some(viewer_id) if viewer_id == comment.author_id.as_str() => {}
            _ => {
                return Err(UnifiedError::forbidden(
                    "Only the comment author may edit this comment",
                ));
            }
        }
        let body_text = input.body.trim().to_string();
        if body_text.is_empty() {
            return Err(UnifiedError::bad_request("Comment body must not be empty"));
        }
        let mut stored = state
            .comments
            .get_mut(comment_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Comment not found"))?;
        stored.body = body_text;
        stored.updated_at = now();
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "comment.updated",
            "comment",
            Some(comment_id.clone()),
            json!({ "task_id": task_id }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_task_comment(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let (viewer_id, viewer_role) = viewer_of(&req);
        let (task_id, comment_id) = path.into_inner();
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        let comment = state
            .comments
            .get(comment_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Comment not found"))?;
        if comment.task_id != task_id {
            return Err(UnifiedError::not_found("Comment not found"));
        }
        // The author may delete their own comment; anyone else needs the
        // project management capability.
        let is_author = viewer_id.as_deref() == Some(comment.author_id.as_str());
        if !is_author {
            require_capability(&state, viewer_role.as_deref(), "projects:manage")?;
        }
        state.comments.remove(comment_id.as_str());
        let actor = viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "comment.deleted",
            "comment",
            Some(comment_id.clone()),
            json!({ "task_id": task_id }),
        );
        Ok(json!({ "deleted": true, "id": comment_id }))
    })();
    json::respond(result, false)
}

// Watchers

pub(crate) async fn list_task_watchers(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<ProjectTaskWatcher>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let task = state.get_task(path.as_str())?;
        let project = state.get_project(&task.project_id)?;
        let (viewer_id, viewer_role) = viewer_of(&req);
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        let sort = params.allowed_sort(&["created_at"], "created_at");
        let mut items: Vec<ProjectTaskWatcher> = state
            .watchers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|watcher| watcher.task_id == task.id)
            .collect();
        match sort.as_str() {
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

pub(crate) async fn add_task_watcher(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<AddTaskWatcher>,
) -> HttpResponse {
    let result: UnifiedResult<(ProjectTaskWatcher, bool)> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let task_id = path.into_inner();
        let user_id = input.user_id.trim().to_string();
        if user_id.is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        // Idempotent: adding the same watcher twice returns the record
        // that already exists.
        if let Some(existing) = state
            .watchers
            .iter()
            .map(|entry| entry.value().clone())
            .find(|watcher| watcher.task_id == task_id && watcher.user_id == user_id)
        {
            return Ok((existing, false));
        }
        let watcher = ProjectTaskWatcher {
            id: new_id(),
            task_id: task_id.clone(),
            user_id: user_id.clone(),
            created_at: now(),
        };
        state.watchers.insert(watcher.id.clone(), watcher.clone());
        let actor = viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "watcher.added",
            "watcher",
            Some(watcher.id.clone()),
            json!({ "task_id": task_id, "user_id": user_id }),
        );
        Ok((watcher, true))
    })();
    match result {
        Ok((watcher, is_new)) => json::success_response(watcher, if is_new { 201 } else { 200 }),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn remove_task_watcher(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let (viewer_id, viewer_role) = viewer_of(&req);
        let (task_id, watcher_id) = path.into_inner();
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        let watcher = state
            .watchers
            .get(watcher_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Watcher not found"))?;
        if watcher.task_id != task_id {
            return Err(UnifiedError::not_found("Watcher not found"));
        }
        state.watchers.remove(watcher_id.as_str());
        let actor = viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "watcher.removed",
            "watcher",
            Some(watcher_id.clone()),
            json!({ "task_id": task_id }),
        );
        Ok(json!({ "deleted": true, "id": watcher_id }))
    })();
    json::respond(result, false)
}

// Attachments (storage references only; bytes live in the Storage module)

pub(crate) async fn list_task_attachments(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<ProjectTaskAttachment>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let task = state.get_task(path.as_str())?;
        let project = state.get_project(&task.project_id)?;
        let (viewer_id, viewer_role) = viewer_of(&req);
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        let sort = params.allowed_sort(&["uploaded_at", "filename", "size_bytes"], "uploaded_at");
        let mut items: Vec<ProjectTaskAttachment> = state
            .attachments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|attachment| {
                attachment.task_id == task.id
                    && matches_search(
                        &format!("{} {}", attachment.filename, attachment.key),
                        &params,
                    )
            })
            .collect();
        match sort.as_str() {
            "filename" => items.sort_by(|a, b| a.filename.cmp(&b.filename)),
            "size_bytes" => {
                items.sort_by(|a, b| a.size_bytes.unwrap_or(0).cmp(&b.size_bytes.unwrap_or(0)))
            }
            _ => items.sort_by(|a, b| a.uploaded_at.cmp(&b.uploaded_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

pub(crate) async fn add_task_attachment(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<AddTaskAttachment>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTaskAttachment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let task_id = path.into_inner();
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        let target = input.target.trim().to_string();
        if target.is_empty() {
            return Err(UnifiedError::bad_request("target must not be empty"));
        }
        let key = input.key.trim().to_string();
        if key.is_empty() {
            return Err(UnifiedError::bad_request("key must not be empty"));
        }
        // The uploader is the header caller: a body `uploaded_by`
        // naming someone else is forgery unless the caller holds
        // `projects:manage` (admin/on-behalf flow).
        let uploaded_by = resolve_body_actor(
            &state,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &input.uploaded_by,
            "uploaded_by",
        )?;
        let stored_key = input
            .stored_key
            .and_then(|stored_key| {
                let stored_key = stored_key.trim().to_string();
                if stored_key.is_empty() {
                    None
                } else {
                    Some(stored_key)
                }
            })
            .unwrap_or_else(|| key.clone());
        let filename = input
            .filename
            .and_then(|filename| {
                let filename = filename.trim().to_string();
                if filename.is_empty() {
                    None
                } else {
                    Some(filename)
                }
            })
            .unwrap_or_else(|| key.clone());
        // Only the storage reference is stored here; the file itself must
        // already exist in the Storage module. No bytes are kept.
        let attachment = ProjectTaskAttachment {
            id: new_id(),
            task_id: task_id.clone(),
            target,
            key,
            stored_key,
            filename,
            content_type: input.content_type,
            size_bytes: input.size_bytes,
            uploaded_by,
            uploaded_at: now(),
        };
        state
            .attachments
            .insert(attachment.id.clone(), attachment.clone());
        let actor = viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "attachment.added",
            "attachment",
            Some(attachment.id.clone()),
            json!({ "task_id": task_id, "filename": attachment.filename }),
        );
        Ok(attachment)
    })();
    match result {
        Ok(attachment) => json::success_response(attachment, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn delete_task_attachment(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let (viewer_id, viewer_role) = viewer_of(&req);
        let (task_id, attachment_id) = path.into_inner();
        let task = state.get_task(&task_id)?;
        let project = state.get_project(&task.project_id)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        let attachment = state
            .attachments
            .get(attachment_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attachment not found"))?;
        if attachment.task_id != task_id {
            return Err(UnifiedError::not_found("Attachment not found"));
        }
        state.attachments.remove(attachment_id.as_str());
        let actor = viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "attachment.deleted",
            "attachment",
            Some(attachment_id.clone()),
            json!({ "task_id": task_id }),
        );
        Ok(json!({ "deleted": true, "id": attachment_id }))
    })();
    json::respond(result, false)
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

    use super::*;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    /// A state whose `role.check` hook allows exactly the given
    /// permissions (all others denied).
    fn allowing_state(allowed: &[&str]) -> Arc<ProjectsState> {
        use unified_core::hooks::HookContext;
        let state = Arc::new(ProjectsState::new());
        let allowed: Vec<String> = allowed
            .iter()
            .map(|permission| permission.to_string())
            .collect();
        state.bus().register(
            "role.check",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                let permission = ctx
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.set(
                    "allowed",
                    json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        state
    }

    fn seed_project(state: &ProjectsState) {
        state.projects.insert(
            "p1".to_string(),
            Project {
                id: "p1".to_string(),
                name: "Test".to_string(),
                description: String::new(),
                methodology: "scrum".to_string(),
                status: "active".to_string(),
                visibility: "private".to_string(),
                owner_id: "user-1".to_string(),
                company_id: None,
                client_id: None,
                start_date: None,
                due_date: None,
                color: None,
                icon: None,
                labels: Vec::new(),
                custom_field_definitions: json!({}),
                archived: false,
                created_at: now(),
                updated_at: now(),
            },
        );
    }

    fn seed_task(state: &ProjectsState) {
        state.tasks.insert(
            "t1".to_string(),
            ProjectTask {
                id: "t1".to_string(),
                project_id: "p1".to_string(),
                board_id: None,
                parent_id: None,
                epic_id: None,
                title: "Task".to_string(),
                description: String::new(),
                task_type: "task".to_string(),
                status: "todo".to_string(),
                priority: "normal".to_string(),
                assignee_id: None,
                reporter_id: "user-1".to_string(),
                estimate_points: None,
                estimate_hours: None,
                labels: Vec::new(),
                custom_values: json!({}),
                start_date: None,
                due_date: None,
                completed_at: None,
                sprint_id: None,
                phase_id: None,
                cycle_id: None,
                pi_id: None,
                milestone_id: None,
                process_group: None,
                knowledge_area: None,
                waste_flag: None,
                pair_partner_id: None,
                test_first: false,
                dmaic_phase: None,
                appetite_weeks: None,
                created_at: now(),
                updated_at: now(),
            },
        );
    }

    fn caller_req(role: &str) -> HttpRequest {
        verified_request("user-1", role)
    }

    fn comment_body(author_id: &str) -> web::Json<AddTaskComment> {
        web::Json(AddTaskComment {
            author_id: author_id.to_string(),
            body: "hello".to_string(),
        })
    }

    fn attachment_body(uploaded_by: &str) -> web::Json<AddTaskAttachment> {
        web::Json(AddTaskAttachment {
            target: "task".to_string(),
            key: "file.txt".to_string(),
            stored_key: None,
            filename: None,
            content_type: None,
            size_bytes: None,
            uploaded_by: uploaded_by.to_string(),
        })
    }

    #[tokio::test]
    async fn add_task_comment_rejects_forged_author() {
        let state = allowing_state(&["projects:edit"]);
        seed_project(&state);
        seed_task(&state);
        let resp = add_task_comment(
            caller_req("member"),
            web::Data::new(state.clone()),
            web::Path::from("t1".to_string()),
            comment_body("someone-else"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(state.comments.is_empty());
    }

    #[tokio::test]
    async fn add_task_comment_records_the_caller() {
        let state = allowing_state(&["projects:edit"]);
        seed_project(&state);
        seed_task(&state);
        let resp = add_task_comment(
            caller_req("member"),
            web::Data::new(state.clone()),
            web::Path::from("t1".to_string()),
            comment_body(&test_user_uuid("user-1").to_string()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let stored = state
            .comments
            .iter()
            .next()
            .expect("comment stored")
            .value()
            .clone();
        assert_eq!(stored.author_id, test_user_uuid("user-1").to_string());
    }

    #[tokio::test]
    async fn add_task_comment_allows_manage_on_behalf() {
        let state = allowing_state(&["projects:edit", "projects:manage"]);
        seed_project(&state);
        seed_task(&state);
        let resp = add_task_comment(
            caller_req("admin"),
            web::Data::new(state.clone()),
            web::Path::from("t1".to_string()),
            comment_body("someone-else"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let stored = state
            .comments
            .iter()
            .next()
            .expect("comment stored")
            .value()
            .clone();
        assert_eq!(stored.author_id, "someone-else");
    }

    #[tokio::test]
    async fn add_task_attachment_rejects_forged_uploader() {
        let state = allowing_state(&["projects:edit"]);
        seed_project(&state);
        seed_task(&state);
        let resp = add_task_attachment(
            caller_req("member"),
            web::Data::new(state.clone()),
            web::Path::from("t1".to_string()),
            attachment_body("someone-else"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(state.attachments.is_empty());
    }

    #[tokio::test]
    async fn add_task_attachment_allows_manage_on_behalf() {
        let state = allowing_state(&["projects:edit", "projects:manage"]);
        seed_project(&state);
        seed_task(&state);
        let resp = add_task_attachment(
            caller_req("admin"),
            web::Data::new(state.clone()),
            web::Path::from("t1".to_string()),
            attachment_body("someone-else"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let stored = state
            .attachments
            .iter()
            .next()
            .expect("attachment stored")
            .value()
            .clone();
        assert_eq!(stored.uploaded_by, "someone-else");
    }
}
