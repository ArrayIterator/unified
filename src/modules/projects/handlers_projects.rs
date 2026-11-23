// Projects module — project handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

/// Whether a project appears in a list for a viewer: capability
/// holders see everything, owners and members see their own projects,
/// and anyone else — including an anonymous caller with no viewer at
/// all — sees nothing.
pub(crate) fn list_project_visible(
    project: &Project,
    viewer_id: Option<&str>,
    is_member: bool,
    can_view_all: bool,
) -> bool {
    if can_view_all {
        return true;
    }
    match viewer_id {
        Some(viewer) => project.owner_id == viewer || is_member,
        None => false,
    }
}

pub(crate) async fn list_projects(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
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

    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    // A viewer without the global view capability only sees projects
    // they own or are a member of. No viewer means an empty list.
    let can_view_all = match viewer_role.as_deref() {
        Some(role) => state.check_capability(role, "projects:view"),
        None => false,
    };
    let sort = params.allowed_sort(
        &["created_at", "updated_at", "name", "due_date"],
        "created_at",
    );
    let mut items: Vec<Project> = state
        .projects
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|project| {
            matches_search(
                &format!("{} {}", project.name, project.description),
                &params,
            ) && params
                .filters
                .get("methodology")
                .is_none_or(|value| project.methodology == *value)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|value| project.status == *value)
                && params
                    .filters
                    .get("visibility")
                    .is_none_or(|value| project.visibility == *value)
                && params
                    .filters
                    .get("owner_id")
                    .is_none_or(|value| project.owner_id == *value)
                && params
                    .filters
                    .get("company_id")
                    .is_none_or(|value| project.company_id.as_deref() == Some(value.as_str()))
                && params
                    .filters
                    .get("archived")
                    .is_none_or(|value| project.archived == (value == "true"))
                && list_project_visible(
                    project,
                    viewer_id.as_deref(),
                    viewer_id
                        .as_ref()
                        .is_some_and(|viewer| state.is_member(&project.id, viewer)),
                    can_view_all,
                )
                && in_company_scope(&project.company_id, &_scope)
        })
        .collect();
    match sort.as_str() {
        "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "due_date" => items.sort_by(|a, b| a.due_date.cmp(&b.due_date)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_project(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateProject>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<Project> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_capability(&state, viewer_role.as_deref(), "projects:create")?;
        let input = body.into_inner();
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err(UnifiedError::bad_request("Project name must not be empty"));
        }
        let methodology = match input.methodology {
            Some(value) if valid_methodology(value.trim()) => value.trim().to_string(),
            Some(value) => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown methodology: {}",
                    value.trim()
                )));
            }
            None => {
                return Err(UnifiedError::bad_request("methodology must not be empty"));
            }
        };
        let status = match input.status {
            Some(value) if s2_valid_project_status(value.trim()) => value.trim().to_string(),
            Some(value) => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown project status: {}",
                    value.trim()
                )));
            }
            None => "active".to_string(),
        };
        let visibility = match input.visibility {
            Some(value) if s2_valid_visibility(value.trim()) => value.trim().to_string(),
            Some(value) => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown visibility: {}",
                    value.trim()
                )));
            }
            None => "private".to_string(),
        };
        let start_date = input.start_date;
        let due_date = input.due_date;
        if let (Some(start), Some(due)) = (start_date, due_date) {
            if due < start {
                return Err(UnifiedError::bad_request(
                    "due_date must not be earlier than start_date",
                ));
            }
        }
        let owner_id = match s2_clean_optional(input.owner_id) {
            Some(owner) => owner,
            None => match viewer_id.clone() {
                Some(viewer) if !viewer.trim().is_empty() => viewer.trim().to_string(),
                _ => {
                    return Err(UnifiedError::bad_request("owner_id must not be empty"));
                }
            },
        };
        let profile = methodology_profile(&methodology)
            .ok_or_else(|| UnifiedError::bad_request("Unknown methodology"))?;
        // A body company must match the caller scope (or be adopted
        // from it when omitted); an unresolved caller may not claim
        // a company.
        let company_id = require_company_match(
            &_tenant,
            &req,
            &s2_clean_optional(input.company_id),
        )?;
        let timestamp = now();
        let project = Project {
            id: new_id(),
            name: name.clone(),
            description: s2_clean_optional(input.description).unwrap_or_default(),
            methodology: methodology.clone(),
            status,
            visibility,
            owner_id: owner_id.clone(),
            company_id,
            client_id: s2_clean_optional(input.client_id),
            start_date,
            due_date,
            color: s2_clean_optional(input.color),
            icon: s2_clean_optional(input.icon),
            labels: input
                .labels
                .unwrap_or_default()
                .into_iter()
                .map(|label| label.trim().to_string())
                .filter(|label| !label.is_empty())
                .collect(),
            custom_field_definitions: input.custom_field_definitions.unwrap_or_else(|| json!({})),
            archived: false,
            created_at: timestamp,
            updated_at: timestamp,
        };
        state.projects.insert(project.id.clone(), project.clone());
        // The owner joins the project as its owner member.
        let owner_member = ProjectMember {
            id: new_id(),
            project_id: project.id.clone(),
            user_id: owner_id.clone(),
            role: "owner".to_string(),
            joined_at: timestamp,
        };
        state.members.insert(owner_member.id.clone(), owner_member);
        // Default board from the methodology profile, with one column
        // per default column in profile order.
        let board = ProjectBoard {
            id: new_id(),
            project_id: project.id.clone(),
            name: format!("{name} Board"),
            kind: profile.default_board_kind.to_string(),
            wip_limit: None,
            created_at: timestamp,
            updated_at: timestamp,
        };
        state.boards.insert(board.id.clone(), board.clone());
        for (position, column_name) in profile.default_columns.iter().enumerate() {
            let column_name = column_name.to_string();
            let column = ProjectBoardColumn {
                id: new_id(),
                board_id: board.id.clone(),
                name: column_name.clone(),
                position: position as u32,
                status_key: s2_normalize_status_key(&column_name),
                wip_limit: None,
            };
            state.columns.insert(column.id.clone(), column);
        }
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project.id,
            actor,
            "project.created",
            "project",
            Some(project.id.clone()),
            json!({ "name": project.name, "methodology": project.methodology }),
        );
        state.emit(
            "project.created",
            serde_json::to_value(&project).unwrap_or(Value::Null),
        );
        Ok(project)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn get_project(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<Project> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        Ok(project)
    })();
    json::respond(result, false)
}

