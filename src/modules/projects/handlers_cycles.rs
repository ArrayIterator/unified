// Projects module - Shape Up cycle handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::sync::Arc;

// Shape Up cycles

/// Decision stored on the cycle for a bet: the decision itself. A
/// pass records "pass", never the pitch text.
fn bet_stored_decision(decision: &str, _pitch: &Option<String>) -> String {
    decision.to_string()
}

/// A cycle may only complete from `cooldown`, the state immediately
/// before done (shaping -> betting -> building -> cooldown -> done).
/// Completing from planned, shaping, or betting skips the work, and
/// completing a done cycle is an error.
fn cycle_complete_requires_cooldown(cycle: &ProjectCycle) -> UnifiedResult<()> {
    if cycle.status == "done" {
        return Err(UnifiedError::bad_request("Cycle is already done"));
    }
    if cycle.status != "cooldown" {
        return Err(UnifiedError::bad_request(
            "Only a cycle in cooldown can be completed",
        ));
    }
    Ok(())
}

pub(crate) async fn list_project_cycles(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<PageResult<ProjectCycle>> = (|| {
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
        let sort = params.allowed_sort(&["created_at", "name", "updated_at"], "created_at");
        let mut items: Vec<ProjectCycle> = state
            .cycles
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|cycle| {
                cycle.project_id == path.as_str()
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| cycle.status == *status)
                    && matches_search(
                        &format!(
                            "{} {}",
                            cycle.name,
                            cycle.pitch.as_deref().unwrap_or_default()
                        ),
                        &params,
                    )
            })
            .collect();
        match sort.as_str() {
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
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

pub(crate) async fn create_project_cycle(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateProjectCycle>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let result: UnifiedResult<ProjectCycle> = (|| {
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
        let name = s4_require_text(&input.name, "Cycle name")?;
        let profile = methodology_profile(&project.methodology);
        if !profile.is_some_and(|profile| profile.uses_cycles) {
            return Err(UnifiedError::bad_request("methodology does not use cycles"));
        }
        let appetite_weeks = input.appetite_weeks.unwrap_or(6);
        if !(1..=12).contains(&appetite_weeks) {
            return Err(UnifiedError::bad_request(
                "appetite_weeks must be between 1 and 12",
            ));
        }
        let cooldown_weeks = input.cooldown_weeks.unwrap_or(2);
        let status = input
            .status
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "shaping".to_string());
        if !s4_valid_cycle_status(&status) {
            return Err(UnifiedError::bad_request(
                "Cycle status must be one of: shaping, betting, building, cooldown, done",
            ));
        }
        let current = now();
        let cycle = ProjectCycle {
            id: new_id(),
            project_id: project.id.clone(),
            name,
            appetite_weeks,
            status,
            started_at: input.started_at,
            ends_at: input.ends_at,
            cooldown_weeks,
            pitch: s4_clean_text(input.pitch),
            betting_decision: None,
            created_at: current,
            updated_at: current,
        };
        state.cycles.insert(cycle.id.clone(), cycle.clone());
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "cycle.created",
            "cycle",
            Some(cycle.id.clone()),
            json!({ "name": cycle.name, "appetite_weeks": cycle.appetite_weeks }),
        );
        Ok(cycle)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn get_project_cycle(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, cycle_id) = path.into_inner();
    let result: UnifiedResult<ProjectCycle> = (|| {
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
        s4_get_cycle(&state, &project_id, &cycle_id)
    })();
    json::respond(result, false)
}

pub(crate) async fn update_project_cycle(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdateProjectCycle>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, cycle_id) = path.into_inner();
    let result: UnifiedResult<ProjectCycle> = (|| {
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
        if let Some(appetite_weeks) = input.appetite_weeks {
            if !(1..=12).contains(&appetite_weeks) {
                return Err(UnifiedError::bad_request(
                    "appetite_weeks must be between 1 and 12",
                ));
            }
        }
        if let Some(status) = &input.status {
            if !s4_valid_cycle_status(status.trim()) {
                return Err(UnifiedError::bad_request(
                    "Cycle status must be one of: shaping, betting, building, cooldown, done",
                ));
            }
        }
        let mut cycle = state
            .cycles
            .get_mut(cycle_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Cycle not found"))?;
        if cycle.project_id != project_id {
            return Err(UnifiedError::not_found("Cycle not found"));
        }
        if let Some(name) = input.name {
            cycle.name = s4_require_text(&name, "Cycle name")?;
        }
        if let Some(appetite_weeks) = input.appetite_weeks {
            cycle.appetite_weeks = appetite_weeks;
        }
        if let Some(cooldown_weeks) = input.cooldown_weeks {
            cycle.cooldown_weeks = cooldown_weeks;
        }
        if let Some(status) = input.status {
            cycle.status = status.trim().to_string();
        }
        if let Some(pitch) = input.pitch {
            cycle.pitch = s4_clean_text(Some(pitch));
        }
        if let Some(started_at) = input.started_at {
            cycle.started_at = Some(started_at);
        }
        if let Some(ends_at) = input.ends_at {
            cycle.ends_at = Some(ends_at);
        }
        cycle.updated_at = now();
        let updated = cycle.clone();
        drop(cycle);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "cycle.updated",
            "cycle",
            Some(updated.id.clone()),
            json!({ "name": updated.name, "status": updated.status }),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_project_cycle(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, cycle_id) = path.into_inner();
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
        let cycle = s4_get_cycle(&state, &project_id, &cycle_id)?;
        state
            .cycles
            .remove(cycle_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Cycle not found"))?;
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "cycle.deleted",
            "cycle",
            Some(cycle.id.clone()),
            json!({ "name": cycle.name }),
        );
        Ok(json!({ "deleted": true, "id": cycle_id }))
    })();
    json::respond(result, false)
}

pub(crate) async fn bet_project_cycle(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
    body: web::Json<BetProjectCycle>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, cycle_id) = path.into_inner();
    let result: UnifiedResult<ProjectCycle> = (|| {
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
        let decision = input.decision.trim().to_string();
        if decision != "bet" && decision != "pass" {
            return Err(UnifiedError::bad_request(
                "Bet decision must be one of: bet, pass",
            ));
        }
        let pitch = s4_clean_text(input.pitch);
        let cycle = s4_get_cycle(&state, &project_id, &cycle_id)?;
        if cycle.status == "done" {
            return Err(UnifiedError::bad_request(
                "Cannot bet on a cycle that is already done",
            ));
        }
        let mut cycle = state
            .cycles
            .get_mut(cycle_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Cycle not found"))?;
        if decision == "bet" {
            if cycle.status != "betting" {
                return Err(UnifiedError::bad_request(
                    "Only a cycle in betting can be bet on",
                ));
            }
            cycle.status = "building".to_string();
            cycle.betting_decision = Some(bet_stored_decision("bet", &pitch));
        } else {
            cycle.status = "done".to_string();
            cycle.betting_decision = Some(bet_stored_decision("pass", &pitch));
        }
        if let Some(pitch) = pitch {
            cycle.pitch = Some(pitch);
        }
        cycle.updated_at = now();
        let updated = cycle.clone();
        drop(cycle);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "cycle.bet",
            "cycle",
            Some(updated.id.clone()),
            json!({ "name": updated.name, "decision": decision, "status": updated.status }),
        );
        state.notify(
            &project.owner_id,
            format!("Cycle {}: {}", decision, updated.name),
            format!(
                "The betting decision for cycle {} is {}.",
                updated.name, decision
            ),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn complete_project_cycle(
    req: HttpRequest,
    state: web::Data<Arc<ProjectsState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _params = query.into_inner();
    let (viewer_id, viewer_role) = viewer_of(&req);
    let (project_id, cycle_id) = path.into_inner();
    let result: UnifiedResult<ProjectCycle> = (|| {
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
        let cycle = s4_get_cycle(&state, &project_id, &cycle_id)?;
        cycle_complete_requires_cooldown(&cycle)?;
        let current = now();
        let mut cycle = state
            .cycles
            .get_mut(cycle_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Cycle not found"))?;
        cycle.status = "done".to_string();
        if cycle.ends_at.is_none() {
            cycle.ends_at = Some(current);
        }
        cycle.updated_at = current;
        let completed = cycle.clone();
        drop(cycle);
        state.log_activity(
            &project.id,
            s4_actor(&viewer_id),
            "cycle.completed",
            "cycle",
            Some(completed.id.clone()),
            json!({ "name": completed.name }),
        );
        state.notify(
            &project.owner_id,
            format!("Cycle done: {}", completed.name),
            format!("Cycle {} is now done.", completed.name),
        );
        Ok(completed)
    })();
    json::respond(result, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn cycle_with_status(status: &str) -> ProjectCycle {
        ProjectCycle {
            id: "c1".to_string(),
            project_id: "p1".to_string(),
            name: "Cycle".to_string(),
            appetite_weeks: 6,
            status: status.to_string(),
            started_at: None,
            ends_at: None,
            cooldown_weeks: 2,
            pitch: None,
            betting_decision: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn pass_bet_stores_pass_not_the_pitch() {
        assert_eq!(
            bet_stored_decision("pass", &Some("Ship the new onboarding".to_string())),
            "pass"
        );
        assert_eq!(bet_stored_decision("pass", &None), "pass");
        assert_eq!(
            bet_stored_decision("bet", &Some("Ship the new onboarding".to_string())),
            "bet"
        );
    }

    #[test]
    fn complete_requires_the_cooldown_state() {
        assert!(cycle_complete_requires_cooldown(&cycle_with_status("cooldown")).is_ok());
        assert!(cycle_complete_requires_cooldown(&cycle_with_status("shaping")).is_err());
        assert!(cycle_complete_requires_cooldown(&cycle_with_status("betting")).is_err());
        assert!(cycle_complete_requires_cooldown(&cycle_with_status("building")).is_err());
        assert!(cycle_complete_requires_cooldown(&cycle_with_status("done")).is_err());
    }
}
