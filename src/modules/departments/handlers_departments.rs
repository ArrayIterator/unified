//! HTTP handlers for departments, SLAs and agents.

use super::helpers::{
    PERM_DEPARTMENTS_CREATE, PERM_DEPARTMENTS_MANAGE, PERM_DEPARTMENTS_VIEW, authorize,
    caller_company, require_tenant,
};
use super::state::DepartmentsState;
use super::types::{AddAgent, CreateDepartment, Department, UpdateDepartment};
use super::validation::{
    matches_search, new_id, now, paginate, respond_created, slugify, valid_email, valid_slug,
    valid_working_hours,
};
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;

// ---------------------------------------------------------------------------
// Handlers: departments
// ---------------------------------------------------------------------------

pub(crate) async fn list_departments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DepartmentsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_DEPARTMENTS_VIEW) {
        return json::error_response(&error, false);
    }
    let params = query.into_inner();
    let mut items: Vec<Department> = state
        .departments
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|department| {
            matches_search(
                &format!("{} {} {}", department.name, department.slug, department.description),
                &params,
            ) && params
                .filters
                .get("status")
                .is_none_or(|status| department.status == *status)
                && params
                    .filters
                    .get("is_default")
                    .is_none_or(|flag| department.is_default == (flag == "true"))
        })
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_department(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DepartmentsState>>,
    req: HttpRequest,
    body: web::Json<CreateDepartment>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_DEPARTMENTS_CREATE) {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<Department> = (|| {
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Department name must not be empty"));
        }
        let slug = match input.slug {
            Some(slug) if !slug.trim().is_empty() => slug.trim().to_lowercase(),
            _ => slugify(&input.name),
        };
        if !valid_slug(&slug) {
            return Err(UnifiedError::bad_request(
                "slug must be lowercase letters, digits, and hyphens",
            ));
        }
        if state
            .departments
            .iter()
            .any(|entry| entry.value().slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "A department with slug \"{slug}\" already exists"
            )));
        }
        let email = input
            .email
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(email) = &email {
            if !valid_email(email) {
                return Err(UnifiedError::bad_request("email must be a valid email address"));
            }
        }
        let status = input.status.unwrap_or_else(|| "active".to_string());
        if !matches!(status.as_str(), "active" | "inactive") {
            return Err(UnifiedError::bad_request(
                "status must be one of: active, inactive",
            ));
        }
        let first_response = input.sla_first_response_minutes.unwrap_or(240);
        let resolution = input.sla_resolution_minutes.unwrap_or(1440);
        if first_response == 0 || resolution == 0 {
            return Err(UnifiedError::bad_request(
                "SLA targets must be greater than zero minutes",
            ));
        }
        let working_hours = input.working_hours.unwrap_or_default();
        valid_working_hours(&working_hours)?;
        let is_default = input.is_default.unwrap_or(false);
        if is_default {
            for mut entry in state.departments.iter_mut() {
                entry.value_mut().is_default = false;
            }
        }
        let department = Department {
            id: new_id(),
            name: input.name.trim().to_string(),
            slug,
            description: input
                .description
                .map(|value| value.trim().to_string())
                .unwrap_or_default(),
            email,
            status,
            is_default,
            sla_first_response_minutes: first_response,
            sla_resolution_minutes: resolution,
            working_hours,
            auto_reply: input.auto_reply.unwrap_or_default(),
            agents: Vec::new(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .departments
            .insert(department.id.clone(), department.clone());
        let value = serde_json::to_value(&department).unwrap_or(Value::Null);
        state.emit("department.created", value);
        Ok(department)
    })();
    respond_created(result)
}

pub(crate) async fn get_department(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DepartmentsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_DEPARTMENTS_VIEW) {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<Department> = state
        .departments
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Department not found"));
    json::respond(result, false)
}

pub(crate) async fn update_department(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DepartmentsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDepartment>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_DEPARTMENTS_MANAGE) {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<Department> = (|| {
        let input = body.into_inner();
        if let Some(slug) = &input.slug {
            if !slug.trim().is_empty() && !valid_slug(&slug.trim().to_lowercase()) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and hyphens",
                ));
            }
            if !slug.trim().is_empty()
                && state.departments.iter().any(|entry| {
                    entry.key() != path.as_str() && entry.value().slug == slug.trim().to_lowercase()
                })
            {
                return Err(UnifiedError::conflict(format!(
                    "A department with slug \"{}\" already exists",
                    slug.trim().to_lowercase()
                )));
            }
        }
        if let Some(email) = &input.email {
            if !email.trim().is_empty() && !valid_email(email.trim()) {
                return Err(UnifiedError::bad_request("email must be a valid email address"));
            }
        }
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "inactive") {
                return Err(UnifiedError::bad_request(
                    "status must be one of: active, inactive",
                ));
            }
        }
        if let Some(hours) = &input.working_hours {
            valid_working_hours(hours)?;
        }
        if let Some(true) = input.is_default {
            for mut entry in state.departments.iter_mut() {
                if entry.key() != path.as_str() {
                    entry.value_mut().is_default = false;
                }
            }
        }
        let mut department = state
            .departments
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Department not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Department name must not be empty"));
            }
            department.name = name.trim().to_string();
        }
        if let Some(slug) = input.slug {
            if !slug.trim().is_empty() {
                department.slug = slug.trim().to_lowercase();
            }
        }
        if let Some(description) = input.description {
            department.description = description;
        }
        if let Some(email) = input.email {
            department.email = if email.trim().is_empty() {
                None
            } else {
                Some(email.trim().to_string())
            };
        }
        if let Some(status) = input.status {
            department.status = status;
        }
        if let Some(is_default) = input.is_default {
            department.is_default = is_default;
        }
        if let Some(minutes) = input.sla_first_response_minutes {
            if minutes == 0 {
                return Err(UnifiedError::bad_request(
                    "sla_first_response_minutes must be greater than zero",
                ));
            }
            department.sla_first_response_minutes = minutes;
        }
        if let Some(minutes) = input.sla_resolution_minutes {
            if minutes == 0 {
                return Err(UnifiedError::bad_request(
                    "sla_resolution_minutes must be greater than zero",
                ));
            }
            department.sla_resolution_minutes = minutes;
        }
        if let Some(hours) = input.working_hours {
            department.working_hours = hours;
        }
        if let Some(auto_reply) = input.auto_reply {
            department.auto_reply = auto_reply;
        }
        department.updated_at = now();
        let value = serde_json::to_value(&*department).unwrap_or(Value::Null);
        let updated = department.clone();
        drop(department);
        state.emit("department.updated", value);
        Ok(updated)
    })();
    json::respond(result, false)
}

