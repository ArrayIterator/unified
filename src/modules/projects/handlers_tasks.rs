// Projects module - task handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::sync::Arc;

// Task handlers

/// Shared completion side effects for a task that just entered a done
/// status: run the `task_completed` automation rules, then emit
/// `project.task.completed` and notify watchers. Every path that can
/// reach done (update, move, complete) runs this, so no path can
/// reach done silently. Returns the stored task after the rules ran.
pub(crate) fn run_task_completion_effects(
    state: &ProjectsState,
    project: &Project,
    task: &ProjectTask,
) -> ProjectTask {
    apply_automation_rules(state, project, task, "task_completed");
    let final_task = state
        .get_task(&task.id)
        .unwrap_or_else(|_| task.clone());
    state.emit(
        "project.task.completed",
        serde_json::to_value(&final_task).unwrap_or(Value::Null),
    );
    state.notify_task_watchers(
        &final_task,
        format!("Task completed: {}", final_task.title),
        format!(
            "\"{}\" was completed in project {}.",
            final_task.title, project.name
        ),
    );
    final_task
}

pub(crate) async fn list_project_tasks(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<ProjectTask>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let project = state.get_project(path.as_str())?;
        let (viewer_id, viewer_role) = viewer_of(&req);
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        let sort = params.allowed_sort(
            &[
                "created_at",
                "updated_at",
                "due_date",
                "priority",
                "estimate_points",
            ],
            "created_at",
        );
        let mut items: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| {
                task.project_id == project.id
                    && matches_search(&format!("{} {}", task.title, task.description), &params)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| task.status == *status)
                    && params.filters.get("assignee_id").is_none_or(|assignee| {
                        task.assignee_id.as_deref() == Some(assignee.as_str())
                    })
                    && params
                        .filters
                        .get("reporter_id")
                        .is_none_or(|reporter| task.reporter_id == *reporter)
                    && params
                        .filters
                        .get("task_type")
                        .is_none_or(|task_type| task.task_type == *task_type)
                    && params
                        .filters
                        .get("priority")
                        .is_none_or(|priority| task.priority == *priority)
                    && params.filters.get("label").is_none_or(|label| {
                        task.labels.iter().any(|task_label| task_label == label)
                    })
                    && params
                        .filters
                        .get("sprint_id")
                        .is_none_or(|sprint| task.sprint_id.as_deref() == Some(sprint.as_str()))
                    && params
                        .filters
                        .get("phase_id")
                        .is_none_or(|phase| task.phase_id.as_deref() == Some(phase.as_str()))
                    && params.filters.get("milestone_id").is_none_or(|milestone| {
                        task.milestone_id.as_deref() == Some(milestone.as_str())
                    })
                    && params
                        .filters
                        .get("parent_id")
                        .is_none_or(|parent| task.parent_id.as_deref() == Some(parent.as_str()))
                    && params
                        .filters
                        .get("epic_id")
                        .is_none_or(|epic| task.epic_id.as_deref() == Some(epic.as_str()))
            })
            .collect();
        s3_sort_tasks(&mut items, &sort, params.normalized().order == "desc");
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

