// Projects module — project board and column handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Project boards
// ---------------------------------------------------------------------------

pub(crate) async fn list_project_boards(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let project_id = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let project = match state.get_project(&project_id) {
        Ok(project) => project,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = require_project_access(
        &state,
        &project,
        viewer_id.as_deref(),
        viewer_role.as_deref(),
        &_scope,
    ) {
        return json::error_response(&error, false);
    }
    let sort = params.allowed_sort(&["created_at", "updated_at", "name"], "created_at");
    let mut items: Vec<ProjectBoard> = state
        .boards
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|board| {
            board.project_id == project_id
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| board.kind == *kind)
        })
        .collect();
    match sort.as_str() {
        "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_project_board(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateProjectBoard>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectBoard> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let input = body.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err(UnifiedError::bad_request("Board name must not be empty"));
        }
        if let Some(limit) = input.wip_limit {
            if limit == 0 {
                return Err(UnifiedError::bad_request(
                    "wip_limit must be greater than 0",
                ));
            }
        }
        let kind = match s2_clean_optional(input.kind) {
            Some(kind) => kind,
            None => methodology_profile(&project.methodology)
                .map(|profile| profile.default_board_kind.to_string())
                .unwrap_or_else(|| "kanban".to_string()),
        };
        let timestamp = now();
        let board = ProjectBoard {
            id: new_id(),
            project_id: project_id.clone(),
            name,
            kind,
            wip_limit: input.wip_limit,
            created_at: timestamp,
            updated_at: timestamp,
        };
        state.boards.insert(board.id.clone(), board.clone());
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "board.created",
            "board",
            Some(board.id.clone()),
            json!({ "name": board.name, "kind": board.kind }),
        );
        Ok(board)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn get_project_board(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (project_id, board_id) = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectBoard> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s2_get_board(&state, &project_id, &board_id)
    })();
    json::respond(result, false)
}