/// Fail-closed deletion guard: `None` means the tickets module did
/// not answer `department.delete_check`, so ticket ownership cannot
/// be verified and the department must stay. Only an explicit zero
/// count allows deletion.
fn deletion_guard(ticket_count: Option<u64>) -> UnifiedResult<()> {
    match ticket_count {
        Some(0) => Ok(()),
        Some(count) => Err(UnifiedError::conflict(format!(
            "Department still owns {count} ticket(s); move or close them first"
        ))
        .with_metadata("ticket_count", json!(count))),
        None => Err(UnifiedError::conflict(
            "Cannot verify whether the department still owns tickets; deletion refused",
        )),
    }
}

pub(crate) async fn delete_department(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DepartmentsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_DEPARTMENTS_MANAGE) {
        return json::error_response(&error, false);
    }
    let department_id = path.as_str().to_string();
    // The Tickets module answers `department.delete_check`; a
    // department that still owns tickets cannot be deleted, and an
    // unanswered check refuses deletion rather than deleting blind.
    if let Err(error) = deletion_guard(state.ticket_count_for(&department_id)) {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<Value> = (|| {
        state
            .departments
            .remove(&department_id)
            .map(|_| json!({ "deleted": true, "id": &department_id }))
            .ok_or_else(|| UnifiedError::not_found("Department not found"))
    })();
    if result.is_ok() {
        // Routing rules belong to the department; they go with it.
        state
            .rules
            .retain(|_, rule| rule.department_id != department_id);
        state.emit(
            "department.deleted",
            json!({ "department_id": department_id }),
        );
    }
    json::respond(result, false)
}

pub(crate) async fn get_department_sla(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DepartmentsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_DEPARTMENTS_VIEW) {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<Value> = state
        .departments
        .get(path.as_str())
        .map(|entry| {
            let department = entry.value();
            json!({
                "department_id": &department.id,
                "first_response_minutes": department.sla_first_response_minutes,
                "resolution_minutes": department.sla_resolution_minutes,
            })
        })
        .ok_or_else(|| UnifiedError::not_found("Department not found"));
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Handlers: agents
// ---------------------------------------------------------------------------

pub(crate) async fn list_agents(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DepartmentsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_DEPARTMENTS_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let department_id = path.as_str().to_string();
        let agents = state
            .departments
            .get(path.as_str())
            .map(|entry| entry.value().agents.clone())
            .ok_or_else(|| UnifiedError::not_found("Department not found"))?;
        let page = paginate(agents, &params);
        let mut data = page.to_json();
        data["department_id"] = json!(department_id);
        Ok(data)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn add_agent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DepartmentsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddAgent>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_DEPARTMENTS_CREATE) {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        if input.user_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        let user_id = input.user_id.trim().to_string();
        if let Some(role_id) = &input.role_id {
            if !role_id.trim().is_empty() && !state.role_exists(role_id.trim()) {
                return Err(UnifiedError::not_found(format!(
                    "Role {} not found",
                    role_id.trim()
                )));
            }
        }
        let mut department = state
            .departments
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Department not found"))?;
        if department.agents.iter().any(|agent| *agent == user_id) {
            return Err(UnifiedError::conflict(format!(
                "User {user_id} is already an agent of this department"
            )));
        }
        department.agents.push(user_id.clone());
        department.updated_at = now();
        let value = json!({
            "department_id": &department.id,
            "agent_ids": &department.agents,
        });
        drop(department);
        state.emit(
            "department.agent_added",
            json!({ "department_id": path.as_str(), "user_id": user_id }),
        );
        Ok(value)
    })();
    json::respond(result, false)
}

pub(crate) async fn remove_agent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DepartmentsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_DEPARTMENTS_MANAGE) {
        return json::error_response(&error, false);
    }
    let (department_id, user_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let mut department = state
            .departments
            .get_mut(department_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Department not found"))?;
        let before = department.agents.len();
        department.agents.retain(|agent| *agent != user_id);
        if department.agents.len() == before {
            return Err(UnifiedError::not_found(format!(
                "User {user_id} is not an agent of this department"
            )));
        }
        department.updated_at = now();
        let value = json!({
            "department_id": &department.id,
            "agent_ids": &department.agents,
        });
        drop(department);
        state.emit(
            "department.agent_removed",
            json!({ "department_id": department_id, "user_id": user_id }),
        );
        Ok(value)
    })();
    json::respond(result, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletion_guard_refuses_when_ticket_count_is_unknown() {
        // No answer from the tickets module: deletion must be refused
        // rather than deleting blind.
        assert!(deletion_guard(None).is_err());
        assert_eq!(deletion_guard(None).unwrap_err().code, 409);
        assert!(deletion_guard(Some(0)).is_ok());
        assert!(deletion_guard(Some(3)).is_err());
    }
}