pub(crate) async fn create_project_task(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<CreateProjectTask>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTask> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let project = state.get_project(path.as_str())?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        let title = input.title.trim().to_string();
        if title.is_empty() {
            return Err(UnifiedError::bad_request("Task title must not be empty"));
        }
        let task_type = input.task_type.unwrap_or_else(|| "task".to_string());
        if !s3_valid_task_type(&task_type) {
            return Err(UnifiedError::bad_request(
                "task_type must be one of: task, story, bug, spike, feature, enabler, chore",
            ));
        }
        let priority = input.priority.unwrap_or_else(|| "normal".to_string());
        if !s3_valid_priority(&priority) {
            return Err(UnifiedError::bad_request(
                "priority must be one of: low, normal, high, urgent",
            ));
        }
        let status = match input.status {
            Some(status) => {
                if !s3_methodology_allows_status(&project, &status) {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown status for this project methodology: {status}"
                    )));
                }
                status
            }
            None => s3_default_status(&project),
        };
        if let Some(points) = input.estimate_points {
            if points < 0.0 {
                return Err(UnifiedError::bad_request(
                    "estimate_points must not be negative",
                ));
            }
        }
        if let Some(hours) = input.estimate_hours {
            if hours < 0.0 {
                return Err(UnifiedError::bad_request(
                    "estimate_hours must not be negative",
                ));
            }
        }
        if let Some(parent_id) = &input.parent_id {
            let parent = state.get_task(parent_id.as_str())?;
            if parent.project_id != project.id {
                return Err(UnifiedError::bad_request(
                    "parent_id must reference a task in the same project",
                ));
            }
        }
        if let Some(epic_id) = &input.epic_id {
            let epic = state.get_task(epic_id.as_str())?;
            if epic.project_id != project.id {
                return Err(UnifiedError::bad_request(
                    "epic_id must reference a task in the same project",
                ));
            }
            if epic.task_type != "feature" && epic.task_type != "epic" {
                return Err(UnifiedError::bad_request(
                    "epic_id must reference a feature or epic task",
                ));
            }
        }
        s3_validate_task_refs(
            &state,
            &project.id,
            input.board_id.as_deref(),
            input.sprint_id.as_deref(),
            input.phase_id.as_deref(),
            input.cycle_id.as_deref(),
            input.pi_id.as_deref(),
            input.milestone_id.as_deref(),
        )?;
        let reporter_id = input
            .reporter_id
            .and_then(|reporter| {
                let reporter = reporter.trim().to_string();
                if reporter.is_empty() {
                    None
                } else {
                    Some(reporter)
                }
            })
            .or_else(|| viewer_id.clone())
            .unwrap_or_else(|| "system".to_string());
        let custom_values = if input.custom_values.is_null() {
            json!({})
        } else {
            input.custom_values
        };
        let task = ProjectTask {
            id: new_id(),
            project_id: project.id.clone(),
            board_id: input.board_id,
            parent_id: input.parent_id,
            epic_id: input.epic_id,
            title,
            description: input.description.unwrap_or_default(),
            task_type,
            status,
            priority,
            assignee_id: input.assignee_id,
            reporter_id,
            estimate_points: input.estimate_points,
            estimate_hours: input.estimate_hours,
            labels: input.labels,
            custom_values,
            start_date: input.start_date,
            due_date: input.due_date,
            completed_at: None,
            sprint_id: input.sprint_id,
            phase_id: input.phase_id,
            cycle_id: input.cycle_id,
            pi_id: input.pi_id,
            milestone_id: input.milestone_id,
            process_group: input.process_group,
            knowledge_area: input.knowledge_area,
            waste_flag: input.waste_flag,
            pair_partner_id: input.pair_partner_id,
            test_first: input.test_first.unwrap_or(false),
            dmaic_phase: input.dmaic_phase,
            appetite_weeks: input.appetite_weeks,
            created_at: now(),
            updated_at: now(),
        };
        state.tasks.insert(task.id.clone(), task.clone());
        apply_automation_rules(&state, &project, &task, "task_created");
        let stored = state.get_task(&task.id)?;
        let actor = viewer_id
            .as_deref()
            .unwrap_or(stored.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.created",
            "task",
            Some(stored.id.clone()),
            json!({
                "title": stored.title,
                "status": stored.status,
                "task_type": stored.task_type,
            }),
        );
        state.emit(
            "project.task.created",
            serde_json::to_value(&stored).unwrap_or(Value::Null),
        );
        if let Some(assignee_id) = &stored.assignee_id {
            state.notify(
                assignee_id,
                format!("Task assigned: {}", stored.title),
                format!(
                    "You were assigned to \"{}\" in project {}.",
                    stored.title, project.name
                ),
            );
        }
        Ok(stored)
    })();
    match result {
        Ok(task) => json::success_response(task, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn get_project_task(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTask> = (|| {
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
        Ok(task)
    })();
    json::respond(result, false)
}

pub(crate) async fn update_project_task(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateProjectTask>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTask> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let task_id = path.into_inner();
        let existing = state.get_task(&task_id)?;
        let project = state.get_project(&existing.project_id)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        if let Some(title) = &input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request("Task title must not be empty"));
            }
        }
        if let Some(priority) = &input.priority {
            if !s3_valid_priority(priority) {
                return Err(UnifiedError::bad_request(
                    "priority must be one of: low, normal, high, urgent",
                ));
            }
        }
        if let Some(status) = &input.status {
            if !s3_methodology_allows_status(&project, status) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown status for this project methodology: {status}"
                )));
            }
            if !s3_allowed_task_transition(&project, &existing.status, status) {
                return Err(UnifiedError::bad_request(format!(
                    "Task status cannot move from {} to {status}",
                    existing.status
                )));
            }
        }
        if let Some(points) = input.estimate_points {
            if points < 0.0 {
                return Err(UnifiedError::bad_request(
                    "estimate_points must not be negative",
                ));
            }
        }
        if let Some(hours) = input.estimate_hours {
            if hours < 0.0 {
                return Err(UnifiedError::bad_request(
                    "estimate_hours must not be negative",
                ));
            }
        }
        s3_validate_task_refs(
            &state,
            &project.id,
            input.board_id.as_ref().and_then(|value| value.as_deref()),
            input.sprint_id.as_ref().and_then(|value| value.as_deref()),
            input.phase_id.as_ref().and_then(|value| value.as_deref()),
            input.cycle_id.as_ref().and_then(|value| value.as_deref()),
            input.pi_id.as_ref().and_then(|value| value.as_deref()),
            input
                .milestone_id
                .as_ref()
                .and_then(|value| value.as_deref()),
        )?;
        let old_status = existing.status.clone();
        let old_assignee = existing.assignee_id.clone();
        let mut task = state
            .tasks
            .get_mut(task_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Task not found"))?;
        if let Some(title) = input.title {
            task.title = title.trim().to_string();
        }
        if let Some(description) = input.description {
            task.description = description;
        }
        if let Some(status) = input.status {
            task.status = status;
        }
        if let Some(priority) = input.priority {
            task.priority = priority;
        }
        if let Some(assignee_id) = input.assignee_id {
            task.assignee_id = assignee_id;
        }
        if let Some(points) = input.estimate_points {
            task.estimate_points = Some(points);
        }
        if let Some(hours) = input.estimate_hours {
            task.estimate_hours = Some(hours);
        }
        if let Some(labels) = input.labels {
            task.labels = labels;
        }
        if let Some(custom_values) = input.custom_values {
            task.custom_values = custom_values;
        }
        if let Some(start_date) = input.start_date {
            task.start_date = start_date;
        }
        if let Some(due_date) = input.due_date {
            task.due_date = due_date;
        }
        if let Some(sprint_id) = input.sprint_id {
            task.sprint_id = sprint_id;
        }
        if let Some(phase_id) = input.phase_id {
            task.phase_id = phase_id;
        }
        if let Some(cycle_id) = input.cycle_id {
            task.cycle_id = cycle_id;
        }
        if let Some(pi_id) = input.pi_id {
            task.pi_id = pi_id;
        }
        if let Some(milestone_id) = input.milestone_id {
            task.milestone_id = milestone_id;
        }
        if let Some(process_group) = input.process_group {
            task.process_group = process_group;
        }
        if let Some(knowledge_area) = input.knowledge_area {
            task.knowledge_area = knowledge_area;
        }
        if let Some(waste_flag) = input.waste_flag {
            task.waste_flag = Some(waste_flag);
        }
        if let Some(pair_partner_id) = input.pair_partner_id {
            task.pair_partner_id = pair_partner_id;
        }
        if let Some(test_first) = input.test_first {
            task.test_first = test_first;
        }
        if let Some(dmaic_phase) = input.dmaic_phase {
            task.dmaic_phase = dmaic_phase;
        }
        if let Some(appetite_weeks) = input.appetite_weeks {
            task.appetite_weeks = appetite_weeks;
        }
        if let Some(board_id) = input.board_id {
            task.board_id = board_id;
        }
        let status_changed = task.status != old_status;
        let became_done = status_changed && task.status == "done";
        let left_done = status_changed && old_status == "done";
        if became_done {
            task.completed_at = Some(now());
        } else if left_done {
            task.completed_at = None;
        }
        let assignee_changed = task.assignee_id != old_assignee;
        task.updated_at = now();
        let updated = task.clone();
        drop(task);
        if status_changed {
            apply_automation_rules(&state, &project, &updated, "status_changed");
            if became_done {
                run_task_completion_effects(&state, &project, &updated);
            }
        }
        let final_task = state.get_task(&task_id)?;
        let actor = viewer_id
            .as_deref()
            .unwrap_or(final_task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.updated",
            "task",
            Some(task_id.clone()),
            json!({
                "status": final_task.status,
                "previous_status": old_status,
            }),
        );
        if assignee_changed {
            if let Some(assignee_id) = &final_task.assignee_id {
                state.emit(
                    "project.task.assigned",
                    serde_json::to_value(&final_task).unwrap_or(Value::Null),
                );
                state.notify(
                    assignee_id,
                    format!("Task assigned: {}", final_task.title),
                    format!(
                        "You were assigned to \"{}\" in project {}.",
                        final_task.title, project.name
                    ),
                );
            }
        }
        Ok(final_task)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_project_task(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
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
        let deleted_tasks = s3_delete_task_cascade(&state, &task_id);
        let actor = viewer_id
            .as_deref()
            .unwrap_or(task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.deleted",
            "task",
            Some(task_id.clone()),
            json!({
                "title": task.title,
                "deleted_tasks": deleted_tasks,
            }),
        );
        Ok(json!({
            "deleted": true,
            "id": task_id,
            "deleted_tasks": deleted_tasks,
        }))
    })();
    json::respond(result, false)
}

