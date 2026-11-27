//! incidents - handlers rotations.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
use super::{constants::*, types::*, inputs::*, state::*, helpers::*, handlers_incidents::*, handlers_policies::*, handlers_postmortems::*, handlers_auto_open::*, module::*};



// Rotation handlers
pub(crate) async fn list_rotations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        let params = query.into_inner();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<OnCallRotation> = state
            .rotations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rotation| {
                search.is_empty() || rotation.name.to_lowercase().contains(&search)
            })
            .collect();
        items.sort_by_key(|rotation| rotation.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn create_rotation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    body: web::Json<CreateRotationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OnCallRotation> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let members = validate_user_ids(&input.members, "members", MAX_MEMBERS)?;
        if input.handover_interval_days < 1 || input.handover_interval_days > 365 {
            return Err(UnifiedError::bad_request(
                "handover_interval_days must be between 1 and 365",
            ));
        }
        if !valid_handover_time(&input.handover_time) {
            return Err(UnifiedError::bad_request("handover_time must be \"HH:MM\" in 24-hour form"));
        }
        if !valid_timezone(&input.timezone) {
            return Err(UnifiedError::bad_request("timezone must be a valid IANA name"));
        }
        let now = Utc::now();
        let rotation = OnCallRotation {
            id: Uuid::now_v7(),
            name,
            description,
            members,
            handover_interval_days: input.handover_interval_days,
            handover_time: input.handover_time,
            timezone: input.timezone,
            current_index: 0,
            created_at: now,
            updated_at: now,
        };
        state.rotations.insert(rotation.id, rotation.clone());
        Ok(rotation)
    })();
    match result {
        Ok(rotation) => created(rotation),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_rotation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OnCallRotation> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        load_rotation(&state, &path.into_inner())
    })();
    match result {
        Ok(rotation) => ok(rotation),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn update_rotation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRotationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OnCallRotation> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let rotation = load_rotation(&state, &path.into_inner())?;
        let input = body.into_inner();
        let mut updated = rotation.clone();
        if let Some(name) = input.name {
            updated.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = input.description {
            updated.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(days) = input.handover_interval_days {
            if days < 1 || days > 365 {
                return Err(UnifiedError::bad_request(
                    "handover_interval_days must be between 1 and 365",
                ));
            }
            updated.handover_interval_days = days;
        }
        if let Some(time) = input.handover_time {
            if !valid_handover_time(&time) {
                return Err(UnifiedError::bad_request(
                    "handover_time must be \"HH:MM\" in 24-hour form",
                ));
            }
            updated.handover_time = time;
        }
        if let Some(timezone) = input.timezone {
            if !valid_timezone(&timezone) {
                return Err(UnifiedError::bad_request("timezone must be a valid IANA name"));
            }
            updated.timezone = timezone;
        }
        updated.updated_at = Utc::now();
        state.rotations.insert(updated.id, updated.clone());
        Ok(updated)
    })();
    match result {
        Ok(rotation) => ok(rotation),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn delete_rotation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let rotation = load_rotation(&state, &path.into_inner())?;
        if state
            .incidents
            .iter()
            .any(|entry| entry.value().rotation_id == Some(rotation.id))
        {
            return Err(UnifiedError::conflict(
                "Rotation is still used by incidents; detach it first",
            ));
        }
        for entry in state
            .schedules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.rotation_id == rotation.id)
        {
            state.schedules.remove(&entry.id);
        }
        state.rotations.remove(&rotation.id);
        Ok(json!({ "deleted": true, "rotation_id": rotation.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn add_rotation_member(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddMemberRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OnCallRotation> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let mut rotation = load_rotation(&state, &path.into_inner())?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", 255)?;
        if rotation.members.contains(&user_id) {
            return Err(UnifiedError::conflict(format!(
                "User \"{user_id}\" is already a rotation member"
            )));
        }
        if rotation.members.len() >= MAX_MEMBERS {
            return Err(UnifiedError::conflict(format!(
                "A rotation may have at most {MAX_MEMBERS} members"
            )));
        }
        rotation.members.push(user_id);
        rotation.updated_at = Utc::now();
        state.rotations.insert(rotation.id, rotation.clone());
        Ok(rotation)
    })();
    match result {
        Ok(rotation) => ok(rotation),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn remove_rotation_member(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OnCallRotation> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let (rotation_id, user_id) = path.into_inner();
        let mut rotation = load_rotation(&state, &rotation_id)?;
        let user_id = user_id.trim().to_string();
        if !rotation.members.contains(&user_id) {
            return Err(UnifiedError::not_found(format!(
                "User \"{user_id}\" is not a member of this rotation"
            )));
        }
        rotation.members.retain(|id| id != &user_id);
        if rotation.current_index >= rotation.members.len() {
            rotation.current_index = 0;
        }
        rotation.updated_at = Utc::now();
        state.rotations.insert(rotation.id, rotation.clone());
        Ok(rotation)
    })();
    match result {
        Ok(rotation) => ok(rotation),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn handover_rotation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OnCallRotation> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let mut rotation = load_rotation(&state, &path.into_inner())?;
        if rotation.members.is_empty() {
            return Err(UnifiedError::conflict(
                "Rotation has no members to hand over to",
            ));
        }
        let previous = rotation
            .members
            .get(rotation.current_index)
            .cloned();
        advance_rotation(&state, &mut rotation);
        let current = rotation
            .members
            .get(rotation.current_index)
            .cloned();
        state.emit(
            "incidents.rotation.handover",
            json!({
                "rotation_id": rotation.id.to_string(),
                "previous_user_id": previous,
                "current_user_id": current,
                "handed_over_by": caller.actor,
            }),
        );
        Ok(rotation)
    })();
    match result {
        Ok(rotation) => ok(rotation),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_on_call(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        let rotation = load_rotation(&state, &path.into_inner())?;
        let now = Utc::now();
        let member = on_call_member(&state, &rotation, now);
        Ok(json!({
            "rotation_id": rotation.id.to_string(),
            "on_call_user_id": member,
            "checked_at": now.to_rfc3339(),
            "handover_time": rotation.handover_time,
            "handover_interval_days": rotation.handover_interval_days,
            "timezone": rotation.timezone,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn list_schedules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        let rotation = load_rotation(&state, &path.into_inner())?;
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let member_filter = params.filters.get("member_id").cloned();
        let mut items: Vec<OnCallSchedule> = state
            .schedules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                if entry.rotation_id != rotation.id {
                    return false;
                }
                if let Some(kind) = &kind_filter {
                    if &entry.kind != kind {
                        return false;
                    }
                }
                if let Some(member) = &member_filter {
                    if &entry.member_id != member {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|entry| entry.starts_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) fn create_schedule_entry(
    state: &IncidentsState,
    rotation: &OnCallRotation,
    input: CreateScheduleRequest,
) -> UnifiedResult<OnCallSchedule> {
    let member_id = require_text(&input.member_id, "member_id", 255)?;
    if !rotation.members.contains(&member_id) {
        return Err(UnifiedError::bad_request(format!(
            "User \"{member_id}\" is not a member of this rotation"
        )));
    }
    if input.ends_at <= input.starts_at {
        return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
    }
    match input.kind.as_str() {
        "regular" | "override" => {}
        other => {
            return Err(UnifiedError::bad_request(format!(
                "kind must be \"regular\" or \"override\", got \"{other}\""
            )))
        }
    }
    if let Some(overrides_id) = input.overrides_entry_id {
        let target = state
            .schedules
            .get(&overrides_id)
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.rotation_id == rotation.id)
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Schedule entry not found: {overrides_id}"))
            })?;
        if input.kind != "override" {
            return Err(UnifiedError::bad_request(
                "overrides_entry_id requires kind \"override\"",
            ));
        }
        if target.kind != "regular" {
            return Err(UnifiedError::bad_request(
                "overrides_entry_id must name a regular schedule entry",
            ));
        }
    }
    let now = Utc::now();
    let entry = OnCallSchedule {
        id: Uuid::now_v7(),
        rotation_id: rotation.id,
        member_id,
        starts_at: input.starts_at,
        ends_at: input.ends_at,
        kind: input.kind,
        overrides_entry_id: input.overrides_entry_id,
        created_at: now,
        updated_at: now,
    };
    state.schedules.insert(entry.id, entry.clone());
    Ok(entry)
}


pub(crate) async fn create_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateScheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OnCallSchedule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let rotation = load_rotation(&state, &path.into_inner())?;
        create_schedule_entry(&state, &rotation, body.into_inner())
    })();
    match result {
        Ok(entry) => created(entry),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn create_override(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateScheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OnCallSchedule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let rotation = load_rotation(&state, &path.into_inner())?;
        let mut input = body.into_inner();
        input.kind = "override".to_string();
        create_schedule_entry(&state, &rotation, input)
    })();
    match result {
        Ok(entry) => created(entry),
        Err(error) => err(error, debug),
    }
}
