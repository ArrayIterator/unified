// Projects module — program increment and business case handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// SAFe program increments
// ---------------------------------------------------------------------------

/// Planning is only valid for a planned increment: planning an
/// already-active (or completed) increment must not silently demote
/// it back to planned.
fn plan_requires_planned_status(pi: &ProjectProgramIncrement) -> UnifiedResult<()> {
    if pi.status != "planned" {
        return Err(UnifiedError::conflict(
            "Only a planned program increment can be planned",
        ));
    }
    Ok(())
}

pub(crate) async fn list_program_increments(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<PageResult<ProjectProgramIncrement>> = (|| {
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
        let mut items: Vec<ProjectProgramIncrement> = state
            .program_increments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|pi| {
                pi.project_id == path.as_str()
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| pi.status == *status)
                    && matches_search(&pi.name, &params)
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

pub(crate) async fn create_program_increment(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateProgramIncrement>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectProgramIncrement> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            path.as_str(),
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
            &_scope,
        )?;
        let input = body.into_inner();
        let name = s4_require_text(&input.name, "Program increment name")?;
        let profile = methodology_profile(&project.methodology);
        if !profile.is_some_and(|profile| profile.uses_program_increments) {
            return Err(UnifiedError::bad_request(
                "methodology does not use program increments",
            ));
        }
        let status = input
            .status
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "planned".to_string());
        if !s4_valid_pi_status(&status) {
            return Err(UnifiedError::bad_request(
                "Program increment status must be one of: planned, active, completed",
            ));
        }
        s4_validate_date_range(input.start_date, input.end_date, "Program increment")?;
        let current = now();
        let pi = ProjectProgramIncrement {
            id: new_id(),
            project_id: project.id.clone(),
            name,
            status,
            start_date: input.start_date,
            end_date: input.end_date,
            objectives: input.objectives.unwrap_or_else(|| Value::Array(Vec::new())),
            created_at: current,
            updated_at: current,
        };
        state.program_increments.insert(pi.id.clone(), pi.clone());
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "program_increment.created",
            "program_increment",
            Some(pi.id.clone()),
            json!({ "name": pi.name, "status": pi.status }),
        );
        Ok(pi)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn get_program_increment(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, pi_id) = path.into_inner();
    let result: UnifiedResult<ProjectProgramIncrement> = (|| {
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
        s4_get_program_increment(&state, &project_id, &pi_id)
    })();
    json::respond(result, false)
}

pub(crate) async fn update_program_increment(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProgramIncrement>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, pi_id) = path.into_inner();
    let result: UnifiedResult<ProjectProgramIncrement> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
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
            if !s4_valid_pi_status(status.trim()) {
                return Err(UnifiedError::bad_request(
                    "Program increment status must be one of: planned, active, completed",
                ));
            }
        }
        let mut pi = state
            .program_increments
            .get_mut(pi_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Program increment not found"))?;
        if pi.project_id != project_id {
            return Err(UnifiedError::not_found("Program increment not found"));
        }
        let start_date = input.start_date.or(pi.start_date);
        let end_date = input.end_date.or(pi.end_date);
        s4_validate_date_range(start_date, end_date, "Program increment")?;
        if let Some(name) = input.name {
            pi.name = s4_require_text(&name, "Program increment name")?;
        }
        if let Some(status) = input.status {
            pi.status = status.trim().to_string();
        }
        pi.start_date = start_date;
        pi.end_date = end_date;
        if let Some(objectives) = input.objectives {
            pi.objectives = objectives;
        }
        pi.updated_at = now();
        let updated = pi.clone();
        drop(pi);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "program_increment.updated",
            "program_increment",
            Some(updated.id.clone()),
            json!({ "name": updated.name, "status": updated.status }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_program_increment(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, pi_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:edit",
            &_scope,
        )?;
        let pi = s4_get_program_increment(&state, &project_id, &pi_id)?;
        state
            .program_increments
            .remove(pi_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Program increment not found"))?;
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "program_increment.deleted",
            "program_increment",
            Some(pi.id.clone()),
            json!({ "name": pi.name }),
        );
        Ok(json!({ "deleted": true, "id": pi_id }))
    })();
    json::respond(result, false)
}

pub(crate) async fn plan_program_increment(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<PlanProgramIncrement>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, pi_id) = path.into_inner();
    let result: UnifiedResult<ProjectProgramIncrement> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:manage",
            &_scope,
        )?;
        let input = body.into_inner();
        let mut pi = state
            .program_increments
            .get_mut(pi_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Program increment not found"))?;
        if pi.project_id != project_id {
            return Err(UnifiedError::not_found("Program increment not found"));
        }
        plan_requires_planned_status(&pi)?;
        // Planning records the plan and keeps the increment planned;
        // it does not start the increment.
        pi.objectives = Value::Array(input.objectives);
        pi.status = "planned".to_string();
        pi.updated_at = now();
        let planned = pi.clone();
        drop(pi);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "program_increment.planned",
            "program_increment",
            Some(planned.id.clone()),
            json!({ "name": planned.name, "objectives": planned.objectives }),
        );
        state.notify(
            &project.owner_id,
            format!("Program increment planned: {}", planned.name),
            format!(
                "The plan for program increment {} was recorded.",
                planned.name
            ),
        );
        Ok(planned)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// PRINCE2 business cases
// ---------------------------------------------------------------------------