pub(crate) async fn update_project_board(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProjectBoard>,
) -> HttpResponse {
    let (project_id, board_id) = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectBoard> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let input = body.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        s2_get_board(&state, &project_id, &board_id)?;
        if let Some(limit) = input.wip_limit {
            if limit == 0 {
                return Err(UnifiedError::bad_request(
                    "wip_limit must be greater than 0",
                ));
            }
        }
        let mut stored = state
            .boards
            .get_mut(&board_id)
            .ok_or_else(|| UnifiedError::not_found("Project board not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Board name must not be empty"));
            }
            stored.name = name.trim().to_string();
        }
        if let Some(kind) = input.kind {
            if kind.trim().is_empty() {
                return Err(UnifiedError::bad_request("Board kind must not be empty"));
            }
            stored.kind = kind.trim().to_string();
        }
        if input.wip_limit.is_some() {
            stored.wip_limit = input.wip_limit;
        }
        stored.updated_at = now();
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "board.updated",
            "board",
            Some(board_id.clone()),
            json!({ "name": updated.name }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_project_board(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (project_id, board_id) = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        let board = s2_get_board(&state, &project_id, &board_id)?;
        state.boards.remove(&board_id);
        state
            .columns
            .retain(|_, column| column.board_id != board_id);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "board.deleted",
            "board",
            Some(board_id.clone()),
            json!({ "name": board.name }),
        );
        Ok(json!({ "deleted": true, "id": board_id }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Board columns
// ---------------------------------------------------------------------------

/// Move every task sitting in a deleted column to the board's first
/// remaining column (lowest position), so no task keeps pointing at a
/// column that no longer exists. Returns the number of tasks moved.
/// When the column holds tasks and no other column remains, deletion
/// is rejected instead of orphaning the tasks.
pub(crate) fn reassign_tasks_from_deleted_column(
    state: &ProjectsState,
    board_id: &str,
    column: &ProjectBoardColumn,
) -> UnifiedResult<usize> {
    let mut remaining: Vec<ProjectBoardColumn> = state
        .columns
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|other| other.board_id == board_id && other.id != column.id)
        .collect();
    remaining.sort_by_key(|other| other.position);
    let task_ids: Vec<String> = state
        .tasks
        .iter()
        .filter(|entry| {
            let task = entry.value();
            task.board_id.as_deref() == Some(board_id) && task.status == column.status_key
        })
        .map(|entry| entry.key().clone())
        .collect();
    if !task_ids.is_empty() && remaining.is_empty() {
        return Err(UnifiedError::bad_request(
            "Cannot delete the last column while it still holds tasks",
        ));
    }
    let mut moved = 0;
    if let Some(target_status) = remaining.first().map(|other| other.status_key.clone()) {
        for task_id in task_ids {
            if let Some(mut task) = state.tasks.get_mut(task_id.as_str()) {
                task.status = target_status.clone();
                if s2_task_is_done(&target_status) {
                    task.completed_at = Some(now());
                }
                task.updated_at = now();
                moved += 1;
            }
        }
    }
    Ok(moved)
}

pub(crate) async fn list_board_columns(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let (project_id, board_id) = path.into_inner();
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let project = match state.get_project(&project_id) {
        Ok(project) => project,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = require_project_access(
        &state,
        &project,
        viewer_id.as_deref(),
        viewer_role.as_deref(),
        &_scope,
    ) {
        return json::error_response(&error, false);
    }
    if let Err(error) = s2_get_board(&state, &project_id, &board_id) {
        return json::error_response(&error, false);
    }
    let sort = params.allowed_sort(&["position", "name"], "position");
    let mut items: Vec<ProjectBoardColumn> = state
        .columns
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|column| column.board_id == board_id)
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        _ => items.sort_by(|a, b| a.position.cmp(&b.position)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_board_column(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateBoardColumn>,
) -> HttpResponse {
    let (project_id, board_id) = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectBoardColumn> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let input = body.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        s2_get_board(&state, &project_id, &board_id)?;
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err(UnifiedError::bad_request("Column name must not be empty"));
        }
        if let Some(limit) = input.wip_limit {
            if limit == 0 {
                return Err(UnifiedError::bad_request(
                    "wip_limit must be greater than 0",
                ));
            }
        }
        let position = match input.position {
            Some(position) => position,
            None => state
                .columns
                .iter()
                .filter(|entry| entry.value().board_id == board_id)
                .map(|entry| entry.value().position)
                .max()
                .map(|max| max.saturating_add(1))
                .unwrap_or(0),
        };
        let status_key = match input.status_key {
            Some(key) if !key.trim().is_empty() => s2_normalize_status_key(&key),
            _ => s2_normalize_status_key(&name),
        };
        let column = ProjectBoardColumn {
            id: new_id(),
            board_id: board_id.clone(),
            name,
            position,
            status_key,
            wip_limit: input.wip_limit,
        };
        state.columns.insert(column.id.clone(), column.clone());
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "column.created",
            "column",
            Some(column.id.clone()),
            json!({ "name": column.name, "board_id": board_id }),
        );
        Ok(column)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn update_board_column(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateBoardColumn>,
) -> HttpResponse {
    let (project_id, board_id, column_id) = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectBoardColumn> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let input = body.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        s2_get_board(&state, &project_id, &board_id)?;
        s2_get_column(&state, &board_id, &column_id)?;
        if let Some(limit) = input.wip_limit {
            if limit == 0 {
                return Err(UnifiedError::bad_request(
                    "wip_limit must be greater than 0",
                ));
            }
        }
        let mut stored = state
            .columns
            .get_mut(&column_id)
            .ok_or_else(|| UnifiedError::not_found("Board column not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Column name must not be empty"));
            }
            stored.name = name.trim().to_string();
        }
        if let Some(status_key) = input.status_key {
            if status_key.trim().is_empty() {
                return Err(UnifiedError::bad_request("status_key must not be empty"));
            }
            stored.status_key = s2_normalize_status_key(&status_key);
        }
        if let Some(position) = input.position {
            stored.position = position;
        }
        if input.wip_limit.is_some() {
            stored.wip_limit = input.wip_limit;
        }
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "column.updated",
            "column",
            Some(column_id.clone()),
            json!({ "name": updated.name, "board_id": board_id }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_board_column(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (project_id, board_id, column_id) = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_capability(&state, viewer_role.as_deref(), "projects:edit")?;
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        s2_get_board(&state, &project_id, &board_id)?;
        let column = s2_get_column(&state, &board_id, &column_id)?;
        let moved = reassign_tasks_from_deleted_column(&state, &board_id, &column)?;
        state.columns.remove(&column_id);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "column.deleted",
            "column",
            Some(column_id.clone()),
            json!({ "name": column.name, "board_id": board_id, "tasks_moved": moved }),
        );
        Ok(json!({ "deleted": true, "id": column_id }))
    })();
    json::respond(result, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn column(id: &str, board_id: &str, position: u32, status_key: &str) -> ProjectBoardColumn {
        ProjectBoardColumn {
            id: id.to_string(),
            board_id: board_id.to_string(),
            name: status_key.to_string(),
            position,
            status_key: status_key.to_string(),
            wip_limit: None,
        }
    }

    fn task_on_board(id: &str, board_id: &str, status: &str) -> ProjectTask {
        ProjectTask {
            id: id.to_string(),
            project_id: "p1".to_string(),
            board_id: Some(board_id.to_string()),
            parent_id: None,
            epic_id: None,
            title: "Task".to_string(),
            description: String::new(),
            task_type: "task".to_string(),
            status: status.to_string(),
            priority: "normal".to_string(),
            assignee_id: None,
            reporter_id: "u1".to_string(),
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
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn deleted_column_tasks_move_to_the_first_remaining_column() {
        let state = ProjectsState::new();
        state.columns.insert("c1".to_string(), column("c1", "b1", 0, "backlog"));
        state.columns.insert("c2".to_string(), column("c2", "b1", 1, "review"));
        state.columns.insert("c3".to_string(), column("c3", "b1", 2, "done"));
        state.tasks.insert("t1".to_string(), task_on_board("t1", "b1", "review"));
        state.tasks.insert("t2".to_string(), task_on_board("t2", "b1", "backlog"));
        let deleted = column("c2", "b1", 1, "review");
        let moved = reassign_tasks_from_deleted_column(&state, "b1", &deleted).unwrap();
        assert_eq!(moved, 1);
        assert_eq!(state.get_task("t1").unwrap().status, "backlog");
        assert_eq!(state.get_task("t2").unwrap().status, "backlog");
    }

    #[test]
    fn last_column_with_tasks_cannot_be_deleted() {
        let state = ProjectsState::new();
        state.columns.insert("c1".to_string(), column("c1", "b1", 0, "backlog"));
        state.tasks.insert("t1".to_string(), task_on_board("t1", "b1", "backlog"));
        let deleted = column("c1", "b1", 0, "backlog");
        assert!(reassign_tasks_from_deleted_column(&state, "b1", &deleted).is_err());
        let state = ProjectsState::new();
        state.columns.insert("c1".to_string(), column("c1", "b1", 0, "backlog"));
        assert_eq!(
            reassign_tasks_from_deleted_column(&state, "b1", &deleted).unwrap(),
            0
        );
    }
}
