// Projects module - import, export, and activity handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::sync::Arc;

// Import / export

/// Validate one imported row with the same rules the create-task path
/// applies: a known task type, a status the methodology allows, and a
/// known priority. Returns the cleaned values, or a skip reason for
/// the import error list.
fn import_row_values(
    project: &Project,
    task_type: Option<&str>,
    status: Option<&str>,
    priority: Option<&str>,
) -> Result<(String, String, String), String> {
    let task_type = task_type
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "task".to_string());
    if !s3_valid_task_type(&task_type) {
        return Err(format!("Unknown task_type: {task_type}"));
    }
    let status = status
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| s3_default_status(project));
    if !s3_methodology_allows_status(project, &status) {
        return Err(format!(
            "Unknown status for this project methodology: {status}"
        ));
    }
    let priority = priority
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "normal".to_string());
    if !s3_valid_priority(&priority) {
        return Err(format!("Unknown priority: {priority}"));
    }
    Ok((task_type, status, priority))
}

/// Completion timestamp for an imported task: a done status carries
/// one, so the timestamp-based done check agrees with the
/// status-based one instead of disagreeing on the same task.
fn import_completed_at(status: &str) -> Option<DateTime<Utc>> {
    if s2_task_is_done(status) {
        Some(now())
    } else {
        None
    }
}

pub(crate) async fn export_project(
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
        require_capability(&state, viewer_role.as_deref(), "projects:manage")?;
        let format = params
            .filters
            .get("format")
            .map(|value| value.trim().to_lowercase())
            .unwrap_or_else(|| "json".to_string());
        if format != "json" && format != "csv" {
            return Err(UnifiedError::bad_request("format must be csv or json"));
        }
        let mut tasks: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| task.project_id == project.id)
            .collect();
        tasks.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if format == "csv" {
            let mut content = String::from(
                "id,title,description,task_type,status,priority,assignee_id,estimate_points,estimate_hours,due_date,labels\n",
            );
            for task in &tasks {
                let row = [
                    s5_csv_escape(&task.id),
                    s5_csv_escape(&task.title),
                    s5_csv_escape(&task.description),
                    s5_csv_escape(&task.task_type),
                    s5_csv_escape(&task.status),
                    s5_csv_escape(&task.priority),
                    s5_csv_escape(task.assignee_id.as_deref().unwrap_or("")),
                    s5_csv_number(task.estimate_points),
                    s5_csv_number(task.estimate_hours),
                    s5_csv_escape(&s5_csv_datetime(task.due_date)),
                    s5_csv_escape(&task.labels.join(";")),
                ];
                content.push_str(&row.join(","));
                content.push('\n');
            }
            return Ok(json!({
                "format": "csv",
                "filename": format!("project-{}-tasks.csv", project.id),
                "content": content,
            }));
        }
        let mut milestones: Vec<_> = state
            .milestones
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|milestone| milestone.project_id == project.id)
            .collect();
        milestones.sort_by(|a, b| a.name.cmp(&b.name));
        let mut sprints: Vec<ProjectSprint> = state
            .sprints
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sprint| sprint.project_id == project.id)
            .collect();
        sprints.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let mut phases: Vec<_> = state
            .phases
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|phase| phase.project_id == project.id)
            .collect();
        phases.sort_by(|a, b| a.name.cmp(&b.name));
        let mut time_entries: Vec<ProjectTimeEntry> = state
            .time_entries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.project_id == project.id)
            .collect();
        time_entries.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(json!({
            "project": project,
            "tasks": tasks,
            "milestones": milestones,
            "sprints": sprints,
            "phases": phases,
            "time_entries": time_entries,
        }))
    })();
    json::respond(result, false)
}

