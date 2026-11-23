// timetracker — HTTP handlers: staff and invites.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub(crate) async fn list_staff(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<StaffOverview>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let params = query.into_inner();
        let role_filter = params.filters.get("role").cloned();
        let team_filter = params
            .filters
            .get("team_id")
            .map(|raw| parse_uuid(raw, "team_id"))
            .transpose()?;
        let mut items: Vec<StaffOverview> = state
            .staff
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|staff| staff.company_id == company_id)
            .filter(|staff| {
                if let Some(role) = &role_filter {
                    let expected = match role.as_str() {
                        "admin" => TrackerRole::Admin,
                        "manager" => TrackerRole::Manager,
                        "member" => TrackerRole::Member,
                        "viewer" => TrackerRole::Viewer,
                        _ => return false,
                    };
                    if staff.role != expected {
                        return false;
                    }
                }
                if let Some(team_id) = team_filter {
                    if !team_ids_for_user(&state, &staff.user_id).contains(&team_id) {
                        return false;
                    }
                }
                true
            })
            .map(|staff| staff_overview(&state, &staff))
            .collect();
        items.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn add_staff(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<AddStaffRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StaffOverview> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", 128)?;
        if state.staff.contains_key(&user_id) {
            return Err(UnifiedError::conflict(format!(
                "User {user_id} is already tracker staff"
            )));
        }
        let manager_id = match &input.manager_id {
            Some(raw) => Some(require_text(raw, "manager_id", 128)?),
            None => None,
        };
        let team_id = input.team_id;
        if let Some(team_id) = team_id {
            let team = state
                .teams
                .get(&team_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found(format!("Team not found: {team_id}")))?;
            if team.company_id != company_id {
                return Err(UnifiedError::forbidden(
                    "Team belongs to a different company",
                ));
            }
        }
        let now = Utc::now();
        let role = input.role.unwrap_or_default();
        let staff = StaffSettings {
            user_id: user_id.clone(),
            company_id,
            role,
            manager_id,
            screenshots_enabled: None,
            blur_screenshots: None,
            idle_threshold_secs: None,
            activity_level: None,
            timezone: None,
            working_hours_start: None,
            working_hours_end: None,
            created_at: now,
            updated_at: now,
        };
        state.staff.insert(user_id.clone(), staff.clone());
        if let Some(team_id) = team_id {
            state.team_members.insert(
                team_member_key(team_id, &user_id),
                TeamMember {
                    team_id,
                    user_id: user_id.clone(),
                    role,
                    joined_at: now,
                },
            );
        }
        state.emit(
            "timetracker.staff.added",
            json!({
                "user_id": user_id,
                "company_id": company_id.to_string(),
                "role": format!("{:?}", role).to_lowercase(),
                "team_id": team_id.map(|id| id.to_string()),
                "added_by": caller.actor,
            }),
        );
        Ok(staff_overview(&state, &staff))
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_staff(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StaffOverview> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let user_id = path.into_inner();
        let staff = state
            .staff
            .get(&user_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Staff member not found: {user_id}")))?;
        if staff.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Staff member belongs to a different company",
            ));
        }
        Ok(staff_overview(&state, &staff))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_staff(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateStaffRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StaffOverview> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        let user_id = path.into_inner();
        let mut staff = state
            .staff
            .get(&user_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Staff member not found: {user_id}")))?;
        if staff.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Staff member belongs to a different company",
            ));
        }
        if let Some(role) = input.role {
            staff.role = role;
        }
        if let Some(manager_id) = input.manager_id {
            staff.manager_id = match manager_id {
                Some(raw) => Some(require_text(&raw, "manager_id", 128)?),
                None => None,
            };
        }
        if let Some(value) = input.screenshots_enabled {
            staff.screenshots_enabled = value;
        }
        if let Some(value) = input.blur_screenshots {
            staff.blur_screenshots = value;
        }
        if let Some(value) = input.idle_threshold_secs {
            staff.idle_threshold_secs = match value {
                Some(secs) => {
                    if !(30..=86_400).contains(&secs) {
                        return Err(UnifiedError::bad_request(
                            "idle_threshold_secs must be between 30 and 86400",
                        ));
                    }
                    Some(secs)
                }
                None => None,
            };
        }
        if let Some(value) = input.activity_level {
            staff.activity_level = value;
        }
        if let Some(value) = input.timezone {
            staff.timezone = match value {
                Some(raw) => Some(valid_timezone(&raw)?),
                None => None,
            };
        }
        if let Some(value) = input.working_hours_start {
            staff.working_hours_start = match value {
                Some(raw) => Some(valid_hhmm(&raw, "working_hours_start")?),
                None => None,
            };
        }
        if let Some(value) = input.working_hours_end {
            staff.working_hours_end = match value {
                Some(raw) => Some(valid_hhmm(&raw, "working_hours_end")?),
                None => None,
            };
        }
        staff.updated_at = Utc::now();
        state.staff.insert(user_id.clone(), staff.clone());
        state.emit(
            "timetracker.staff.updated",
            json!({
                "user_id": user_id,
                "company_id": company_id.to_string(),
                "role": format!("{:?}", staff.role).to_lowercase(),
                "updated_by": caller.actor,
            }),
        );
        Ok(staff_overview(&state, &staff))
    })();
    json::respond(result, debug)
}

