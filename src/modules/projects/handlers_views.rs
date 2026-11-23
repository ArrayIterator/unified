// Projects module — workload, Gantt, timeline, and calendar handlers.
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
// Workload
// ---------------------------------------------------------------------------

pub(crate) async fn get_project_workload(
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
        require_capability(&state, viewer_role.as_deref(), "projects:view")?;
        let capacity_hours = params
            .filters
            .get("capacity_hours")
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| *value > 0.0)
            .unwrap_or(40.0);
        // Rolling seven-day window for "this week" logged hours.
        let week_start = now() - chrono::Duration::days(7);
        let tasks: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| task.project_id == project.id)
            .collect();
        let mut member_ids: Vec<String> = state
            .members
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|member| member.project_id == project.id)
            .map(|member| member.user_id)
            .collect();
        member_ids.sort();
        member_ids.dedup();
        if member_ids.is_empty() {
            // No roster recorded: fall back to the distinct assignees
            // of open tasks so the workload is never empty.
            let mut assignees: Vec<String> = tasks
                .iter()
                .filter(|task| s5_task_is_open(task))
                .filter_map(|task| task.assignee_id.clone())
                .collect();
            assignees.sort();
            assignees.dedup();
            member_ids = assignees;
        }
        let mut workload: Vec<Value> = member_ids
            .iter()
            .map(|user_id| {
                let open_tasks: Vec<&ProjectTask> = tasks
                    .iter()
                    .filter(|task| {
                        task.assignee_id.as_deref() == Some(user_id.as_str())
                            && s5_task_is_open(task)
                    })
                    .collect();
                let assigned_hours = s5_round2(
                    open_tasks
                        .iter()
                        .map(|task| task.estimate_hours.unwrap_or(0.0))
                        .sum::<f64>(),
                );
                let logged_hours_week = s5_round2(
                    state
                        .time_entries
                        .iter()
                        .map(|entry| entry.value().clone())
                        .filter(|entry| {
                            entry.project_id == project.id
                                && entry.user_id == *user_id
                                && entry.started_at >= week_start
                        })
                        .map(|entry| entry.hours)
                        .sum::<f64>(),
                );
                let utilization = if capacity_hours > 0.0 {
                    s5_round4(assigned_hours / capacity_hours)
                } else {
                    0.0
                };
                json!({
                    "user_id": user_id,
                    "open_tasks": open_tasks.len(),
                    "assigned_hours": assigned_hours,
                    "logged_hours_week": logged_hours_week,
                    "capacity_hours": capacity_hours,
                    "utilization": utilization,
                })
            })
            .collect();
        workload.sort_by(|a, b| {
            let left = a.get("utilization").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let right = b.get("utilization").and_then(|v| v.as_f64()).unwrap_or(0.0);
            right
                .partial_cmp(&left)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(json!(workload))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Views: Gantt, timeline, calendar
// ---------------------------------------------------------------------------

pub(crate) async fn get_project_gantt(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
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
        require_capability(&state, viewer_role.as_deref(), "projects:view")?;
        let mut tasks: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| task.project_id == project.id)
            .collect();
        tasks.sort_by(|a, b| {
            let start_a = a.start_date.unwrap_or(a.created_at);
            let start_b = b.start_date.unwrap_or(b.created_at);
            start_a.cmp(&start_b).then(a.title.cmp(&b.title))
        });
        let rows: Vec<Value> = tasks
            .iter()
            .map(|task| {
                let start = task.start_date.unwrap_or(task.created_at);
                let end = task
                    .due_date
                    .unwrap_or_else(|| start + chrono::Duration::days(1));
                let dependencies: Vec<String> = state
                    .dependencies
                    .iter()
                    .map(|entry| entry.value().clone())
                    .filter(|dependency| dependency.task_id == task.id)
                    .map(|dependency| dependency.depends_on_task_id)
                    .collect();
                json!({
                    "task_id": task.id,
                    "title": task.title,
                    "start": start,
                    "end": end,
                    "progress": if s5_task_is_done(task) { 100 } else { 0 },
                    "dependencies": dependencies,
                })
            })
            .collect();
        Ok(json!(rows))
    })();
    json::respond(result, false)
}