pub(crate) async fn list_business_cases(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<PageResult<ProjectBusinessCase>> = (|| {
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
        let sort = params.allowed_sort(&["created_at", "title", "updated_at"], "created_at");
        let mut items: Vec<ProjectBusinessCase> = state
            .business_cases
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|case| {
                case.project_id == path.as_str()
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| case.status == *status)
                    && matches_search(
                        &format!(
                            "{} {}",
                            case.title,
                            case.justification.as_str()
                        ),
                        &params,
                    )
            })
            .collect();
        match sort.as_str() {
            "title" => items.sort_by(|a, b| a.title.cmp(&b.title)),
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

pub(crate) async fn create_business_case(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateBusinessCase>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectBusinessCase> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            path.as_str(),
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:manage",
            &_scope,
        )?;
        let input = body.into_inner();
        let title = s4_require_text(&input.title, "Business case title")?;
        let status = input
            .status
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "draft".to_string());
        if !s4_valid_business_case_status(&status) {
            return Err(UnifiedError::bad_request(
                "Business case status must be one of: draft, approved, rejected",
            ));
        }
        let current = now();
        let case = ProjectBusinessCase {
            id: new_id(),
            project_id: project.id.clone(),
            title,
            justification: s4_clean_text(input.justification).unwrap_or_default(),
            benefits: input.benefits.unwrap_or_else(|| Value::Array(Vec::new())),
            costs: input.costs.unwrap_or_else(|| Value::Array(Vec::new())),
            risks: input.risks.unwrap_or_else(|| Value::Array(Vec::new())),
            status,
            exception_report: None,
            created_at: current,
            updated_at: current,
        };
        state.business_cases.insert(case.id.clone(), case.clone());
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "business_case.created",
            "business_case",
            Some(case.id.clone()),
            json!({ "title": case.title, "status": case.status }),
        );
        Ok(case)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn get_business_case(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, case_id) = path.into_inner();
    let result: UnifiedResult<ProjectBusinessCase> = (|| {
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
        s4_get_business_case(&state, &project_id, &case_id)
    })();
    json::respond(result, false)
}

pub(crate) async fn update_business_case(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateBusinessCase>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, case_id) = path.into_inner();
    let result: UnifiedResult<ProjectBusinessCase> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:manage",
            &_scope,
        )?;
        let input = body.into_inner();
        if let Some(status) = &input.status {
            if !s4_valid_business_case_status(status.trim()) {
                return Err(UnifiedError::bad_request(
                    "Business case status must be one of: draft, approved, rejected",
                ));
            }
        }
        let mut case = state
            .business_cases
            .get_mut(case_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Business case not found"))?;
        if case.project_id != project_id {
            return Err(UnifiedError::not_found("Business case not found"));
        }
        if let Some(title) = input.title {
            case.title = s4_require_text(&title, "Business case title")?;
        }
        if let Some(justification) = input.justification {
            case.justification = s4_clean_text(Some(justification)).unwrap_or_default();
        }
        if let Some(benefits) = input.benefits {
            case.benefits = benefits;
        }
        if let Some(costs) = input.costs {
            case.costs = costs;
        }
        if let Some(risks) = input.risks {
            case.risks = risks;
        }
        if let Some(status) = input.status {
            case.status = status.trim().to_string();
        }
        case.updated_at = now();
        let updated = case.clone();
        drop(case);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "business_case.updated",
            "business_case",
            Some(updated.id.clone()),
            json!({ "title": updated.title, "status": updated.status }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_business_case(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, case_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:manage",
            &_scope,
        )?;
        let case = s4_get_business_case(&state, &project_id, &case_id)?;
        state
            .business_cases
            .remove(case_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Business case not found"))?;
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "business_case.deleted",
            "business_case",
            Some(case.id.clone()),
            json!({ "title": case.title }),
        );
        Ok(json!({ "deleted": true, "id": case_id }))
    })();
    json::respond(result, false)
}

pub(crate) async fn save_exception_report(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<SaveExceptionReport>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, case_id) = path.into_inner();
    let result: UnifiedResult<ProjectBusinessCase> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let project = s4_resolve_project_for_write(
            &state,
            &project_id,
            viewer_id.as_deref(),
            viewer_role.as_deref(),
            "projects:manage",
            &_scope,
        )?;
        let input = body.into_inner();
        let report = s4_require_text(&input.report, "Exception report")?;
        let mut case = state
            .business_cases
            .get_mut(case_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Business case not found"))?;
        if case.project_id != project_id {
            return Err(UnifiedError::not_found("Business case not found"));
        }
        case.exception_report = Some(report);
        case.updated_at = now();
        let updated = case.clone();
        drop(case);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "business_case.exception_report_saved",
            "business_case",
            Some(updated.id.clone()),
            json!({ "title": updated.title }),
        );
        state.notify(
            &project.owner_id,
            format!("Exception report saved: {}", updated.title),
            format!(
                "An exception report was saved for business case {}.",
                updated.title
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

    fn pi_with_status(status: &str) -> ProjectProgramIncrement {
        ProjectProgramIncrement {
            id: "pi1".to_string(),
            project_id: "p1".to_string(),
            name: "PI 1".to_string(),
            status: status.to_string(),
            start_date: None,
            end_date: None,
            objectives: json!([]),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn planning_requires_a_planned_increment() {
        assert!(plan_requires_planned_status(&pi_with_status("planned")).is_ok());
        let error = plan_requires_planned_status(&pi_with_status("active")).unwrap_err();
        assert_eq!(error.code, 409);
        assert!(plan_requires_planned_status(&pi_with_status("completed")).is_err());
    }
}
