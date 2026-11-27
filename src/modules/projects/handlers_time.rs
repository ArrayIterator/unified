// Projects module - time tracking handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::sync::Arc;

// Time tracking

pub(crate) async fn list_time_entries(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result = (|| {
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
        require_capability(&state, viewer_role.as_deref(), "projects:view")?;
        let sort = params.allowed_sort(&["started_at", "hours", "created_at"], "started_at");
        let mut items: Vec<ProjectTimeEntry> = state
            .time_entries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                entry.project_id == project.id
                    && params
                        .filters
                        .get("user_id")
                        .is_none_or(|value| entry.user_id == *value)
                    && params
                        .filters
                        .get("task_id")
                        .is_none_or(|value| entry.task_id == *value)
                    && params
                        .filters
                        .get("billable")
                        .is_none_or(|value| s5_parse_flag(Some(value)) == Some(entry.billable))
                    && params
                        .filters
                        .get("billed")
                        .is_none_or(|value| s5_parse_flag(Some(value)) == Some(entry.billed))
                    && matches_search(&format!("{} {}", entry.description, entry.user_id), &params)
            })
            .collect();
        match sort.as_str() {
            "hours" => items.sort_by(|a, b| {
                a.hours
                    .partial_cmp(&b.hours)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            "created_at" => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
            _ => items.sort_by(|a, b| a.started_at.cmp(&b.started_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

pub(crate) async fn create_time_entry(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateTimeEntry>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectTimeEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        require_capability(&state, viewer_role.as_deref(), "projects:time_log")?;
        let input = body.into_inner();
        let task = state.get_task(&input.task_id)?;
        if task.project_id != project.id {
            return Err(UnifiedError::bad_request(
                "Task does not belong to this project",
            ));
        }
        if !s5_valid_hours(input.hours) {
            return Err(UnifiedError::bad_request(
                "Hours must be greater than 0 and at most 24",
            ));
        }
        let started_at = input.started_at.unwrap_or_else(now);
        if let Some(ended_at) = input.ended_at {
            if ended_at <= started_at {
                return Err(UnifiedError::bad_request(
                    "ended_at must be after started_at",
                ));
            }
        }
        // The time entry belongs to the header caller: a body
        // `user_id` naming someone else is forgery unless the
        // caller holds `projects:manage` (admin/on-behalf flow).
        let user_id = resolve_body_actor(
            &state,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &input.user_id,
            "user_id",
        )?;
        let entry = ProjectTimeEntry {
            id: new_id(),
            task_id: task.id.clone(),
            project_id: project.id.clone(),
            user_id: user_id.clone(),
            description: input
                .description
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_default(),
            hours: s5_round2(input.hours),
            billable: input.billable.unwrap_or(false),
            billed: false,
            invoice_id: None,
            started_at,
            ended_at: input.ended_at,
            timer_running: false,
            created_at: now(),
            updated_at: now(),
        };
        state.time_entries.insert(entry.id.clone(), entry.clone());
        state.log_activity(
            &project.id,
            viewer_id.as_deref().unwrap_or(&user_id),
            "time_entry.created",
            "time_entry",
            Some(entry.id.clone()),
            json!({
                "task_id": entry.task_id,
                "hours": entry.hours,
                "billable": entry.billable,
            }),
        );
        s5_emit_time_logged(&state, &entry);
        Ok(entry)
    })();
    s5_respond_created(result)
}

pub(crate) async fn get_time_entry(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectTimeEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let (project_id, entry_id) = path.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        require_capability(&state, viewer_role.as_deref(), "projects:view")?;
        let entry = state
            .time_entries
            .get(&entry_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Time entry not found"))?;
        if entry.project_id != project.id {
            return Err(UnifiedError::not_found("Time entry not found"));
        }
        Ok(entry)
    })();
    json::respond(result, false)
}

pub(crate) async fn update_time_entry(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateTimeEntry>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectTimeEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let (project_id, entry_id) = path.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        require_capability(&state, viewer_role.as_deref(), "projects:time_log")?;
        let input = body.into_inner();
        if let Some(hours) = input.hours {
            if !s5_valid_hours(hours) {
                return Err(UnifiedError::bad_request(
                    "Hours must be greater than 0 and at most 24",
                ));
            }
        }
        let mut entry = state
            .time_entries
            .get_mut(&entry_id)
            .ok_or_else(|| UnifiedError::not_found("Time entry not found"))?;
        if entry.project_id != project.id {
            return Err(UnifiedError::not_found("Time entry not found"));
        }
        if entry.billed {
            return Err(UnifiedError::bad_request(
                "A billed time entry cannot be edited",
            ));
        }
        if let Some(description) = input.description {
            entry.description = description.trim().to_string();
        }
        if let Some(hours) = input.hours {
            entry.hours = s5_round2(hours);
        }
        if let Some(billable) = input.billable {
            entry.billable = billable;
        }
        if let Some(billed) = input.billed {
            entry.billed = billed;
        }
        if let Some(invoice_id) = input.invoice_id {
            entry.invoice_id = Some(invoice_id.trim().to_string());
        }
        entry.updated_at = now();
        let updated = entry.clone();
        let actor = viewer_id.clone().unwrap_or_else(|| updated.user_id.clone());
        drop(entry);
        state.log_activity(
            &project.id,
            &actor,
            "time_entry.updated",
            "time_entry",
            Some(updated.id.clone()),
            json!({
                "task_id": updated.task_id,
                "hours": updated.hours,
            }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_time_entry(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let (project_id, entry_id) = path.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        require_capability(&state, viewer_role.as_deref(), "projects:time_log")?;
        let entry = state
            .time_entries
            .get(&entry_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Time entry not found"))?;
        if entry.project_id != project.id {
            return Err(UnifiedError::not_found("Time entry not found"));
        }
        if entry.billed {
            return Err(UnifiedError::bad_request(
                "A billed time entry cannot be deleted",
            ));
        }
        state.time_entries.remove(&entry_id);
        state.log_activity(
            &project.id,
            viewer_id.as_deref().unwrap_or(&entry.user_id),
            "time_entry.deleted",
            "time_entry",
            Some(entry_id.clone()),
            json!({
                "task_id": entry.task_id,
                "hours": entry.hours,
            }),
        );
        Ok(json!({ "deleted": true, "id": entry_id }))
    })();
    json::respond(result, false)
}

pub(crate) async fn start_time_timer(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<StartTimer>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectTimeEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let project = state.get_project(path.as_str())?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        require_capability(&state, viewer_role.as_deref(), "projects:time_log")?;
        let input = body.into_inner();
        let task = state.get_task(&input.task_id)?;
        if task.project_id != project.id {
            return Err(UnifiedError::bad_request(
                "Task does not belong to this project",
            ));
        }
        // The timer belongs to the header caller: a body `user_id`
        // naming someone else is forgery unless the caller holds
        // `projects:manage` (admin/on-behalf flow).
        let user_id = resolve_body_actor(
            &state,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &input.user_id,
            "user_id",
        )?;
        let already_running = state.time_entries.iter().any(|entry| {
            let entry = entry.value();
            entry.project_id == project.id && entry.user_id == user_id && entry.timer_running
        });
        if already_running {
            return Err(UnifiedError::bad_request(
                "User already has a running timer in this project",
            ));
        }
        let entry = ProjectTimeEntry {
            id: new_id(),
            task_id: task.id.clone(),
            project_id: project.id.clone(),
            user_id: user_id.clone(),
            description: input
                .description
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_default(),
            hours: 0.0,
            billable: false,
            billed: false,
            invoice_id: None,
            started_at: now(),
            ended_at: None,
            timer_running: true,
            created_at: now(),
            updated_at: now(),
        };
        state.time_entries.insert(entry.id.clone(), entry.clone());
        state.log_activity(
            &project.id,
            viewer_id.as_deref().unwrap_or(&user_id),
            "timer.started",
            "time_entry",
            Some(entry.id.clone()),
            json!({ "task_id": entry.task_id }),
        );
        Ok(entry)
    })();
    s5_respond_created(result)
}

pub(crate) async fn stop_time_timer(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectTimeEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let (project_id, entry_id) = path.into_inner();
        let project = state.get_project(&project_id)?;
        require_project_access(
            &state,
            &project,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            &_scope,
        )?;
        s4_require_not_archived(&project)?;
        require_capability(&state, viewer_role.as_deref(), "projects:time_log")?;
        let mut entry = state
            .time_entries
            .get_mut(&entry_id)
            .ok_or_else(|| UnifiedError::not_found("Time entry not found"))?;
        if entry.project_id != project.id {
            return Err(UnifiedError::not_found("Time entry not found"));
        }
        if !entry.timer_running {
            return Err(UnifiedError::bad_request("Timer is not running"));
        }
        let stopped_at = now();
        let elapsed_ms = stopped_at
            .signed_duration_since(entry.started_at)
            .num_milliseconds();
        // No minimum billing floor: a timer stopped immediately
        // records 0 hours instead of billing 0.01h (36 seconds).
        let hours = s5_timer_hours(elapsed_ms);
        entry.timer_running = false;
        entry.ended_at = Some(stopped_at);
        entry.hours = hours;
        entry.updated_at = stopped_at;
        let stopped = entry.clone();
        let actor = viewer_id.clone().unwrap_or_else(|| stopped.user_id.clone());
        drop(entry);
        state.log_activity(
            &project.id,
            &actor,
            "timer.stopped",
            "time_entry",
            Some(stopped.id.clone()),
            json!({
                "task_id": stopped.task_id,
                "hours": stopped.hours,
            }),
        );
        s5_emit_time_logged(&state, &stopped);
        Ok(stopped)
    })();
    json::respond(result, false)
}

pub(crate) async fn list_billable_time(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<Value> = (|| {
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
        require_capability(&state, viewer_role.as_deref(), "projects:reports")?;
        let sort = params.allowed_sort(&["started_at", "hours", "created_at"], "started_at");
        let mut items: Vec<ProjectTimeEntry> = state
            .time_entries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                entry.project_id == project.id
                    && entry.billable
                    && !entry.billed
                    && params
                        .filters
                        .get("user_id")
                        .is_none_or(|value| entry.user_id == *value)
                    && params
                        .filters
                        .get("task_id")
                        .is_none_or(|value| entry.task_id == *value)
            })
            .collect();
        let total_hours = s5_round2(items.iter().map(|entry| entry.hours).sum::<f64>());
        // Every entry in this list is billable, so the billable total
        // is the same sum; both keys are returned because that is the
        // shape the billing module consumes.
        let total_billable_hours = total_hours;
        match sort.as_str() {
            "hours" => items.sort_by(|a, b| {
                a.hours
                    .partial_cmp(&b.hours)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            "created_at" => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
            _ => items.sort_by(|a, b| a.started_at.cmp(&b.started_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        let page = paginate(items, &params);
        Ok(json!({
            "entries": page,
            "total_hours": total_hours,
            "total_billable_hours": total_billable_hours,
        }))
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
    fn stopped_timer_has_no_minimum_billing_floor() {
        assert_eq!(s5_timer_hours(0), 0.0);
        assert_eq!(s5_timer_hours(500), 0.0);
        assert_eq!(s5_timer_hours(3_600_000), 1.0);
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

    fn seed_project_with_task(state: &ProjectsState) {
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
        use actix_web::test::TestRequest;
        verified_request("user-1", role)
    }

    fn entry_body(user_id: &str) -> web::Json<CreateTimeEntry> {
        web::Json(CreateTimeEntry {
            task_id: "t1".to_string(),
            user_id: user_id.to_string(),
            description: None,
            hours: 2.0,
            billable: None,
            started_at: None,
            ended_at: None,
        })
    }

    fn timer_body(user_id: &str) -> web::Json<StartTimer> {
        web::Json(StartTimer {
            task_id: "t1".to_string(),
            user_id: user_id.to_string(),
            description: None,
        })
    }

    #[tokio::test]
    async fn create_time_entry_rejects_forged_user() {
        use actix_web::http::StatusCode;
        let state = allowing_state(&["projects:time_log"]);
        seed_project_with_task(&state);
        let resp = create_time_entry(
            caller_req("member"),
            web::Data::new(state.clone()),
            web::Path::from("p1".to_string()),
            web::Query(PaginationParams::default()),
            entry_body("someone-else"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(state.time_entries.is_empty());
    }

    #[tokio::test]
    async fn create_time_entry_allows_manage_on_behalf() {
        use actix_web::http::StatusCode;
        let state = allowing_state(&["projects:time_log", "projects:manage"]);
        seed_project_with_task(&state);
        let resp = create_time_entry(
            caller_req("admin"),
            web::Data::new(state.clone()),
            web::Path::from("p1".to_string()),
            web::Query(PaginationParams::default()),
            entry_body("someone-else"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let stored = state
            .time_entries
            .iter()
            .next()
            .expect("time entry stored")
            .value()
            .clone();
        assert_eq!(stored.user_id, "someone-else");
    }

    #[tokio::test]
    async fn start_time_timer_rejects_forged_user() {
        use actix_web::http::StatusCode;
        let state = allowing_state(&["projects:time_log"]);
        seed_project_with_task(&state);
        let resp = start_time_timer(
            caller_req("member"),
            web::Data::new(state.clone()),
            web::Path::from("p1".to_string()),
            web::Query(PaginationParams::default()),
            timer_body("someone-else"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(state.time_entries.is_empty());
    }
}
