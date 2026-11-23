// Projects module — burndown, velocity, flow, cycle time, and critical path reports.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------------

pub(crate) async fn get_burndown_report(
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
        let sprint_id = params
            .filters
            .get("sprint_id")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| UnifiedError::bad_request("sprint_id query parameter is required"))?;
        let sprint = state
            .sprints
            .get(&sprint_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Sprint not found"))?;
        if sprint.project_id != project.id {
            return Err(UnifiedError::not_found("Sprint not found"));
        }
        let tasks: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| {
                task.project_id == project.id
                    && task.sprint_id.as_deref() == Some(sprint_id.as_str())
            })
            .collect();
        let total_points = s5_round2(
            tasks
                .iter()
                .map(|task| task.estimate_points.unwrap_or(0.0))
                .sum::<f64>(),
        );
        let today = now().date_naive();
        let start_day = sprint
            .start_date
            .map(|moment| moment.date_naive())
            .or_else(|| tasks.iter().map(|task| task.created_at.date_naive()).min())
            .unwrap_or(today);
        let mut end_day = sprint
            .end_date
            .map(|moment| moment.date_naive())
            .unwrap_or(today);
        if end_day < start_day {
            end_day = start_day;
        }
        // Cap the chart at one year so a bad date range cannot
        // produce an unbounded response.
        if end_day.signed_duration_since(start_day).num_days() > 365 {
            end_day = start_day + chrono::Duration::days(365);
        }
        let total_days = end_day.signed_duration_since(start_day).num_days();
        let mut day_rows: Vec<Value> = Vec::new();
        for offset in 0..=total_days {
            let day = start_day + chrono::Duration::days(offset);
            let day_end = s5_end_of_day(day);
            let ideal_remaining = if total_days == 0 {
                total_points
            } else {
                s5_round2(total_points * (1.0 - offset as f64 / total_days as f64)).max(0.0)
            };
            let actual_remaining = s5_round2(
                tasks
                    .iter()
                    .filter(|task| {
                        task.created_at <= day_end
                            && task
                                .completed_at
                                .is_none_or(|completed| completed > day_end)
                    })
                    .map(|task| task.estimate_points.unwrap_or(0.0))
                    .sum::<f64>(),
            );
            day_rows.push(json!({
                "date": day.format("%Y-%m-%d").to_string(),
                "ideal_remaining": ideal_remaining,
                "actual_remaining": actual_remaining,
            }));
        }
        Ok(json!({
            "sprint_id": sprint.id,
            "total_points": total_points,
            "days": day_rows,
        }))
    })();
    json::respond(result, false)
}

pub(crate) async fn get_velocity_report(
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
        require_capability(&state, viewer_role.as_deref(), "projects:reports")?;
        let mut sprints: Vec<ProjectSprint> = state
            .sprints
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sprint| {
                sprint.project_id == project.id
                    && matches!(sprint.status.as_str(), "completed" | "done" | "closed")
            })
            .collect();
        // Most recent first, then keep the last eight and present
        // them in chronological order for charting.
        sprints.sort_by(|a, b| {
            let end_a = a.end_date.unwrap_or(a.updated_at);
            let end_b = b.end_date.unwrap_or(b.updated_at);
            end_b.cmp(&end_a)
        });
        sprints.truncate(8);
        sprints.reverse();
        let rows: Vec<Value> = sprints
            .iter()
            .map(|sprint| {
                json!({
                    "sprint_id": sprint.id,
                    "name": sprint.name,
                    "planned": sprint.velocity_planned.unwrap_or(0.0),
                    "actual": sprint.velocity_actual.unwrap_or(0.0),
                })
            })
            .collect();
        let average_actual = if rows.is_empty() {
            0.0
        } else {
            s5_round2(
                sprints
                    .iter()
                    .map(|sprint| sprint.velocity_actual.unwrap_or(0.0))
                    .sum::<f64>()
                    / sprints.len() as f64,
            )
        };
        Ok(json!({
            "sprints": rows,
            "average_actual": average_actual,
        }))
    })();
    json::respond(result, false)
}

