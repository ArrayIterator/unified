// Projects module — project template handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Project templates
// ---------------------------------------------------------------------------

/// Read guard for the template list and get handlers: templates are
/// only readable by a role holding `projects:view`, like every other
/// template handler's capability check.
fn require_template_read(state: &ProjectsState, viewer_role: Option<&str>) -> UnifiedResult<()> {
    require_capability(state, viewer_role, "projects:view")
}

pub(crate) async fn list_project_templates(
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
    let (_viewer_id, viewer_role) = viewer_of(&req);
    if let Err(error) = require_template_read(&state, viewer_role.as_deref()) {
        return json::error_response(&error, false);
    }
    let sort = params.allowed_sort(&["created_at", "updated_at", "name"], "created_at");
    let mut items: Vec<ProjectTemplate> = state
        .templates
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|template| {
            matches_search(
                &format!("{} {}", template.name, template.description),
                &params,
            ) && params
                .filters
                .get("methodology")
                .is_none_or(|value| template.methodology == *value)
                && params
                    .filters
                    .get("is_system")
                    .is_none_or(|value| template.is_system == (value == "true"))
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

pub(crate) async fn create_project_template(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateProjectTemplate>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (_viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_capability(&state, viewer_role.as_deref(), "projects:manage")?;
        let input = body.into_inner();
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err(UnifiedError::bad_request("Template name must not be empty"));
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
        let timestamp = now();
        let template = ProjectTemplate {
            id: new_id(),
            name,
            description: s2_clean_optional(input.description).unwrap_or_default(),
            methodology,
            boards: input.boards.unwrap_or_else(|| json!([])),
            columns: input.columns.unwrap_or_else(|| json!([])),
            statuses: input.statuses.unwrap_or_else(|| json!([])),
            phases: input.phases.unwrap_or_else(|| json!([])),
            custom_fields: input.custom_fields.unwrap_or_else(|| json!({})),
            is_system: input.is_system.unwrap_or(false),
            created_at: timestamp,
            updated_at: timestamp,
        };
        state
            .templates
            .insert(template.id.clone(), template.clone());
        Ok(template)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn get_project_template(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (_viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_template_read(&state, viewer_role.as_deref())?;
        state
            .templates
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Project template not found"))
    })();
    json::respond(result, false)
}

pub(crate) async fn update_project_template(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProjectTemplate>,
) -> HttpResponse {
    let template_id = path.into_inner();
    let _params = query.into_inner();
    let (_viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_capability(&state, viewer_role.as_deref(), "projects:manage")?;
        let input = body.into_inner();
        if let Some(methodology) = input.methodology.as_ref() {
            if !valid_methodology(methodology.trim()) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown methodology: {}",
                    methodology.trim()
                )));
            }
        }
        let mut stored = state
            .templates
            .get_mut(&template_id)
            .ok_or_else(|| UnifiedError::not_found("Project template not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Template name must not be empty"));
            }
            stored.name = name.trim().to_string();
        }
        if let Some(description) = input.description {
            stored.description = description.trim().to_string();
        }
        if let Some(methodology) = input.methodology {
            stored.methodology = methodology.trim().to_string();
        }
        if let Some(boards) = input.boards {
            stored.boards = boards;
        }
        if let Some(columns) = input.columns {
            stored.columns = columns;
        }
        if let Some(statuses) = input.statuses {
            stored.statuses = statuses;
        }
        if let Some(phases) = input.phases {
            stored.phases = phases;
        }
        if let Some(custom_fields) = input.custom_fields {
            stored.custom_fields = custom_fields;
        }
        if let Some(is_system) = input.is_system {
            stored.is_system = is_system;
        }
        stored.updated_at = now();
        Ok(stored.clone())
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_project_template(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let template_id = path.into_inner();
    let _params = query.into_inner();
    let (_viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_capability(&state, viewer_role.as_deref(), "projects:manage")?;
        let template = state
            .templates
            .get(&template_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Project template not found"))?;
        if template.is_system {
            return Err(UnifiedError::forbidden(
                "System templates cannot be deleted",
            ));
        }
        state.templates.remove(&template_id);
        Ok(json!({ "deleted": true, "id": template_id }))
    })();
    json::respond(result, false)
}

