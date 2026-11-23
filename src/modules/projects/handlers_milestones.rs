// Projects module — project milestone handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Project milestones
// ---------------------------------------------------------------------------

pub(crate) async fn list_project_milestones(
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
    let sort = params.allowed_sort(&["created_at", "due_date", "name"], "created_at");
    let mut items: Vec<ProjectMilestone> = state
        .milestones
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|milestone| {
            milestone.project_id == project_id
                && params
                    .filters
                    .get("reached")
                    .is_none_or(|value| milestone.reached == (value == "true"))
        })
        .collect();
    match sort.as_str() {
        "due_date" => items.sort_by(|a, b| a.due_date.cmp(&b.due_date)),
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_project_milestone(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateProjectMilestone>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectMilestone> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
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
            return Err(UnifiedError::bad_request(
                "Milestone name must not be empty",
            ));
        }
        let timestamp = now();
        let milestone = ProjectMilestone {
            id: new_id(),
            project_id: project_id.clone(),
            name,
            description: s2_clean_optional(input.description).unwrap_or_default(),
            due_date: input.due_date,
            reached: false,
            reached_at: None,
            created_at: timestamp,
            updated_at: timestamp,
        };
        state
            .milestones
            .insert(milestone.id.clone(), milestone.clone());
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "milestone.created",
            "milestone",
            Some(milestone.id.clone()),
            json!({ "name": milestone.name }),
        );
        Ok(milestone)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn get_project_milestone(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (project_id, milestone_id) = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectMilestone> = (|| {
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
        s2_get_milestone(&state, &project_id, &milestone_id)
    })();
    json::respond(result, false)
}

pub(crate) async fn update_project_milestone(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProjectMilestone>,
) -> HttpResponse {
    let (project_id, milestone_id) = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectMilestone> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
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
        s2_get_milestone(&state, &project_id, &milestone_id)?;
        let mut stored = state
            .milestones
            .get_mut(&milestone_id)
            .ok_or_else(|| UnifiedError::not_found("Project milestone not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request(
                    "Milestone name must not be empty",
                ));
            }
            stored.name = name.trim().to_string();
        }
        if let Some(description) = input.description {
            stored.description = description.trim().to_string();
        }
        if input.due_date.is_some() {
            stored.due_date = input.due_date;
        }
        if let Some(reached) = input.reached {
            stored.reached = reached;
            stored.reached_at = if reached {
                stored.reached_at.or_else(|| Some(now()))
            } else {
                None
            };
        }
        stored.updated_at = now();
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "milestone.updated",
            "milestone",
            Some(milestone_id.clone()),
            json!({ "name": updated.name }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_project_milestone(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (project_id, milestone_id) = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
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
        let milestone = s2_get_milestone(&state, &project_id, &milestone_id)?;
        state.milestones.remove(&milestone_id);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "milestone.deleted",
            "milestone",
            Some(milestone_id.clone()),
            json!({ "name": milestone.name }),
        );
        Ok(json!({ "deleted": true, "id": milestone_id }))
    })();
    json::respond(result, false)
}

pub(crate) async fn reach_project_milestone(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (project_id, milestone_id) = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectMilestone> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
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
        s2_get_milestone(&state, &project_id, &milestone_id)?;
        let mut stored = state
            .milestones
            .get_mut(&milestone_id)
            .ok_or_else(|| UnifiedError::not_found("Project milestone not found"))?;
        stored.reached = true;
        stored.reached_at = Some(now());
        stored.updated_at = now();
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "milestone.reached",
            "milestone",
            Some(milestone_id.clone()),
            json!({ "name": updated.name }),
        );
        state.emit(
            "project.milestone.reached",
            json!({
                "project_id": project_id,
                "milestone_id": milestone_id,
                "milestone": serde_json::to_value(&updated).unwrap_or(Value::Null),
            }),
        );
        state.notify(
            &project.owner_id,
            format!("Milestone reached: {}", updated.name),
            format!(
                "Milestone {} was reached in project {}.",
                updated.name, project.name
            ),
        );
        let member_ids: Vec<String> = state
            .members
            .iter()
            .filter(|entry| {
                entry.value().project_id == project_id && entry.value().user_id != project.owner_id
            })
            .map(|entry| entry.value().user_id.clone())
            .collect();
        for user_id in member_ids {
            state.notify(
                &user_id,
                format!("Milestone reached: {}", updated.name),
                format!(
                    "Milestone {} was reached in project {}.",
                    updated.name, project.name
                ),
            );
        }
        Ok(updated)
    })();
    json::respond(result, false)
}
