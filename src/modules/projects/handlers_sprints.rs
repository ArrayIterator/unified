// Projects module — sprint handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Sprints
// ---------------------------------------------------------------------------

/// Status a newly created sprint starts with: always `planned`, no
/// matter what the client supplied, so a sprint can never be born
/// active (bypassing the one-active-sprint rule and the start event)
/// or completed (with no velocity computed and no tasks returned to
/// the backlog).
fn sprint_initial_status(_supplied: Option<&str>) -> String {
    "planned".to_string()
}

pub(crate) async fn list_project_sprints(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<PageResult<ProjectSprint>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        s4_resolve_project(
            &state,
            path.as_str(),
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:view",
            &_scope,
        )?;
        let sort = params.allowed_sort(
            &["created_at", "name", "start_date", "end_date", "updated_at"],
            "created_at",
        );
        let mut items: Vec<ProjectSprint> = state
            .sprints
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sprint| {
                sprint.project_id == path.as_str()
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| sprint.status == *status)
                    && matches_search(
                        &format!(
                            "{} {}",
                            sprint.name,
                            sprint.goal.as_str()
                        ),
                        &params,
                    )
            })
            .collect();
        match sort.as_str() {
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
            "start_date" => items.sort_by(|a, b| a.start_date.cmp(&b.start_date)),
            "end_date" => items.sort_by(|a, b| a.end_date.cmp(&b.end_date)),
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

pub(crate) async fn create_project_sprint(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateProjectSprint>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectSprint> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            path.as_str(),
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
            &_scope,
        )?;
        let input = body.into_inner();
        let name = s4_require_text(&input.name, "Sprint name")?;
        let profile = methodology_profile(&project.methodology);
        if !profile.is_some_and(|profile| profile.uses_sprints) {
            return Err(UnifiedError::bad_request(
                "methodology does not use sprints",
            ));
        }
        let status = sprint_initial_status(input.status.as_deref());
        s4_validate_date_range(input.start_date, input.end_date, "Sprint")?;
        let current = now();
        let sprint = ProjectSprint {
            id: new_id(),
            project_id: project.id.clone(),
            name,
            goal: s4_clean_text(input.goal).unwrap_or_default(),
            status,
            start_date: input.start_date,
            end_date: input.end_date,
            velocity_planned: input.velocity_planned,
            velocity_actual: None,
            standup_notes: Value::Array(Vec::new()),
            review_notes: None,
            retrospective_notes: None,
            created_at: current,
            updated_at: current,
        };
        state.sprints.insert(sprint.id.clone(), sprint.clone());
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "sprint.created",
            "sprint",
            Some(sprint.id.clone()),
            json!({ "name": sprint.name, "status": sprint.status }),
        );
        Ok(sprint)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn get_project_sprint(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, sprint_id) = path.into_inner();
    let result: UnifiedResult<ProjectSprint> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        s4_resolve_project(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:view",
            &_scope,
        )?;
        s4_get_sprint(&state, &project_id, &sprint_id)
    })();
    json::respond(result, false)
}