pub(crate) async fn apply_project_template(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<ApplyTemplate>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<Project> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_capability(&state, viewer_role.as_deref(), "projects:manage")?;
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
        let template = state
            .templates
            .get(input.template_id.trim())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Project template not found"))?;
        let default_kind = methodology_profile(&project.methodology)
            .map(|profile| profile.default_board_kind.to_string())
            .unwrap_or_else(|| "kanban".to_string());
        let timestamp = now();
        // Boards from the template, with embedded columns when the
        // board object carries its own `columns` array.
        let mut created_boards: Vec<(String, String)> = Vec::new();
        let mut embedded_columns = false;
        let board_values = template.boards.as_array().cloned().unwrap_or_default();
        for board_value in &board_values {
            let Some(board_object) = board_value.as_object() else {
                continue;
            };
            let board_name = board_object
                .get("name")
                .and_then(|entry| entry.as_str())
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .unwrap_or("Board")
                .to_string();
            let kind = board_object
                .get("kind")
                .and_then(|entry| entry.as_str())
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .unwrap_or(&default_kind)
                .to_string();
            let wip_limit = board_object
                .get("wip_limit")
                .and_then(|entry| entry.as_u64())
                .map(|limit| limit as u32)
                .filter(|limit| *limit > 0);
            let board = ProjectBoard {
                id: new_id(),
                project_id: project_id.clone(),
                name: board_name.clone(),
                kind,
                wip_limit,
                created_at: timestamp,
                updated_at: timestamp,
            };
            state.boards.insert(board.id.clone(), board.clone());
            created_boards.push((board.id.clone(), board_name));
            if let Some(columns) = board_object
                .get("columns")
                .and_then(|entry| entry.as_array())
            {
                for (position, column_value) in columns.iter().enumerate() {
                    if let Some(column) =
                        s2_column_from_value(&board.id, column_value, position as u32)
                    {
                        state.columns.insert(column.id.clone(), column);
                        embedded_columns = true;
                    }
                }
            }
        }
        // Standalone `columns` array: attach each column to the board
        // named by `board_name` / `board_id` when present, otherwise
        // to the first board the template created.
        if !embedded_columns {
            if let Some(columns) = template.columns.as_array() {
                for (position, column_value) in columns.iter().enumerate() {
                    let target_board = column_value
                        .get("board_name")
                        .and_then(|entry| entry.as_str())
                        .and_then(|name| {
                            created_boards
                                .iter()
                                .find(|(_, board_name)| board_name == name)
                                .map(|(board_id, _)| board_id.clone())
                        })
                        .or_else(|| {
                            column_value
                                .get("board_id")
                                .and_then(|entry| entry.as_str())
                                .and_then(|board_id| {
                                    created_boards
                                        .iter()
                                        .find(|(id, _)| id == board_id)
                                        .map(|(id, _)| id.clone())
                                })
                        })
                        .or_else(|| created_boards.first().map(|(board_id, _)| board_id.clone()));
                    let Some(board_id) = target_board else {
                        continue;
                    };
                    if let Some(column) =
                        s2_column_from_value(&board_id, column_value, position as u32)
                    {
                        state.columns.insert(column.id.clone(), column);
                    }
                }
            }
        }
        // Phases from the template `phases` array.
        if let Some(phases) = template.phases.as_array() {
            for (index, phase_value) in phases.iter().enumerate() {
                let Some(phase_object) = phase_value.as_object() else {
                    continue;
                };
                let Some(phase_name) = phase_object
                    .get("name")
                    .and_then(|entry| entry.as_str())
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                else {
                    continue;
                };
                let position = phase_object
                    .get("position")
                    .and_then(|entry| entry.as_u64())
                    .or_else(|| phase_object.get("order").and_then(|entry| entry.as_u64()))
                    .unwrap_or(index as u64) as u32;
                let parse_date = |key: &str| {
                    phase_object
                        .get(key)
                        .and_then(|entry| entry.as_str())
                        .and_then(|text| {
                            DateTime::parse_from_rfc3339(text)
                                .ok()
                                .map(|parsed| parsed.with_timezone(&Utc))
                        })
                };
                let phase = ProjectPhase {
                    id: new_id(),
                    project_id: project_id.clone(),
                    name: phase_name.to_string(),
                    kind: phase_object
                        .get("kind")
                        .and_then(|entry| entry.as_str())
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .unwrap_or("waterfall")
                        .to_string(),
                    position,
                    status: "planned".to_string(),
                    gate_status: "pending".to_string(),
                    gate_decision: None,
                    start_date: parse_date("start_date"),
                    end_date: parse_date("due_date").or_else(|| parse_date("end_date")),
                    created_at: timestamp,
                    updated_at: timestamp,
                };
                state.phases.insert(phase.id.clone(), phase);
            }
        }
        // The template's custom fields become the project's own.
        let mut stored = state
            .projects
            .get_mut(&project_id)
            .ok_or_else(|| UnifiedError::not_found("Project not found"))?;
        stored.custom_field_definitions = template.custom_fields.clone();
        stored.updated_at = now();
        let updated = stored.clone();
        drop(stored);
        let actor = viewer_id.as_deref().unwrap_or("system");
        state.log_activity(
            &project_id,
            actor,
            "template.applied",
            "template",
            Some(template.id.clone()),
            json!({ "template_id": template.id, "template": template.name }),
        );
        state.emit(
            "project.template.applied",
            json!({
                "project_id": project_id,
                "template_id": template.id,
                "project": serde_json::to_value(&updated).unwrap_or(Value::Null),
            }),
        );
        state.notify(
            &updated.owner_id,
            format!("Template applied to project: {}", updated.name),
            format!(
                "Template {} was applied to project {}.",
                template.name, updated.name
            ),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_reads_require_the_view_capability() {
        let state = ProjectsState::new();
        // No role service is wired in a bare state, so the check
        // fails closed for anonymous and for named roles alike.
        assert!(require_template_read(&state, None).is_err());
        assert!(require_template_read(&state, Some("admin")).is_err());
    }
}