pub(crate) async fn move_project_task(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<MoveTask>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTask> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let task_id = path.into_inner();
        let existing = state.get_task(&task_id)?;
        let project = state.get_project(&existing.project_id)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        let target_board = match &input.board_id {
            Some(board_id) => {
                s3_require_board_in_project(&state, &project.id, board_id.as_str())?;
                Some(board_id.clone())
            }
            None => existing.board_id.clone(),
        };
        let target_status = match &input.column_status_key {
            Some(column_key) => {
                let column = s3_find_column(
                    &state,
                    &project.id,
                    target_board.as_deref(),
                    column_key.as_str(),
                )
                .ok_or_else(|| {
                    UnifiedError::bad_request(format!("Unknown column: {column_key}"))
                })?;
                column.status_key
            }
            None => match &input.status {
                Some(status) => status.clone(),
                None => {
                    return Err(UnifiedError::bad_request(
                        "Provide status or column_status_key to move the task",
                    ));
                }
            },
        };
        if !s3_methodology_allows_status(&project, &target_status) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown status for this project methodology: {target_status}"
            )));
        }
        if !s3_allowed_task_transition(&project, &existing.status, &target_status) {
            return Err(UnifiedError::bad_request(format!(
                "Task status cannot move from {} to {target_status}",
                existing.status
            )));
        }
        s3_check_wip_limit(
            &state,
            &project.id,
            target_board.as_deref(),
            &target_status,
            &task_id,
        )?;
        let old_status = existing.status.clone();
        let mut task = state
            .tasks
            .get_mut(task_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Task not found"))?;
        let status_changed = task.status != target_status;
        let became_done = status_changed && target_status == "done";
        let left_done = status_changed && task.status == "done";
        task.board_id = target_board.clone();
        task.status = target_status.clone();
        if became_done {
            task.completed_at = Some(now());
        } else if left_done {
            task.completed_at = None;
        }
        task.updated_at = now();
        let updated = task.clone();
        drop(task);
        // A move runs the same automation and completion effects as
        // an update: a move into done must not skip the
        // `task_completed` rules or the `project.task.completed`
        // event that billing and notifications listen to.
        if status_changed {
            apply_automation_rules(&state, &project, &updated, "status_changed");
        }
        let mut final_task = state.get_task(&task_id)?;
        if became_done {
            final_task = run_task_completion_effects(&state, &project, &final_task);
        }
        let actor = viewer_id
            .as_deref()
            .unwrap_or(final_task.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.moved",
            "task",
            Some(task_id.clone()),
            json!({
                "from_status": old_status,
                "to_status": target_status,
                "board_id": target_board,
            }),
        );
        state.emit(
            "project.task.moved",
            json!({
                "task": serde_json::to_value(&final_task).unwrap_or(Value::Null),
                "from_status": old_status,
                "to_status": target_status,
                "board_id": target_board,
            }),
        );
        if status_changed {
            state.notify_task_watchers(
                &final_task,
                format!("Task moved: {}", final_task.title),
                format!(
                    "\"{}\" moved to {} in project {}.",
                    final_task.title, target_status, project.name
                ),
            );
        }
        Ok(final_task)
    })();
    json::respond(result, false)
}