pub(crate) async fn update_project_sprint(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProjectSprint>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, sprint_id) = path.into_inner();
    let result: UnifiedResult<ProjectSprint> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
            &_scope,
        )?;
        let input = body.into_inner();
        if let Some(status) = &input.status {
            if !s4_valid_sprint_status(status.trim()) {
                return Err(UnifiedError::bad_request(
                    "Sprint status must be one of: planned, active, completed",
                ));
            }
            // Status may only change via the start/close handlers,
            // which enforce the one-active-sprint rule, the start
            // event, velocity computation, and the backlog return.
            s4_reject_direct_status_change(Some(status.trim()), "Sprint status")?;
        }
        let mut sprint = state
            .sprints
            .get_mut(sprint_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
        if sprint.project_id != project_id {
            return Err(UnifiedError::not_found("Sprint not found"));
        }
        let start_date = input.start_date.or(sprint.start_date);
        let end_date = input.end_date.or(sprint.end_date);
        s4_validate_date_range(start_date, end_date, "Sprint")?;
        if let Some(name) = input.name {
            sprint.name = s4_require_text(&name, "Sprint name")?;
        }
        if let Some(goal) = input.goal {
            sprint.goal = s4_clean_text(Some(goal)).unwrap_or_default();
        }
        sprint.start_date = start_date;
        sprint.end_date = end_date;
        if let Some(velocity_planned) = input.velocity_planned {
            sprint.velocity_planned = Some(velocity_planned);
        }
        sprint.updated_at = now();
        let updated = sprint.clone();
        drop(sprint);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "sprint.updated",
            "sprint",
            Some(updated.id.clone()),
            json!({ "name": updated.name, "status": updated.status }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_project_sprint(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, sprint_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
            &_scope,
        )?;
        let sprint = s4_get_sprint(&state, &project_id, &sprint_id)?;
        if sprint.status == "active" {
            return Err(UnifiedError::bad_request("Cannot delete an active sprint"));
        }
        // Return the sprint's tasks to the product backlog first so
        // no task keeps pointing at a sprint that no longer exists.
        for mut task in state.tasks.iter_mut() {
            if task.sprint_id.as_deref() == Some(sprint_id.as_str()) {
                task.sprint_id = None;
                task.updated_at = now();
            }
        }
        state
            .sprints
            .remove(sprint_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "sprint.deleted",
            "sprint",
            Some(sprint.id.clone()),
            json!({ "name": sprint.name }),
        );
        Ok(json!({ "deleted": true, "id": sprint_id }))
    })();
    json::respond(result, false)
}

pub(crate) async fn start_project_sprint(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, sprint_id) = path.into_inner();
    let result: UnifiedResult<ProjectSprint> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
            &_scope,
        )?;
        let sprint = s4_get_sprint(&state, &project_id, &sprint_id)?;
        if sprint.status != "planned" {
            return Err(UnifiedError::bad_request(
                "Only a planned sprint can be started",
            ));
        }
        if s4_active_sprint_id(&state, &project.id, Some(&sprint_id)).is_some() {
            return Err(UnifiedError::bad_request(
                "Project already has an active sprint",
            ));
        }
        let current = now();
        let mut sprint = state
            .sprints
            .get_mut(sprint_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
        sprint.status = "active".to_string();
        if sprint.start_date.is_none() {
            sprint.start_date = Some(current);
        }
        sprint.updated_at = current;
        let started = sprint.clone();
        drop(sprint);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "sprint.started",
            "sprint",
            Some(started.id.clone()),
            json!({ "name": started.name }),
        );
        state.emit(
            "project.sprint.started",
            json!({
                "project_id": project.id,
                "sprint_id": started.id,
                "name": started.name,
            }),
        );
        state.notify(
            &project.owner_id,
            format!("Sprint started: {}", started.name),
            format!("Sprint {} is now active.", started.name),
        );
        Ok(started)
    })();
    json::respond(result, false)
}