pub(crate) async fn update_project(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProject>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<Project> = (|| {
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
        // A methodology change is only allowed while the project has
        // no tasks yet; existing tasks already follow the old method.
        if let Some(methodology) = input
            .methodology
            .as_ref()
            .map(|value| value.trim().to_string())
        {
            if methodology != project.methodology {
                if !valid_methodology(&methodology) {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown methodology: {methodology}"
                    )));
                }
                let has_tasks = state
                    .tasks
                    .iter()
                    .any(|entry| entry.value().project_id == project_id);
                if has_tasks {
                    return Err(UnifiedError::bad_request(
                        "Cannot change methodology while the project has tasks",
                    ));
                }
            }
        }
        let new_start = input.start_date.or(project.start_date);
        let new_due = input.due_date.or(project.due_date);
        if let (Some(start), Some(due)) = (new_start, new_due) {
            if due < start {
                return Err(UnifiedError::bad_request(
                    "due_date must not be earlier than start_date",
                ));
            }
        }
        let mut stored = state
            .projects
            .get_mut(&project_id)
            .ok_or_else(|| UnifiedError::not_found("Project not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Project name must not be empty"));
            }
            stored.name = name.trim().to_string();
        }
        if let Some(description) = input.description {
            stored.description = description.trim().to_string();
        }
        if let Some(methodology) = input.methodology {
            stored.methodology = methodology.trim().to_string();
        }
        if let Some(status) = input.status {
            if !s2_valid_project_status(status.trim()) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown project status: {}",
                    status.trim()
                )));
            }
            stored.status = status.trim().to_string();
        }
        if let Some(visibility) = input.visibility {
            if !s2_valid_visibility(visibility.trim()) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown visibility: {}",
                    visibility.trim()
                )));
            }
            stored.visibility = visibility.trim().to_string();
        }
        if input.start_date.is_some() {
            stored.start_date = input.start_date;
        }
        if input.due_date.is_some() {
            stored.due_date = input.due_date;
        }
        if let Some(color) = input.color {
            stored.color = s2_clean_optional(Some(color));
        }
        if let Some(icon) = input.icon {
            stored.icon = s2_clean_optional(Some(icon));
        }
        if let Some(labels) = input.labels {
            stored.labels = labels
                .into_iter()
                .map(|label| label.trim().to_string())
                .filter(|label| !label.is_empty())
                .collect();
        }
        if let Some(definitions) = input.custom_field_definitions {
            stored.custom_field_definitions = definitions;
        }
        stored.updated_at = now();
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "project.updated",
            "project",
            Some(project_id.clone()),
            json!({ "name": updated.name }),
        );
        state.emit(
            "project.updated",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_project(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_capability(&state, viewer_role.as_deref(), "projects:delete")?;
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        // Boards first: their columns are removed by board id below.
        let board_ids: Vec<String> = state
            .boards
            .iter()
            .filter(|entry| entry.value().project_id == project_id)
            .map(|entry| entry.key().clone())
            .collect();
        // Dependencies, comments, watchers and attachments link to
        // tasks, not to the project, so collect the project's task
        // ids before the tasks themselves are removed.
        let task_ids: std::collections::HashSet<String> = state
            .tasks
            .iter()
            .filter(|entry| entry.value().project_id == project_id)
            .map(|entry| entry.key().clone())
            .collect();
        state.projects.remove(&project_id);
        state
            .members
            .retain(|_, member| member.project_id != project_id);
        state
            .boards
            .retain(|_, board| board.project_id != project_id);
        state
            .columns
            .retain(|_, column| !board_ids.contains(&column.board_id));
        state.tasks.retain(|_, task| task.project_id != project_id);
        state.dependencies.retain(|_, dependency| {
            !task_ids.contains(&dependency.task_id)
                && !task_ids.contains(&dependency.depends_on_task_id)
        });
        state
            .comments
            .retain(|_, comment| !task_ids.contains(&comment.task_id));
        state
            .watchers
            .retain(|_, watcher| !task_ids.contains(&watcher.task_id));
        state
            .attachments
            .retain(|_, attachment| !task_ids.contains(&attachment.task_id));
        state
            .milestones
            .retain(|_, milestone| milestone.project_id != project_id);
        state
            .sprints
            .retain(|_, sprint| sprint.project_id != project_id);
        state
            .phases
            .retain(|_, phase| phase.project_id != project_id);
        state
            .time_entries
            .retain(|_, entry| entry.project_id != project_id);
        state
            .automation_rules
            .retain(|_, rule| rule.project_id != project_id);
        state
            .cycles
            .retain(|_, cycle| cycle.project_id != project_id);
        state
            .program_increments
            .retain(|_, increment| increment.project_id != project_id);
        state
            .business_cases
            .retain(|_, business_case| business_case.project_id != project_id);
        state
            .activity
            .retain(|_, record| record.project_id != project_id);
        Ok(json!({ "deleted": true, "id": project_id }))
    })();
    json::respond(result, false)
}