pub(crate) async fn assign_project_task(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    body: web::Json<AssignTask>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTask> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let task_id = path.into_inner();
        let assignee_id = input.assignee_id.trim().to_string();
        if assignee_id.is_empty() {
            return Err(UnifiedError::bad_request("assignee_id must not be empty"));
        }
        let existing = state.get_task(&task_id)?;
        let project = state.get_project(&existing.project_id)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        if !state.is_member(&project.id, &assignee_id) {
            return Err(UnifiedError::bad_request(
                "Assignee must be a project member",
            ));
        }
        let mut task = state
            .tasks
            .get_mut(task_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Task not found"))?;
        task.assignee_id = Some(assignee_id.clone());
        task.updated_at = now();
        let updated = task.clone();
        drop(task);
        let actor = viewer_id
            .as_deref()
            .unwrap_or(updated.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.assigned",
            "task",
            Some(task_id.clone()),
            json!({ "assignee_id": assignee_id }),
        );
        state.emit(
            "project.task.assigned",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        state.notify(
            &assignee_id,
            format!("Task assigned: {}", updated.title),
            format!(
                "You were assigned to \"{}\" in project {}.",
                updated.title, project.name
            ),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn complete_project_task(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTask> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let (viewer_id, viewer_role) = viewer_of(&req);
        let task_id = path.into_inner();
        let existing = state.get_task(&task_id)?;
        let project = state.get_project(&existing.project_id)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        // `done` is terminal, so completing a cancelled task must
        // not revive it; completing an already-done task stays a
        // no-op. Any other open status may complete directly: this
        // dedicated handler is the one path that jumps to done.
        if existing.status == "cancelled" {
            return Err(UnifiedError::bad_request(
                "Cannot complete a cancelled task",
            ));
        }
        let already_done = s2_task_is_done(&existing.status);
        let mut task = state
            .tasks
            .get_mut(task_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Task not found"))?;
        if !already_done {
            task.status = "done".to_string();
            task.completed_at = Some(now());
            task.updated_at = now();
        }
        let updated = task.clone();
        drop(task);
        let mut final_task = state.get_task(&task_id)?;
        if !already_done {
            let actor = viewer_id
                .as_deref()
                .unwrap_or(final_task.reporter_id.as_str());
            state.log_activity(
                &project.id,
                actor,
                "task.completed",
                "task",
                Some(task_id.clone()),
                json!({}),
            );
            final_task = run_task_completion_effects(&state, &project, &updated);
        }
        Ok(final_task)
    })();
    json::respond(result, false)
}

pub(crate) async fn reopen_project_task(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ProjectTask> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let (viewer_id, viewer_role) = viewer_of(&req);
        let task_id = path.into_inner();
        let existing = state.get_task(&task_id)?;
        let project = state.get_project(&existing.project_id)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        // Reopening is only valid from a done status: a cancelled
        // task stays cancelled, and an open task has nothing to
        // reopen.
        if !s2_task_is_done(&existing.status) {
            return Err(UnifiedError::bad_request(
                "Only a completed task can be reopened",
            ));
        }
        let target_status = s3_first_open_status(&project);
        let mut task = state
            .tasks
            .get_mut(task_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Task not found"))?;
        task.status = target_status.clone();
        task.completed_at = None;
        task.updated_at = now();
        let updated = task.clone();
        drop(task);
        let actor = viewer_id
            .as_deref()
            .unwrap_or(updated.reporter_id.as_str());
        state.log_activity(
            &project.id,
            actor,
            "task.reopened",
            "task",
            Some(task_id.clone()),
            json!({ "status": target_status }),
        );
        state.emit(
            "project.task.reopened",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        state.notify_task_watchers(
            &updated,
            format!("Task reopened: {}", updated.title),
            format!(
                "\"{}\" was reopened in project {}.",
                updated.title, project.name
            ),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;
    use std::sync::Mutex;

    fn test_project() -> Project {
        Project {
            id: "p1".to_string(),
            name: "Test".to_string(),
            description: String::new(),
            methodology: "scrum".to_string(),
            status: "active".to_string(),
            visibility: "private".to_string(),
            owner_id: "u1".to_string(),
            company_id: None,
            client_id: None,
            start_date: None,
            due_date: None,
            color: None,
            icon: None,
            labels: Vec::new(),
            custom_field_definitions: json!({}),
            archived: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn done_task() -> ProjectTask {
        ProjectTask {
            id: "t1".to_string(),
            project_id: "p1".to_string(),
            board_id: None,
            parent_id: None,
            epic_id: None,
            title: "Task".to_string(),
            description: String::new(),
            task_type: "task".to_string(),
            status: "done".to_string(),
            priority: "normal".to_string(),
            assignee_id: None,
            reporter_id: "u1".to_string(),
            estimate_points: None,
            estimate_hours: None,
            labels: Vec::new(),
            custom_values: json!({}),
            start_date: None,
            due_date: None,
            completed_at: Some(Utc::now()),
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
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn completion_effects_run_rules_and_emit_completed_event() {
        let state = ProjectsState::new();
        let project = test_project();
        state.projects.insert(project.id.clone(), project.clone());
        let task = done_task();
        state.tasks.insert(task.id.clone(), task.clone());
        let rule = ProjectAutomationRule {
            id: "r1".to_string(),
            project_id: "p1".to_string(),
            name: "On complete".to_string(),
            trigger: "task_completed".to_string(),
            conditions: json!({}),
            actions: json!([{"action": "add_label", "label": "completed-label"}]),
            enabled: true,
            run_count: 0,
            last_run_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state.automation_rules.insert(rule.id.clone(), rule);
        let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = seen.clone();
        state.bus().register(
            "project.task.completed",
            "test",
            Arc::new(move |ctx| {
                seen_clone.lock().expect("seen lock poisoned").push(ctx.data.clone());
            }),
        );
        let finished = run_task_completion_effects(&state, &project, &task);
        assert_eq!(finished.id, "t1");
        let events = seen.lock().expect("seen lock poisoned");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].get("id").and_then(|value| value.as_str()), Some("t1"));
        drop(events);
        let stored = state.get_task("t1").unwrap();
        assert!(stored.labels.iter().any(|label| label == "completed-label"));
        let stored_rule = state
            .automation_rules
            .get("r1")
            .map(|entry| entry.value().clone())
            .unwrap();
        assert_eq!(stored_rule.run_count, 1);
    }
}