pub(crate) async fn get_cumulative_flow_report(
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
        let days = params
            .filters
            .get("days")
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(30)
            .clamp(1, 90);
        let tasks: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| task.project_id == project.id)
            .collect();
        let today = now().date_naive();
        let mut day_rows: Vec<Value> = Vec::new();
        for offset in (0..days).rev() {
            let day = today - chrono::Duration::days(offset);
            let day_end = s5_end_of_day(day);
            // Approximation: a task is present on a day once created.
            // It counts as done from the day it completed; otherwise
            // it counts under its current status.
            let mut counts: std::collections::BTreeMap<String, u64> =
                std::collections::BTreeMap::new();
            for task in &tasks {
                if task.created_at > day_end {
                    continue;
                }
                let bucket = if task
                    .completed_at
                    .is_some_and(|completed| completed <= day_end)
                    || (s5_task_is_done(task) && task.updated_at <= day_end)
                {
                    "done".to_string()
                } else {
                    task.status.clone()
                };
                *counts.entry(bucket).or_insert(0) += 1;
            }
            let mut counts_json = serde_json::Map::new();
            for (status, count) in counts {
                counts_json.insert(status, json!(count));
            }
            day_rows.push(json!({
                "date": day.format("%Y-%m-%d").to_string(),
                "counts": counts_json,
            }));
        }
        Ok(json!({ "days": day_rows }))
    })();
    json::respond(result, false)
}

pub(crate) async fn get_cycle_time_report(
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
        require_capability(&state, viewer_role.as_deref(), "projects:reports")?;
        let mut samples: Vec<(String, String, f64)> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| task.project_id == project.id)
            .filter_map(|task| {
                task.completed_at.map(|completed| {
                    let hours = s5_round2(
                        completed
                            .signed_duration_since(task.created_at)
                            .num_seconds() as f64
                            / 3600.0,
                    );
                    (task.id, task.title, hours)
                })
            })
            .collect();
        samples.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        let mut hours_sorted: Vec<f64> = samples.iter().map(|sample| sample.2).collect();
        hours_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let count = hours_sorted.len();
        let average_hours = if count == 0 {
            0.0
        } else {
            s5_round2(hours_sorted.iter().sum::<f64>() / count as f64)
        };
        let items: Vec<Value> = samples
            .iter()
            .map(|(task_id, title, hours)| {
                json!({
                    "task_id": task_id,
                    "title": title,
                    "hours": hours,
                })
            })
            .collect();
        Ok(json!({
            "average_hours": average_hours,
            "median_hours": s5_round2(s5_median(&hours_sorted)),
            "p90_hours": s5_round2(s5_percentile(&hours_sorted, 90.0)),
            "samples": count,
            "items": items,
        }))
    })();
    json::respond(result, false)
}