pub(crate) async fn archive_project(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<Project> = (|| {
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
        let mut stored = state
            .projects
            .get_mut(&project_id)
            .ok_or_else(|| UnifiedError::not_found("Project not found"))?;
        stored.archived = true;
        stored.updated_at = now();
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "project.archived",
            "project",
            Some(project_id.clone()),
            json!({ "name": updated.name }),
        );
        state.emit(
            "project.archived",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        state.notify(
            &updated.owner_id,
            format!("Project archived: {}", updated.name),
            format!("Project {} was archived.", updated.name),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn restore_project(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<Project> = (|| {
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
        let mut stored = state
            .projects
            .get_mut(&project_id)
            .ok_or_else(|| UnifiedError::not_found("Project not found"))?;
        stored.archived = false;
        stored.updated_at = now();
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "project.restored",
            "project",
            Some(project_id.clone()),
            json!({ "name": updated.name }),
        );
        state.emit(
            "project.restored",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        state.notify(
            &updated.owner_id,
            format!("Project restored: {}", updated.name),
            format!("Project {} was restored.", updated.name),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn get_project_overview(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<S2ProjectOverview> = (|| {
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
        let current = now();
        let tasks: Vec<ProjectTask> = state
            .tasks
            .iter()
            .filter(|entry| entry.value().project_id == project_id)
            .map(|entry| entry.value().clone())
            .collect();
        let tasks_total = tasks.len() as u64;
        let tasks_done = tasks
            .iter()
            .filter(|task| s2_task_is_done(&task.status))
            .count() as u64;
        let tasks_open = tasks_total.saturating_sub(tasks_done);
        let tasks_overdue = tasks
            .iter()
            .filter(|task| {
                !s2_task_is_done(&task.status)
                    && task.status != "cancelled"
                    && task.due_date.is_some_and(|due| due < current)
            })
            .count() as u64;
        let members = state
            .members
            .iter()
            .filter(|entry| entry.value().project_id == project_id)
            .count() as u64;
        let milestones: Vec<ProjectMilestone> = state
            .milestones
            .iter()
            .filter(|entry| entry.value().project_id == project_id)
            .map(|entry| entry.value().clone())
            .collect();
        let milestones_total = milestones.len() as u64;
        let milestones_reached = milestones.iter().filter(|item| item.reached).count() as u64;
        let sprints_active = state
            .sprints
            .iter()
            .filter(|entry| {
                entry.value().project_id == project_id && entry.value().status == "active"
            })
            .count() as u64;
        let mut total_logged_hours = 0.0_f64;
        let mut billable_unbilled_hours = 0.0_f64;
        for entry in state.time_entries.iter() {
            let record = entry.value();
            if record.project_id != project_id {
                continue;
            }
            total_logged_hours += record.hours;
            if record.billable && !record.billed {
                billable_unbilled_hours += record.hours;
            }
        }
        let progress_percent = if tasks_total == 0 {
            0.0
        } else {
            tasks_done as f64 / tasks_total as f64 * 100.0
        };
        Ok(S2ProjectOverview {
            project_id: project_id.clone(),
            tasks_total,
            tasks_open,
            tasks_done,
            tasks_overdue,
            members,
            milestones_total,
            milestones_reached,
            sprints_active,
            total_logged_hours,
            billable_unbilled_hours,
            progress_percent,
        })
    })();
    json::respond(result, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn owned_project() -> Project {
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

    #[test]
    fn anonymous_list_viewer_sees_nothing() {
        let project = owned_project();
        assert!(!list_project_visible(&project, None, false, false));
        assert!(!list_project_visible(&project, None, true, false));
        assert!(list_project_visible(&project, None, false, true));
        assert!(list_project_visible(&project, Some("u1"), false, false));
        assert!(list_project_visible(&project, Some("u2"), true, false));
        assert!(!list_project_visible(&project, Some("u2"), false, false));
    }
}