pub(crate) async fn get_project_timeline(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
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
        require_capability(&state, viewer_role.as_deref(), "projects:view")?;
        let mut rows: Vec<(DateTime<Utc>, Value)> = Vec::new();
        for milestone in state.milestones.iter().map(|entry| entry.value().clone()) {
            if milestone.project_id != project.id {
                continue;
            }
            if let Some(date) = milestone.due_date {
                rows.push((
                    date,
                    json!({
                        "date": date,
                        "kind": "milestone",
                        "title": milestone.name,
                        "id": milestone.id,
                    }),
                ));
            }
        }
        for sprint in state.sprints.iter().map(|entry| entry.value().clone()) {
            if sprint.project_id != project.id {
                continue;
            }
            if let Some(date) = sprint.start_date.or(sprint.end_date) {
                rows.push((
                    date,
                    json!({
                        "date": date,
                        "kind": "sprint",
                        "title": sprint.name,
                        "id": sprint.id,
                    }),
                ));
            }
        }
        for phase in state.phases.iter().map(|entry| entry.value().clone()) {
            if phase.project_id != project.id {
                continue;
            }
            if let Some(date) = phase.start_date.or(phase.end_date) {
                rows.push((
                    date,
                    json!({
                        "date": date,
                        "kind": "phase",
                        "title": phase.name,
                        "id": phase.id,
                    }),
                ));
            }
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let items: Vec<Value> = rows.into_iter().map(|(_, row)| row).collect();
        Ok(json!(items))
    })();
    json::respond(result, false)
}

pub(crate) async fn get_project_calendar(
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
        require_capability(&state, viewer_role.as_deref(), "projects:view")?;
        let current = now();
        let year = match params.filters.get("year") {
            Some(value) => value
                .parse::<i32>()
                .map_err(|_| UnifiedError::bad_request("year must be a valid year"))?,
            None => current
                .format("%Y")
                .to_string()
                .parse::<i32>()
                .unwrap_or(2026),
        };
        let month = match params.filters.get("month") {
            Some(value) => value.parse::<u32>().map_err(|_| {
                UnifiedError::bad_request("month must be a number between 1 and 12")
            })?,
            None => current.format("%m").to_string().parse::<u32>().unwrap_or(1),
        };
        if !(1970..=2100).contains(&year) {
            return Err(UnifiedError::bad_request(
                "year must be between 1970 and 2100",
            ));
        }
        if !(1..=12).contains(&month) {
            return Err(UnifiedError::bad_request("month must be between 1 and 12"));
        }
        // Group task and milestone due dates by calendar day within
        // the requested month. Days without items are omitted.
        let mut days: std::collections::BTreeMap<String, (Vec<Value>, Vec<Value>)> =
            std::collections::BTreeMap::new();
        for task in state.tasks.iter().map(|entry| entry.value().clone()) {
            if task.project_id != project.id {
                continue;
            }
            if let Some(due) = task.due_date {
                let due_year: i32 = due.format("%Y").to_string().parse().unwrap_or(0);
                let due_month: u32 = due.format("%m").to_string().parse().unwrap_or(0);
                if due_year == year && due_month == month {
                    let key = due.format("%Y-%m-%d").to_string();
                    let bucket = days.entry(key).or_default();
                    bucket.0.push(json!({
                        "id": task.id,
                        "title": task.title,
                        "status": task.status,
                    }));
                }
            }
        }
        for milestone in state.milestones.iter().map(|entry| entry.value().clone()) {
            if milestone.project_id != project.id {
                continue;
            }
            if let Some(due) = milestone.due_date {
                let due_year: i32 = due.format("%Y").to_string().parse().unwrap_or(0);
                let due_month: u32 = due.format("%m").to_string().parse().unwrap_or(0);
                if due_year == year && due_month == month {
                    let key = due.format("%Y-%m-%d").to_string();
                    let bucket = days.entry(key).or_default();
                    bucket.1.push(json!({
                        "id": milestone.id,
                        "title": milestone.name,
                    }));
                }
            }
        }
        let day_rows: Vec<Value> = days
            .into_iter()
            .map(|(date, (tasks, milestones))| {
                json!({
                    "date": date,
                    "tasks": tasks,
                    "milestones": milestones,
                })
            })
            .collect();
        Ok(json!({
            "year": year,
            "month": month,
            "days": day_rows,
        }))
    })();
    json::respond(result, false)
}
