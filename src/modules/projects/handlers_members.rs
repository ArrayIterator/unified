// Projects module — project member and methodology handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Project members
// ---------------------------------------------------------------------------

/// Cleanup after a member is removed: unassign their open tasks in
/// the project (done and cancelled tasks keep their assignee as
/// history), and hand project ownership to another owner member when
/// the removed user was the project owner.
pub(crate) fn remove_member_cleanup(state: &ProjectsState, project: &Project, user_id: &str) {
    for mut task in state.tasks.iter_mut() {
        if task.project_id == project.id
            && task.assignee_id.as_deref() == Some(user_id)
            && !s2_task_is_done(&task.status)
            && task.status != "cancelled"
        {
            task.assignee_id = None;
            task.updated_at = now();
        }
    }
    if project.owner_id == user_id {
        let next_owner = state.members.iter().find_map(|entry| {
            let member = entry.value();
            if member.project_id == project.id
                && member.role == "owner"
                && member.user_id != user_id
            {
                Some(member.user_id.clone())
            } else {
                None
            }
        });
        if let Some(next_owner) = next_owner {
            if let Some(mut stored) = state.projects.get_mut(project.id.as_str()) {
                stored.owner_id = next_owner;
                stored.updated_at = now();
            }
        }
    }
}

pub(crate) async fn list_project_members(
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
    let sort = params.allowed_sort(&["joined_at"], "joined_at");
    let mut items: Vec<ProjectMember> = state
        .members
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|member| {
            member.project_id == project_id
                && params
                    .filters
                    .get("role")
                    .is_none_or(|role| member.role == *role)
        })
        .collect();
    match sort.as_str() {
        _ => items.sort_by(|a, b| a.joined_at.cmp(&b.joined_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn add_project_member(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<AddProjectMember>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectMember> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_capability(&state, viewer_role.as_deref(), "projects:manage_members")?;
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
        let user_id = input.user_id.trim().to_string();
        if user_id.is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        let role = match input.role {
            Some(value) if s2_valid_member_role(value.trim()) => value.trim().to_string(),
            Some(value) => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown member role: {}",
                    value.trim()
                )));
            }
            None => "member".to_string(),
        };
        let duplicate = state.members.iter().any(|entry| {
            entry.value().project_id == project_id && entry.value().user_id == user_id
        });
        if duplicate {
            return Err(UnifiedError::bad_request(
                "User is already a member of this project",
            ));
        }
        let member = ProjectMember {
            id: new_id(),
            project_id: project_id.clone(),
            user_id: user_id.clone(),
            role,
            joined_at: now(),
        };
        state.members.insert(member.id.clone(), member.clone());
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "member.added",
            "member",
            Some(member.id.clone()),
            json!({ "user_id": member.user_id, "role": member.role }),
        );
        state.notify(
            &user_id,
            format!("Added to project: {}", project.name),
            format!(
                "You were added to project {} as {}.",
                project.name, member.role
            ),
        );
        Ok(member)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn update_project_member(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProjectMember>,
) -> HttpResponse {
    let (project_id, user_id) = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectMember> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_capability(&state, viewer_role.as_deref(), "projects:manage_members")?;
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
        let role = match input.role {
            Some(value) if s2_valid_member_role(value.trim()) => value.trim().to_string(),
            Some(value) => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown member role: {}",
                    value.trim()
                )));
            }
            None => {
                return Err(UnifiedError::bad_request("role must not be empty"));
            }
        };
        // The route carries the member's user id, not the membership
        // record id: resolve the record by (project_id, user_id).
        let current = state
            .members
            .iter()
            .map(|entry| entry.value().clone())
            .find(|member| member.project_id == project_id && member.user_id == user_id)
            .ok_or_else(|| UnifiedError::not_found("Project member not found"))?;
        // Demoting the last owner would leave the project ownerless.
        if current.role == "owner" && role != "owner" {
            let owners = state
                .members
                .iter()
                .filter(|entry| {
                    entry.value().project_id == project_id && entry.value().role == "owner"
                })
                .count();
            if owners <= 1 {
                return Err(UnifiedError::bad_request(
                    "Cannot demote the last owner of the project",
                ));
            }
        }
        let member_id = current.id.clone();
        let mut stored = state
            .members
            .get_mut(&member_id)
            .ok_or_else(|| UnifiedError::not_found("Project member not found"))?;
        stored.role = role;
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "member.updated",
            "member",
            Some(member_id.clone()),
            json!({ "user_id": updated.user_id, "role": updated.role }),
        );
        state.notify(
            &updated.user_id,
            format!("Role changed in project: {}", project.name),
            format!(
                "Your role in project {} is now {}.",
                project.name, updated.role
            ),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn remove_project_member(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (project_id, user_id) = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_capability(&state, viewer_role.as_deref(), "projects:manage_members")?;
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        // The route carries the member's user id, not the membership
        // record id: resolve the record by (project_id, user_id).
        let member = state
            .members
            .iter()
            .map(|entry| entry.value().clone())
            .find(|member| member.project_id == project_id && member.user_id == user_id)
            .ok_or_else(|| UnifiedError::not_found("Project member not found"))?;
        // Removing the last owner would leave the project ownerless.
        if member.role == "owner" {
            let owners = state
                .members
                .iter()
                .filter(|entry| {
                    entry.value().project_id == project_id && entry.value().role == "owner"
                })
                .count();
            if owners <= 1 {
                return Err(UnifiedError::bad_request(
                    "Cannot remove the last owner of the project",
                ));
            }
        }
        state.members.remove(&member.id);
        remove_member_cleanup(&state, &project, &user_id);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "member.removed",
            "member",
            Some(member.id.clone()),
            json!({ "user_id": member.user_id }),
        );
        state.notify(
            &member.user_id,
            format!("Removed from project: {}", project.name),
            format!("You were removed from project {}.", project.name),
        );
        Ok(json!({ "deleted": true, "id": member.id }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Methodologies
// ---------------------------------------------------------------------------

pub(crate) async fn list_methodologies(state: web::Data<Arc<ProjectsState>>) -> HttpResponse {
    let _state = state;
    json::success_response(&METHODOLOGIES, 200)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn test_project(owner_id: &str) -> Project {
        Project {
            id: "p1".to_string(),
            name: "Test".to_string(),
            description: String::new(),
            methodology: "scrum".to_string(),
            status: "active".to_string(),
            visibility: "private".to_string(),
            owner_id: owner_id.to_string(),
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

    fn member(id: &str, user_id: &str, role: &str) -> ProjectMember {
        ProjectMember {
            id: id.to_string(),
            project_id: "p1".to_string(),
            user_id: user_id.to_string(),
            role: role.to_string(),
            joined_at: Utc::now(),
        }
    }

    fn task(id: &str, assignee: Option<&str>, status: &str) -> ProjectTask {
        ProjectTask {
            id: id.to_string(),
            project_id: "p1".to_string(),
            board_id: None,
            parent_id: None,
            epic_id: None,
            title: "Task".to_string(),
            description: String::new(),
            task_type: "task".to_string(),
            status: status.to_string(),
            priority: "normal".to_string(),
            assignee_id: assignee.map(str::to_string),
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
    fn removal_unassigns_open_tasks_and_hands_over_ownership() {
        let state = ProjectsState::new();
        let project = test_project("u1");
        state.projects.insert(project.id.clone(), project.clone());
        state.members.insert("m1".to_string(), member("m1", "u1", "owner"));
        state.members.insert("m2".to_string(), member("m2", "u2", "owner"));
        state.members.insert("m3".to_string(), member("m3", "u3", "member"));
        state.tasks.insert("t1".to_string(), task("t1", Some("u3"), "in_progress"));
        state.tasks.insert("t2".to_string(), task("t2", Some("u3"), "done"));
        state.tasks.insert("t3".to_string(), task("t3", Some("u3"), "cancelled"));
        remove_member_cleanup(&state, &project, "u3");
        assert_eq!(state.get_task("t1").unwrap().assignee_id, None);
        assert_eq!(state.get_task("t2").unwrap().assignee_id.as_deref(), Some("u3"));
        assert_eq!(state.get_task("t3").unwrap().assignee_id.as_deref(), Some("u3"));
        assert_eq!(state.get_project("p1").unwrap().owner_id, "u1");
        // Removing the owner hands ownership to the other owner.
        state.members.remove("m1");
        remove_member_cleanup(&state, &project, "u1");
        assert_eq!(state.get_project("p1").unwrap().owner_id, "u2");
    }
}