pub(crate) async fn close_project_sprint(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, sprint_id) = path.into_inner();
    let result: UnifiedResult<ProjectSprint> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
            &_scope,
        )?;
        let sprint = s4_get_sprint(&state, &project_id, &sprint_id)?;
        if sprint.status != "active" {
            return Err(UnifiedError::bad_request(
                "Only an active sprint can be closed",
            ));
        }
        // Delivered velocity: estimated points of the sprint's tasks
        // that reached done.
        let velocity_actual: f64 = state
            .tasks
            .iter()
            .filter(|entry| {
                let task = entry.value();
                task.sprint_id.as_deref() == Some(sprint_id.as_str()) && task.status == "done"
            })
            .map(|entry| entry.value().estimate_points.unwrap_or(0.0))
            .sum();
        let current = now();
        let mut sprint = state
            .sprints
            .get_mut(sprint_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
        sprint.status = "completed".to_string();
        sprint.velocity_actual = Some(velocity_actual);
        if sprint.end_date.is_none() {
            sprint.end_date = Some(current);
        }
        sprint.updated_at = current;
        let closed = sprint.clone();
        drop(sprint);
        // Unfinished work goes back to the product backlog.
        for mut task in state.tasks.iter_mut() {
            if task.sprint_id.as_deref() == Some(sprint_id.as_str()) && task.status != "done" {
                task.sprint_id = None;
                task.updated_at = now();
            }
        }
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "sprint.closed",
            "sprint",
            Some(closed.id.clone()),
            json!({ "name": closed.name, "velocity_actual": velocity_actual }),
        );
        state.emit(
            "project.sprint.closed",
            json!({
                "project_id": project.id,
                "sprint_id": closed.id,
                "name": closed.name,
                "velocity_actual": velocity_actual,
            }),
        );
        state.notify(
            &project.owner_id,
            format!("Sprint closed: {}", closed.name),
            format!(
                "Sprint {} is complete with a delivered velocity of {}.",
                closed.name, velocity_actual
            ),
        );
        Ok(closed)
    })();
    json::respond(result, false)
}

