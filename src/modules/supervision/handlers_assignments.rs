// supervision module -- HTTP handlers for assignments and templates.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// Assignment handlers

/// Validate an assignment period against the effective start and
/// end, whatever the caller supplied in this request. Callers
/// must pass the merged values (request fields over the stored
/// ones) so moving only `starts_at` past an existing `ends_at`
/// is caught just like supplying both ends inverted.
pub(crate) fn validate_assignment_period(
    starts_at: DateTime<Utc>,
    ends_at: Option<DateTime<Utc>>,
) -> UnifiedResult<()> {
    if let Some(end) = ends_at {
        if end <= starts_at {
            return Err(UnifiedError::bad_request(
                "ends_at must be after starts_at",
            ));
        }
    }
    Ok(())
}

pub(crate) async fn list_assignments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let params = query.into_inner();
        let project_filter = params.filters.get("project_id").cloned();
        let user_filter = params.filters.get("user_id").cloned();
        let role_filter = params.filters.get("role").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<SupervisionAssignment> = state
            .assignments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|assignment| {
                if let Some(project_id) = &project_filter {
                    if &assignment.project_id != project_id {
                        return false;
                    }
                }
                if let Some(user_id) = &user_filter {
                    if &assignment.user_id != user_id {
                        return false;
                    }
                }
                if let Some(role) = &role_filter {
                    if assignment.role.as_str() != role {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if assignment.status.as_str() != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|assignment| assignment.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn create_assignment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    body: web::Json<CreateAssignmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SupervisionAssignment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let input = body.into_inner();
        let project_id = require_id(&input.project_id, "project_id")?;
        let project = query_project(&ctx, &project_id)?;
        if project.archived {
            return Err(UnifiedError::conflict(
                "cannot assign a supervisor to an archived project",
            ));
        }
        let user_id = require_id(&input.user_id, "user_id")?;
        validate_assignment_period(input.starts_at, input.ends_at)?;
        let notes = opt_text(&input.notes, "notes", MAX_TEXT_LEN)?;
        let now = Utc::now();
        let assignment = SupervisionAssignment {
            id: Uuid::now_v7(),
            project_id: project.id,
            user_id: user_id.clone(),
            role: input.role,
            status: AssignmentStatus::Active,
            starts_at: input.starts_at,
            ends_at: input.ends_at,
            notes,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state
            .assignments
            .insert(assignment.id, assignment.clone());
        state.emit(
            "supervision.assignment.created",
            json!({
                "assignment_id": assignment.id.to_string(),
                "project_id": assignment.project_id,
                "user_id": assignment.user_id,
                "role": assignment.role.as_str(),
            }),
        );
        state.notify(
            "supervision.assignment.created",
            &user_id,
            "Supervision assignment created",
            &format!(
                "You were assigned as {} on project \"{}\".",
                assignment.role.as_str(),
                project.name
            ),
            Some(format!("/api/v1/supervision/assignments/{}", assignment.id)),
        );
        Ok(assignment)
    })();
    match result {
        Ok(assignment) => created(assignment),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_assignment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SupervisionAssignment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        load_assignment(&state, &path.into_inner())
    })();
    match result {
        Ok(assignment) => ok(assignment),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn update_assignment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateAssignmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SupervisionAssignment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let mut assignment = load_assignment(&state, &path.into_inner())?;
        let input = body.into_inner();
        if assignment.status == AssignmentStatus::Ended {
            return Err(UnifiedError::conflict(
                "an ended assignment cannot be updated",
            ));
        }
        if let Some(role) = input.role {
            assignment.role = role;
        }
        if let Some(status) = input.status {
            assignment.status = status;
        }
        if let Some(starts_at) = input.starts_at {
            assignment.starts_at = starts_at;
        }
        if let Some(ends_at) = input.ends_at {
            assignment.ends_at = ends_at;
        }
        // Validate the merged period, not just the supplied
        // fields: moving only starts_at past the stored ends_at
        // inverts the period just as surely as supplying both.
        validate_assignment_period(assignment.starts_at, assignment.ends_at)?;
        if let Some(notes) = input.notes {
            assignment.notes = match notes {
                Some(raw) => Some(require_text(&raw, "notes", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        assignment.updated_at = Utc::now();
        state
            .assignments
            .insert(assignment.id, assignment.clone());
        state.emit(
            "supervision.assignment.updated",
            json!({
                "assignment_id": assignment.id.to_string(),
                "project_id": assignment.project_id,
                "status": assignment.status.as_str(),
            }),
        );
        Ok(assignment)
    })();
    match result {
        Ok(assignment) => ok(assignment),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn delete_assignment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let assignment = load_assignment(&state, &path.into_inner())?;
        state.assignments.remove(&assignment.id);
        state.emit(
            "supervision.assignment.deleted",
            json!({
                "assignment_id": assignment.id.to_string(),
                "project_id": assignment.project_id,
            }),
        );
        Ok(json!({ "deleted": true, "assignment_id": assignment.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Inspection template handlers


pub(crate) async fn list_templates(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let params = query.into_inner();
        let project_filter = params.filters.get("project_id").cloned();
        let category_filter = params.filters.get("category").cloned();
        let mut items: Vec<InspectionTemplate> = state
            .templates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|template| {
                if let Some(project_id) = &project_filter {
                    if template.project_id.as_deref() != Some(project_id.as_str()) {
                        return false;
                    }
                }
                if let Some(category) = &category_filter {
                    if &template.category != category {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|template| template.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn create_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    body: web::Json<CreateTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InspectionTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let input = body.into_inner();
        let project_id = match &input.project_id {
            Some(raw) => {
                let id = require_id(raw, "project_id")?;
                // A project-scoped template must reference a real
                // project; a shared template (null) needs no lookup.
                query_project(&ctx, &id)?;
                Some(id)
            }
            None => None,
        };
        let category = match &input.category {
            Some(raw) => require_text(raw, "category", 80)?,
            None => "general".to_string(),
        };
        let items = build_template_items(&input.items)?;
        if items.is_empty() {
            return Err(UnifiedError::bad_request(
                "a template must contain at least one item",
            ));
        }
        let now = Utc::now();
        let template = InspectionTemplate {
            id: Uuid::now_v7(),
            project_id,
            title: require_text(&input.title, "title", MAX_TITLE_LEN)?,
            description: opt_text(&input.description, "description", MAX_TEXT_LEN)?,
            category,
            items,
            created_by: caller.actor,
            created_at: now,
            updated_at: now,
        };
        state.templates.insert(template.id, template.clone());
        state.emit(
            "supervision.template.created",
            json!({
                "template_id": template.id.to_string(),
                "project_id": template.project_id,
                "title": template.title,
            }),
        );
        Ok(template)
    })();
    match result {
        Ok(template) => created(template),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InspectionTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        load_template(&state, &path.into_inner())
    })();
    match result {
        Ok(template) => ok(template),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn update_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InspectionTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let mut template = load_template(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(project_id) = input.project_id {
            template.project_id = match project_id {
                Some(raw) => {
                    let id = require_id(&raw, "project_id")?;
                    query_project(&ctx, &id)?;
                    Some(id)
                }
                None => None,
            };
        }
        if let Some(title) = input.title {
            template.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = input.description {
            template.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        if let Some(category) = input.category {
            template.category = require_text(&category, "category", 80)?;
        }
        if let Some(items) = input.items {
            let built = build_template_items(&items)?;
            if built.is_empty() {
                return Err(UnifiedError::bad_request(
                    "a template must contain at least one item",
                ));
            }
            template.items = built;
        }
        template.updated_at = Utc::now();
        state.templates.insert(template.id, template.clone());
        state.emit(
            "supervision.template.updated",
            json!({ "template_id": template.id.to_string() }),
        );
        Ok(template)
    })();
    match result {
        Ok(template) => ok(template),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn delete_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let template = load_template(&state, &path.into_inner())?;
        let in_use = state
            .inspections
            .iter()
            .any(|entry| entry.value().template_id == Some(template.id));
        if in_use {
            return Err(UnifiedError::conflict(
                "cannot delete a template that inspections were created from",
            ));
        }
        state.templates.remove(&template.id);
        state.emit(
            "supervision.template.deleted",
            json!({ "template_id": template.id.to_string() }),
        );
        Ok(json!({ "deleted": true, "template_id": template.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Inspection handlers


#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, 0, 0, 0)
            .single()
            .expect("valid date")
    }

    #[test]
    fn period_with_end_after_start_is_valid() {
        assert!(validate_assignment_period(at(2026, 1, 1), Some(at(2026, 1, 2))).is_ok());
    }

    #[test]
    fn open_ended_period_is_valid() {
        assert!(validate_assignment_period(at(2026, 1, 1), None).is_ok());
    }

    #[test]
    fn end_equal_to_start_is_rejected() {
        assert!(validate_assignment_period(at(2026, 1, 1), Some(at(2026, 1, 1))).is_err());
    }

    #[test]
    fn end_before_start_is_rejected() {
        assert!(validate_assignment_period(at(2026, 1, 2), Some(at(2026, 1, 1))).is_err());
    }

    #[test]
    fn moving_only_starts_at_past_stored_ends_at_is_rejected() {
        // The S2 defect: an update moving only starts_at later
        // past the stored ends_at was unchecked. Validating the
        // merged period catches it.
        let stored_ends_at = Some(at(2026, 1, 10));
        let moved_starts_at = at(2026, 1, 15);
        assert!(validate_assignment_period(moved_starts_at, stored_ends_at).is_err());
    }

    #[test]
    fn clearing_ends_at_allows_a_later_start() {
        assert!(validate_assignment_period(at(2026, 1, 15), None).is_ok());
    }
}