pub(crate) async fn remove_staff(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let user_id = path.into_inner();
        let staff = state
            .staff
            .get(&user_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Staff member not found: {user_id}")))?;
        if staff.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Staff member belongs to a different company",
            ));
        }
        // Removing staff removes the tracker link and the team
        // memberships only. Time entries, activity, screenshots,
        // and rates are history and are kept.
        state.staff.remove(&user_id);
        let memberships: Vec<String> = state
            .team_members
            .iter()
            .filter(|entry| entry.value().user_id == user_id)
            .map(|entry| entry.key().clone())
            .collect();
        for key in memberships {
            state.team_members.remove(&key);
        }
        state.emit(
            "timetracker.staff.removed",
            json!({
                "user_id": user_id,
                "company_id": company_id.to_string(),
                "removed_by": caller.actor,
            }),
        );
        Ok(json!({ "user_id": user_id, "removed": true }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn staff_activity(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let user_id = path.into_inner();
        let staff = state
            .staff
            .get(&user_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Staff member not found: {user_id}")))?;
        if staff.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Staff member belongs to a different company",
            ));
        }
        let params = query.into_inner();
        let (from, to) = parse_window(&params)?;
        let entries: Vec<TimeEntry> = state
            .entries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                entry.user_id == user_id && entry.started_at >= from && entry.started_at <= to
            })
            .collect();
        let total_secs: i64 = entries.iter().map(|entry| entry.duration_secs).sum();
        let billable_secs: i64 = entries
            .iter()
            .filter(|entry| entry.billable)
            .map(|entry| entry.duration_secs)
            .sum();
        let samples: Vec<ActivitySample> = state
            .activity
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sample| {
                sample.user_id == user_id
                    && sample.interval_start >= from
                    && sample.interval_start <= to
            })
            .collect();
        let avg_activity = if samples.is_empty() {
            None
        } else {
            Some(samples.iter().map(|sample| sample.activity_score).sum::<i64>() as f64 / samples.len() as f64)
        };
        let idle_secs: i64 = samples.iter().map(|sample| sample.idle_secs).sum();
        Ok(json!({
            "staff": staff_overview(&state, &staff),
            "from": from.to_rfc3339(),
            "to": to.to_rfc3339(),
            "total_secs": total_secs,
            "billable_secs": billable_secs,
            "entry_count": entries.len(),
            "avg_activity": avg_activity,
            "idle_secs": idle_secs,
            "activity_samples": samples.len(),
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_invites(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<StaffInvite>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<StaffInvite> = state
            .invites
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|invite| invite.company_id == company_id)
            .filter(|invite| {
                if let Some(status) = &status_filter {
                    if &invite.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_invite(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateInviteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        let email = valid_email(&input.email)?;
        if let Some(team_id) = input.team_id {
            let team = state
                .teams
                .get(&team_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found(format!("Team not found: {team_id}")))?;
            if team.company_id != company_id {
                return Err(UnifiedError::forbidden(
                    "Team belongs to a different company",
                ));
            }
        }
        let pending_same_email = state.invites.iter().any(|entry| {
            entry.value().company_id == company_id
                && entry.value().email == email
                && entry.value().status == "pending"
        });
        if pending_same_email {
            return Err(UnifiedError::conflict(format!(
                "A pending invite already exists for {email} in this company"
            )));
        }
        let now = Utc::now();
        // Opaque token: 32 random bytes, hex-encoded. Only the hash
        // is stored; the raw token is returned once, in this
        // response, for the invitation message.
        let token: String = {
            use rand::{distr::Alphanumeric, Rng};
            rand::rng()
                .sample_iter(&Alphanumeric)
                .take(48)
                .map(char::from)
                .collect()
        };
        let invite = StaffInvite {
            id: Uuid::now_v7(),
            company_id,
            email: email.clone(),
            role: input.role.unwrap_or_default(),
            team_id: input.team_id,
            invited_by: caller.actor.clone(),
            token_hash: sha256_hex(token.as_bytes()),
            status: "pending".to_string(),
            expires_at: now + Duration::days(7),
            accepted_at: None,
            accepted_user_id: None,
            created_at: now,
            updated_at: now,
        };
        state.invites.insert(invite.id, invite.clone());
        state.emit(
            "timetracker.staff.invite.created",
            json!({
                "invite_id": invite.id.to_string(),
                "company_id": company_id.to_string(),
                "email": email,
                "role": format!("{:?}", invite.role).to_lowercase(),
                "invited_by": caller.actor,
                "expires_at": invite.expires_at.to_rfc3339(),
            }),
        );
        Ok(json!({
            "invite": invite,
            "token": token,
            "note": "The token is shown once here. Send it to the invitee out of band; only its hash is stored.",
        }))
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn revoke_invite(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StaffInvite> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let id = parse_uuid(&path.into_inner(), "invite")?;
        let mut invite = state
            .invites
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Invite not found: {id}")))?;
        if invite.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Invite belongs to a different company",
            ));
        }
        if invite.status != "pending" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending invite can be revoked (current status: {})",
                invite.status
            )));
        }
        invite.status = "revoked".to_string();
        invite.updated_at = Utc::now();
        state.invites.insert(invite.id, invite.clone());
        state.emit(
            "timetracker.staff.invite.revoked",
            json!({
                "invite_id": invite.id.to_string(),
                "company_id": company_id.to_string(),
                "revoked_by": caller.actor,
            }),
        );
        Ok(invite)
    })();
    json::respond(result, debug)
}
