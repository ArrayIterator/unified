// timetracker — HTTP handlers: teams and team members.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub(crate) async fn list_teams(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Team>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let params = query.into_inner();
        let mut items: Vec<Team> = state
            .teams
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|team| team.company_id == company_id)
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_team(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateTeamRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Team> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_DESCRIPTION_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let manager_id = match &input.manager_id {
            Some(raw) => Some(require_text(raw, "manager_id", 128)?),
            None => None,
        };
        let now = Utc::now();
        let team = Team {
            id: Uuid::now_v7(),
            company_id,
            name,
            description,
            manager_id,
            created_at: now,
            updated_at: now,
        };
        state.teams.insert(team.id, team.clone());
        state.emit(
            "timetracker.team.created",
            json!({
                "team_id": team.id.to_string(),
                "company_id": company_id.to_string(),
                "name": team.name,
                "created_by": caller.actor,
            }),
        );
        Ok(team)
    })();
    match result {
        Ok(team) => json::success_response(team, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_team(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Team> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let team = load_team(&state, &path.into_inner())?;
        if team.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Team belongs to a different company",
            ));
        }
        Ok(team)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_team(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTeamRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Team> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        let mut team = load_team(&state, &path.into_inner())?;
        if team.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Team belongs to a different company",
            ));
        }
        if let Some(name) = &input.name {
            team.name = require_text(name, "name", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(description) = &input.description {
            team.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(manager_id) = input.manager_id {
            team.manager_id = match manager_id {
                Some(raw) => Some(require_text(&raw, "manager_id", 128)?),
                None => None,
            };
        }
        team.updated_at = Utc::now();
        state.teams.insert(team.id, team.clone());
        Ok(team)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_team(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let team = load_team(&state, &path.into_inner())?;
        if team.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Team belongs to a different company",
            ));
        }
        state.teams.remove(&team.id);
        let memberships: Vec<String> = state
            .team_members
            .iter()
            .filter(|entry| entry.value().team_id == team.id)
            .map(|entry| entry.key().clone())
            .collect();
        for key in memberships {
            state.team_members.remove(&key);
        }
        state.emit(
            "timetracker.team.deleted",
            json!({
                "team_id": team.id.to_string(),
                "company_id": company_id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "id": team.id.to_string(), "deleted": true }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_team_members(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TeamMember>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let team = load_team(&state, &path.into_inner())?;
        if team.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Team belongs to a different company",
            ));
        }
        let params = query.into_inner();
        let mut items: Vec<TeamMember> = state
            .team_members
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|member| member.team_id == team.id)
            .collect();
        items.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn add_team_member(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddTeamMemberRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TeamMember> =
        (|| {
            let _tenant = require_tenant(&req)?;
            let _scope = caller_company(&_tenant, &req)?;
            let caller = caller(&req)?;
            require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
            let company_id = staff_company(&ctx, &req, &caller)?;
            let input = body.into_inner();
            let team = load_team(&state, &path.into_inner())?;
            if team.company_id != company_id {
                return Err(UnifiedError::forbidden(
                    "Team belongs to a different company",
                ));
            }
            let user_id = require_text(&input.user_id, "user_id", 128)?;
            let staff = state
            .staff
            .get(&user_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!(
                "User {user_id} is not tracker staff of this company; add them as staff first"
            )))?;
            if staff.company_id != company_id {
                return Err(UnifiedError::forbidden(
                    "Staff member belongs to a different company",
                ));
            }
            let role = input.role.unwrap_or(staff.role);
            let key = team_member_key(team.id, &user_id);
            if state.team_members.contains_key(&key) {
                return Err(UnifiedError::conflict(format!(
                    "User {user_id} is already a member of this team"
                )));
            }
            let member = TeamMember {
                team_id: team.id,
                user_id: user_id.clone(),
                role,
                joined_at: Utc::now(),
            };
            state.team_members.insert(key, member.clone());
            state.emit(
                "timetracker.team.member.added",
                json!({
                    "team_id": team.id.to_string(),
                    "user_id": user_id,
                    "company_id": company_id.to_string(),
                    "role": format!("{:?}", role).to_lowercase(),
                    "added_by": caller.actor,
                }),
            );
            Ok(member)
        })();
    match result {
        Ok(member) => json::success_response(member, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn remove_team_member(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let (team_id, user_id) = path.into_inner();
        let team = load_team(&state, &team_id)?;
        if team.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Team belongs to a different company",
            ));
        }
        let key = team_member_key(team.id, &user_id);
        if !state.team_members.contains_key(&key) {
            return Err(UnifiedError::not_found(format!(
                "User {user_id} is not a member of this team"
            )));
        }
        state.team_members.remove(&key);
        state.emit(
            "timetracker.team.member.removed",
            json!({
                "team_id": team.id.to_string(),
                "user_id": user_id,
                "company_id": company_id.to_string(),
                "removed_by": caller.actor,
            }),
        );
        Ok(json!({ "team_id": team.id.to_string(), "user_id": user_id, "removed": true }))
    })();
    json::respond(result, debug)
}