pub(crate) async fn get_critical_path_report(
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
        require_capability(&state, viewer_role.as_deref(), "projects:reports")?;
        let tasks: Vec<ProjectTask> = state
            .tasks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|task| task.project_id == project.id)
            .collect();
        let task_ids: std::collections::HashSet<String> =
            tasks.iter().map(|task| task.id.clone()).collect();
        // One working day of lag counts as eight working hours.
        let lag_hours = |lag_days: i32| lag_days as f64 * 8.0;
        let durations: std::collections::HashMap<String, f64> = tasks
            .iter()
            .map(|task| {
                let hours = task.estimate_hours.unwrap_or(8.0);
                (task.id.clone(), if hours > 0.0 { hours } else { 8.0 })
            })
            .collect();
        // Edges run from a prerequisite to its dependent, using only
        // finish-to-start dependencies.
        let mut successors: std::collections::HashMap<String, Vec<(String, f64)>> =
            std::collections::HashMap::new();
        let mut predecessors: std::collections::HashMap<String, Vec<(String, f64)>> =
            std::collections::HashMap::new();
        for dependency in state.dependencies.iter().map(|entry| entry.value().clone()) {
            if dependency.kind != "finish_start" {
                continue;
            }
            if !task_ids.contains(&dependency.task_id)
                || !task_ids.contains(&dependency.depends_on_task_id)
            {
                continue;
            }
            let lag = lag_hours(dependency.lag_days);
            successors
                .entry(dependency.depends_on_task_id.clone())
                .or_default()
                .push((dependency.task_id.clone(), lag));
            predecessors
                .entry(dependency.task_id.clone())
                .or_default()
                .push((dependency.depends_on_task_id.clone(), lag));
        }
        // Kahn topological sort.
        let mut indegree: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for task in &tasks {
            indegree.insert(task.id.clone(), 0);
        }
        for (_, edges) in &successors {
            for (to, _) in edges {
                if let Some(count) = indegree.get_mut(to) {
                    *count += 1;
                }
            }
        }
        let mut queue: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        for task in &tasks {
            if indegree.get(&task.id).copied().unwrap_or(0) == 0 {
                queue.push_back(task.id.clone());
            }
        }
        let mut order: Vec<String> = Vec::new();
        while let Some(node) = queue.pop_front() {
            order.push(node.clone());
            if let Some(edges) = successors.get(&node) {
                for (to, _) in edges {
                    if let Some(count) = indegree.get_mut(to) {
                        *count -= 1;
                        if *count == 0 {
                            queue.push_back(to.clone());
                        }
                    }
                }
            }
        }
        if order.len() != tasks.len() {
            // Not a DAG: recover one concrete cycle to name it.
            let plain_successors: std::collections::HashMap<String, Vec<String>> = successors
                .iter()
                .map(|(from, edges)| {
                    (
                        from.clone(),
                        edges.iter().map(|(to, _)| to.clone()).collect(),
                    )
                })
                .collect();
            let mut visit_state: std::collections::HashMap<String, u8> =
                std::collections::HashMap::new();
            let mut cycle: Vec<String> = Vec::new();
            for task in &tasks {
                if visit_state.get(&task.id).copied().unwrap_or(0) == 0 {
                    let mut stack: Vec<String> = Vec::new();
                    if let Some(found) =
                        s5_cycle_dfs(&task.id, &plain_successors, &mut visit_state, &mut stack)
                    {
                        cycle = found;
                        break;
                    }
                }
            }
            let description = if cycle.is_empty() {
                "dependency cycle detected".to_string()
            } else {
                format!("Dependency cycle detected: {}", cycle.join(" -> "))
            };
            return Err(UnifiedError::bad_request(description));
        }
        // Forward pass: earliest start/finish.
        let mut earliest_start: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();
        let mut earliest_finish: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();
        for node in &order {
            let start = predecessors
                .get(node)
                .map(|edges| {
                    edges
                        .iter()
                        .map(|(from, lag)| earliest_finish.get(from).copied().unwrap_or(0.0) + lag)
                        .fold(0.0, f64::max)
                })
                .unwrap_or(0.0);
            let duration = durations.get(node).copied().unwrap_or(8.0);
            earliest_start.insert(node.clone(), start);
            earliest_finish.insert(node.clone(), start + duration);
        }
        let total_duration_hours = s5_round2(earliest_finish.values().copied().fold(0.0, f64::max));
        // Backward pass: latest finish/start.
        let mut latest_finish: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();
        let mut latest_start: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();
        for node in order.iter().rev() {
            let finish = successors
                .get(node)
                .map(|edges| {
                    edges
                        .iter()
                        .map(|(to, lag)| {
                            latest_start
                                .get(to)
                                .copied()
                                .unwrap_or(total_duration_hours)
                                - lag
                        })
                        .fold(f64::INFINITY, f64::min)
                })
                .filter(|value| value.is_finite())
                .unwrap_or(total_duration_hours);
            let duration = durations.get(node).copied().unwrap_or(8.0);
            latest_finish.insert(node.clone(), finish);
            latest_start.insert(node.clone(), finish - duration);
        }
        let by_id: std::collections::HashMap<String, &ProjectTask> =
            tasks.iter().map(|task| (task.id.clone(), task)).collect();
        let mut rows: Vec<Value> = Vec::new();
        let mut critical_task_ids: Vec<String> = Vec::new();
        for node in &order {
            let early_start = earliest_start.get(node).copied().unwrap_or(0.0);
            let early_finish = earliest_finish.get(node).copied().unwrap_or(0.0);
            let late_start = latest_start.get(node).copied().unwrap_or(0.0);
            let late_finish = latest_finish.get(node).copied().unwrap_or(0.0);
            let float_hours = s5_round2(late_start - early_start);
            let critical = float_hours.abs() < 1e-9;
            if critical {
                critical_task_ids.push(node.clone());
            }
            let title = by_id
                .get(node)
                .map(|task| task.title.clone())
                .unwrap_or_default();
            rows.push(json!({
                "task_id": node,
                "title": title,
                "earliest_start_hours": s5_round2(early_start),
                "earliest_finish_hours": s5_round2(early_finish),
                "latest_start_hours": s5_round2(late_start),
                "latest_finish_hours": s5_round2(late_finish),
                "float_hours": float_hours,
                "critical": critical,
            }));
        }
        Ok(json!({
            "critical_task_ids": critical_task_ids,
            "total_duration_hours": total_duration_hours,
            "tasks": rows,
        }))
    })();
    json::respond(result, false)
}