pub(crate) async fn add_sprint_standup(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<AddStandupNote>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, sprint_id) = path.into_inner();
    let result: UnifiedResult<ProjectSprint> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
            &_scope,
        )?;
        let input = body.into_inner();
        // The standup author is the header caller: a body
        // `author_id` naming someone else is forgery unless the
        // caller holds `projects:manage` (admin/on-behalf flow).
        let author_id = resolve_body_actor(
            &state,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &input.author_id,
            "author_id",
        )?;
        let note = s4_require_text(&input.note, "Standup note")?;
        let blockers = s4_clean_text(input.blockers);
        let mut sprint = state
            .sprints
            .get_mut(sprint_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
        if sprint.project_id != project_id {
            return Err(UnifiedError::not_found("Sprint not found"));
        }
        let mut notes = sprint.standup_notes.as_array().cloned().unwrap_or_default();
        notes.push(json!({
            "author_id": author_id,
            "note": note,
            "blockers": blockers,
            "at": now(),
        }));
        sprint.standup_notes = Value::Array(notes);
        sprint.updated_at = now();
        let updated = sprint.clone();
        drop(sprint);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "sprint.standup_added",
            "sprint",
            Some(updated.id.clone()),
            json!({ "name": updated.name, "author_id": author_id }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn save_sprint_review(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<SaveSprintNotes>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, sprint_id) = path.into_inner();
    let result: UnifiedResult<ProjectSprint> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
            &_scope,
        )?;
        let input = body.into_inner();
        let notes = s4_require_text(&input.notes, "Review notes")?;
        let mut sprint = state
            .sprints
            .get_mut(sprint_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
        if sprint.project_id != project_id {
            return Err(UnifiedError::not_found("Sprint not found"));
        }
        sprint.review_notes = Some(notes);
        sprint.updated_at = now();
        let updated = sprint.clone();
        drop(sprint);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "sprint.review_saved",
            "sprint",
            Some(updated.id.clone()),
            json!({ "name": updated.name }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn save_sprint_retrospective(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<SaveSprintNotes>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, sprint_id) = path.into_inner();
    let result: UnifiedResult<ProjectSprint> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
            &_scope,
        )?;
        let input = body.into_inner();
        let notes = s4_require_text(&input.notes, "Retrospective notes")?;
        let mut sprint = state
            .sprints
            .get_mut(sprint_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
        if sprint.project_id != project_id {
            return Err(UnifiedError::not_found("Sprint not found"));
        }
        sprint.retrospective_notes = Some(notes);
        sprint.updated_at = now();
        let updated = sprint.clone();
        drop(sprint);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "sprint.retrospective_saved",
            "sprint",
            Some(updated.id.clone()),
            json!({ "name": updated.name }),
        );
        Ok(updated)
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

    #[test]
    fn new_sprint_is_always_planned() {
        assert_eq!(sprint_initial_status(None), "planned");
        assert_eq!(sprint_initial_status(Some("active")), "planned");
        assert_eq!(sprint_initial_status(Some("completed")), "planned");
        assert_eq!(sprint_initial_status(Some("planned")), "planned");
    }

    #[test]
    fn update_rejects_direct_status_change() {
        assert!(s4_reject_direct_status_change(None, "Sprint status").is_ok());
        assert!(s4_reject_direct_status_change(Some("active"), "Sprint status").is_err());
        assert!(s4_reject_direct_status_change(Some("completed"), "Sprint status").is_err());
        assert!(s4_reject_direct_status_change(Some("planned"), "Sprint status").is_err());
    }

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

    fn seed_project_with_sprint(state: &ProjectsState) {
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
        state.sprints.insert(
            "s1".to_string(),
            ProjectSprint {
                id: "s1".to_string(),
                project_id: "p1".to_string(),
                name: "Sprint 1".to_string(),
                goal: None,
                status: "planned".to_string(),
                start_date: None,
                end_date: None,
                velocity_planned: None,
                velocity_actual: None,
                standup_notes: Value::Array(Vec::new()),
                review_notes: None,
                retrospective_notes: None,
                created_at: now(),
                updated_at: now(),
            },
        );
    }

    fn caller_req(role: &str) -> HttpRequest {
        use actix_web::test::TestRequest;
        verified_request("user-1", role)
    }

    fn standup_body(author_id: &str) -> web::Json<AddStandupNote> {
        web::Json(AddStandupNote {
            author_id: author_id.to_string(),
            note: "yesterday: x".to_string(),
            blockers: None,
        })
    }

    fn last_standup_author(state: &ProjectsState) -> Option<String> {
        state
            .sprints
            .get("s1")
            .map(|entry| entry.value().standup_notes.clone())
            .and_then(|notes| notes.as_array().cloned())
            .and_then(|notes| notes.last().cloned())
            .and_then(|note| {
                note.get("author_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
    }

    #[tokio::test]
    async fn add_sprint_standup_rejects_forged_author() {
        use actix_web::http::StatusCode;
        let state = allowing_state(&["projects:edit"]);
        seed_project_with_sprint(&state);
        let resp = add_sprint_standup(
            caller_req("member"),
            web::Data::new(state.clone()),
            web::Path::from(("p1".to_string(), "s1".to_string())),
            web::Query(PaginationParams::default()),
            standup_body("someone-else"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(last_standup_author(&state), None);
    }

    #[tokio::test]
    async fn add_sprint_standup_records_the_caller() {
        use actix_web::http::StatusCode;
        let state = allowing_state(&["projects:edit"]);
        seed_project_with_sprint(&state);
        let resp = add_sprint_standup(
            caller_req("member"),
            web::Data::new(state.clone()),
            web::Path::from(("p1".to_string(), "s1".to_string())),
            web::Query(PaginationParams::default()),
            standup_body(&test_user_uuid("user-1").to_string()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            last_standup_author(&state).as_deref(),
            Some(test_user_uuid("user-1").to_string().as_str())
        );
    }

    #[tokio::test]
    async fn add_sprint_standup_allows_manage_on_behalf() {
        use actix_web::http::StatusCode;
        let state = allowing_state(&["projects:edit", "projects:manage"]);
        seed_project_with_sprint(&state);
        let resp = add_sprint_standup(
            caller_req("admin"),
            web::Data::new(state.clone()),
            web::Path::from(("p1".to_string(), "s1".to_string())),
            web::Query(PaginationParams::default()),
            standup_body("someone-else"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            last_standup_author(&state).as_deref(),
            Some("someone-else")
        );
    }
}