pub(crate) async fn import_project(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<ImportProject>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<Value> = (|| {
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
        require_capability(&state, viewer_role.as_deref(), "projects:manage")?;
        let input = body.into_inner();
        if input.format.trim().to_lowercase() != "json" {
            return Err(UnifiedError::bad_request("format must be json"));
        }
        let reporter_id = viewer_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| project.owner_id.clone());
        let mut imported: u64 = 0;
        let mut skipped: u64 = 0;
        let mut errors: Vec<Value> = Vec::new();
        for (index, row) in input.tasks.into_iter().enumerate() {
            let title = row.title.map(|value| value.trim().to_string());
            let title = match title {
                Some(value) if !value.is_empty() => value,
                _ => {
                    skipped += 1;
                    errors.push(json!({
                        "row": index,
                        "error": "title is required",
                    }));
                    continue;
                }
            };
            if let Some(points) = row.estimate_points {
                if points < 0.0 {
                    skipped += 1;
                    errors.push(json!({
                        "row": index,
                        "title": title,
                        "error": "estimate_points must not be negative",
                    }));
                    continue;
                }
            }
            if let Some(hours) = row.estimate_hours {
                if hours < 0.0 {
                    skipped += 1;
                    errors.push(json!({
                        "row": index,
                        "title": title,
                        "error": "estimate_hours must not be negative",
                    }));
                    continue;
                }
            }
            let (task_type, status, priority) = match import_row_values(
                &project,
                row.task_type.as_deref(),
                row.status.as_deref(),
                row.priority.as_deref(),
            ) {
                Ok(values) => values,
                Err(error) => {
                    skipped += 1;
                    errors.push(json!({
                        "row": index,
                        "title": title,
                        "error": error,
                    }));
                    continue;
                }
            };
            let completed_at = import_completed_at(&status);
            let task = ProjectTask {
                id: new_id(),
                project_id: project.id.clone(),
                board_id: None,
                parent_id: None,
                epic_id: None,
                title,
                description: row
                    .description
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_default(),
                task_type,
                status,
                priority,
                assignee_id: row
                    .assignee_id
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
                reporter_id: reporter_id.clone(),
                estimate_points: row.estimate_points,
                estimate_hours: row.estimate_hours,
                labels: Vec::new(),
                custom_values: Value::Null,
                start_date: None,
                due_date: row.due_date,
                completed_at,
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
            };
            state.tasks.insert(task.id.clone(), task);
            imported += 1;
        }
        state.log_activity(
            &project.id,
            &reporter_id,
            "project.imported",
            "project",
            Some(project.id.clone()),
            json!({
                "imported": imported,
                "skipped": skipped,
            }),
        );
        Ok(json!({
            "imported": imported,
            "skipped": skipped,
            "errors": errors,
        }))
    })();
    s5_respond_created(result)
}

// Activity

pub(crate) async fn list_project_activity(
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
        let mut items: Vec<ProjectActivity> = state
            .activity
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|activity| {
                activity.project_id == project.id
                    && params
                        .filters
                        .get("actor_id")
                        .is_none_or(|value| activity.actor_id == *value)
                    && params
                        .filters
                        .get("action")
                        .is_none_or(|value| activity.action == *value)
                    && params
                        .filters
                        .get("target_type")
                        .is_none_or(|value| activity.target_type == *value)
                    && matches_search(
                        &format!(
                            "{} {} {}",
                            activity.action, activity.target_type, activity.actor_id
                        ),
                        &params,
                    )
            })
            .collect();
        // Newest first by default; an explicit asc order is honoured.
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.order.as_deref() != Some("asc") {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn scrum_project() -> Project {
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
    fn imported_rows_use_create_task_validation() {
        let project = scrum_project();
        let (task_type, status, priority) =
            import_row_values(&project, Some("bug"), Some("review"), Some("high")).unwrap();
        assert_eq!(task_type, "bug");
        assert_eq!(status, "review");
        assert_eq!(priority, "high");
        // Defaults follow the create path: task type, methodology
        // default status, and normal priority.
        let (_, status, priority) = import_row_values(&project, None, None, None).unwrap();
        assert_eq!(status, "backlog");
        assert_eq!(priority, "normal");
        // Unknown values are rejected, exactly as create rejects them.
        assert!(import_row_values(&project, Some("bug"), Some("banana"), Some("high")).is_err());
        assert!(import_row_values(&project, Some("bug"), Some("review"), Some("mega")).is_err());
        assert!(import_row_values(&project, Some("epic"), Some("review"), Some("high")).is_err());
    }

    #[test]
    fn imported_done_tasks_carry_a_completion_timestamp() {
        assert!(import_completed_at("done").is_some());
        assert!(import_completed_at("completed").is_some());
        assert!(import_completed_at("backlog").is_none());
        assert!(import_completed_at("in_progress").is_none());
    }
}
