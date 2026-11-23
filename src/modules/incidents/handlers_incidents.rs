//! incidents - handlers incidents.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
use super::{constants::*, types::*, inputs::*, state::*, helpers::*, handlers_rotations::*, handlers_policies::*, handlers_postmortems::*, handlers_auto_open::*, module::*};



// ---------------------------------------------------------------------------
// Incident handlers
// ---------------------------------------------------------------------------
pub(crate) async fn list_incidents(
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
        let status_filter = params.filters.get("status").cloned();
        let severity_filter = params.filters.get("severity").cloned();
        let responder_filter = params.filters.get("responder_id").cloned();
        let service_filter = params.filters.get("service_id").cloned();
        let source_filter = params.filters.get("source").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Incident> = state
            .incidents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|incident| {
                if let Some(status) = &status_filter {
                    if incident.status.as_str() != status {
                        return false;
                    }
                }
                if let Some(severity) = &severity_filter {
                    if incident.severity.as_str() != severity {
                        return false;
                    }
                }
                if let Some(responder) = &responder_filter {
                    if !incident.responders.iter().any(|id| id == responder)
                        && incident.current_responder.as_deref() != Some(responder.as_str())
                    {
                        return false;
                    }
                }
                if let Some(service) = &service_filter {
                    if !incident.affected_service_ids.iter().any(|id| id == service) {
                        return false;
                    }
                }
                if let Some(source) = &source_filter {
                    if &incident.source != source {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        incident.title,
                        incident.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|incident| incident.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn create_incident(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    body: web::Json<CreateIncidentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let affected = validate_user_ids(&input.affected_service_ids, "affected_service_ids", MAX_SERVICES)?;
        let responders = validate_user_ids(&input.responders, "responders", MAX_RESPONDERS)?;
        if let Some(policy_id) = input.escalation_policy_id {
            if state.escalation_policies.get(&policy_id).is_none() {
                return Err(UnifiedError::not_found(format!(
                    "Escalation policy not found: {policy_id}"
                )));
            }
        }
        if let Some(rotation_id) = input.rotation_id {
            if state.rotations.get(&rotation_id).is_none() {
                return Err(UnifiedError::not_found(format!("Rotation not found: {rotation_id}")));
            }
        }
        let now = Utc::now();
        let incident = Incident {
            id: Uuid::now_v7(),
            title,
            description,
            severity: input.severity.unwrap_or_default(),
            status: IncidentStatus::Open,
            affected_service_ids: affected,
            responders,
            current_responder: None,
            source: "manual".to_string(),
            source_service_id: None,
            escalation_policy_id: input.escalation_policy_id,
            rotation_id: input.rotation_id,
            created_by: caller.actor.clone(),
            acknowledged_by: None,
            acknowledged_at: None,
            resolved_at: None,
            closed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.incidents.insert(incident.id, incident.clone());
        state.add_timeline(
            incident.id,
            &caller.actor,
            "system",
            format!("Incident opened by {}", caller.actor),
        );
        start_escalation(&state, &incident);
        state.emit(
            "incidents.created",
            json!({
                "incident_id": incident.id.to_string(),
                "title": incident.title,
                "severity": incident.severity.as_str(),
                "status": incident.status.as_str(),
                "created_by": caller.actor,
                "affected_service_ids": incident.affected_service_ids,
            }),
        );
        // Notify every listed responder that the incident exists.
        for responder in &incident.responders {
            state.notify(
                "incidents.created",
                responder,
                &format!("Incident: {}", incident.title),
                &format!(
                    "A {} severity incident was opened: {}",
                    incident.severity.as_str(),
                    incident.title
                ),
                Some(format!("/api/v1/incidents/{}", incident.id)),
                "normal",
            );
        }
        Ok(incident)
    })();
    match result {
        Ok(incident) => created(incident),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_incident(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        load_incident(&state, &path.into_inner())
    })();
    match result {
        Ok(incident) => ok(incident),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn update_incident(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateIncidentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let incident = load_incident(&state, &path.into_inner())?;
        if incident.status == IncidentStatus::Closed {
            return Err(UnifiedError::conflict("A closed incident cannot be updated"));
        }
        let input = body.into_inner();
        let mut updated = incident.clone();
        if let Some(title) = input.title {
            updated.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = input.description {
            updated.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(severity) = input.severity {
            updated.severity = severity;
        }
        if let Some(services) = input.affected_service_ids {
            updated.affected_service_ids =
                validate_user_ids(&services, "affected_service_ids", MAX_SERVICES)?;
        }
        if let Some(policy) = input.escalation_policy_id {
            match policy {
                Some(policy_id) => {
                    if state.escalation_policies.get(&policy_id).is_none() {
                        return Err(UnifiedError::not_found(format!(
                            "Escalation policy not found: {policy_id}"
                        )));
                    }
                    updated.escalation_policy_id = Some(policy_id);
                }
                None => updated.escalation_policy_id = None,
            }
        }
        if let Some(rotation) = input.rotation_id {
            match rotation {
                Some(rotation_id) => {
                    if state.rotations.get(&rotation_id).is_none() {
                        return Err(UnifiedError::not_found(format!(
                            "Rotation not found: {rotation_id}"
                        )));
                    }
                    updated.rotation_id = Some(rotation_id);
                }
                None => updated.rotation_id = None,
            }
        }
        updated.updated_at = Utc::now();
        state.incidents.insert(updated.id, updated.clone());
        state.emit(
            "incidents.updated",
            json!({
                "incident_id": updated.id.to_string(),
                "updated_by": caller.actor,
            }),
        );
        Ok(updated)
    })();
    match result {
        Ok(incident) => ok(incident),
        Err(error) => err(error, debug),
    }
}


/// Apply a validated status transition, stamping the lifecycle
/// timestamps and writing a timeline entry. Shared by the
/// acknowledge endpoint, the status endpoint, and the Service
/// Monitor recovery listener.
pub(crate) fn apply_status_transition(
    state: &IncidentsState,
    incident: &mut Incident,
    next: IncidentStatus,
    actor: &str,
    note: Option<String>,
) -> UnifiedResult<()> {
    if !incident.status.allowed_next().contains(&next) {
        return Err(UnifiedError::conflict(format!(
            "Invalid status transition from \"{}\" to \"{}\"",
            incident.status.as_str(),
            next.as_str()
        )));
    }
    let previous = incident.status;
    let now = Utc::now();
    incident.status = next;
    incident.updated_at = now;
    match next {
        IncidentStatus::Acknowledged => {
            incident.acknowledged_by = Some(actor.to_string());
            incident.acknowledged_at = Some(now);
            stop_escalation(state, incident.id);
        }
        IncidentStatus::Resolved => {
            incident.resolved_at = Some(now);
            stop_escalation(state, incident.id);
        }
        IncidentStatus::Closed => {
            incident.closed_at = Some(now);
            stop_escalation(state, incident.id);
        }
        IncidentStatus::Investigating if previous == IncidentStatus::Resolved => {
            // Reopen: clear the resolution stamp.
            incident.resolved_at = None;
        }
        _ => {}
    }
    state.incidents.insert(incident.id, incident.clone());
    let mut message = format!(
        "Status changed from \"{}\" to \"{}\" by {}",
        previous.as_str(),
        next.as_str(),
        actor
    );
    if let Some(note) = note {
        message.push_str(&format!(" — {note}"));
    }
    state.add_timeline(incident.id, actor, "status_change", message);
    state.emit(
        "incidents.status_changed",
        json!({
            "incident_id": incident.id.to_string(),
            "previous_status": previous.as_str(),
            "status": next.as_str(),
            "actor_id": actor,
        }),
    );
    match next {
        IncidentStatus::Resolved => state.emit(
            "incidents.resolved",
            json!({
                "incident_id": incident.id.to_string(),
                "resolved_at": now.to_rfc3339(),
            }),
        ),
        IncidentStatus::Closed => state.emit(
            "incidents.closed",
            json!({
                "incident_id": incident.id.to_string(),
                "closed_at": now.to_rfc3339(),
            }),
        ),
        _ => {}
    }
    Ok(())
}


pub(crate) async fn acknowledge_incident(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let mut incident = load_incident(&state, &path.into_inner())?;
        // Acknowledgement also closes out any open page attempts
        // for the caller (or for the current responder).
        let now = Utc::now();
        for mut entry in state.page_attempts.iter_mut() {
            if entry.value().incident_id == incident.id
                && entry.value().status == "sent"
                && (entry.value().paged_user_id == caller.actor
                    || Some(entry.value().paged_user_id.clone()) == incident.current_responder)
            {
                entry.value_mut().status = "acknowledged".to_string();
                entry.value_mut().acknowledged_at = Some(now);
                entry.value_mut().updated_at = now;
            }
        }
        apply_status_transition(&state, &mut incident, IncidentStatus::Acknowledged, &caller.actor, None)?;
        state.emit(
            "incidents.acknowledged",
            json!({
                "incident_id": incident.id.to_string(),
                "acknowledged_by": caller.actor,
                "acknowledged_at": now.to_rfc3339(),
            }),
        );
        state.emit(
            "incidents.page.acknowledged",
            json!({
                "incident_id": incident.id.to_string(),
                "acknowledged_by": caller.actor,
            }),
        );
        Ok(incident)
    })();
    match result {
        Ok(incident) => ok(incident),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn change_incident_status(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ChangeStatusRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let mut incident = load_incident(&state, &path.into_inner())?;
        let input = body.into_inner();
        let note = match &input.note {
            Some(raw) => Some(require_text(raw, "note", MAX_MESSAGE_LEN)?),
            None => None,
        };
        apply_status_transition(&state, &mut incident, input.status, &caller.actor, note)?;
        Ok(incident)
    })();
    match result {
        Ok(incident) => ok(incident),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn page_incident(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let mut incident = load_incident(&state, &path.into_inner())?;
        if matches!(incident.status, IncidentStatus::Resolved | IncidentStatus::Closed) {
            return Err(UnifiedError::conflict(format!(
                "A {} incident cannot be paged",
                incident.status.as_str()
            )));
        }
        let input = body.into_inner();
        let channel = require_text(&input.channel, "channel", 64)?;
        // Explicit user first; otherwise the current on-call member
        // of the incident's rotation; otherwise the first responder.
        let target = match input.user_id {
            Some(user_id) => {
                let trimmed = user_id.trim().to_string();
                if trimmed.is_empty() {
                    return Err(UnifiedError::bad_request("user_id must not be empty"));
                }
                trimmed
            }
            None => {
                let rotation_member = incident.rotation_id.and_then(|rotation_id| {
                    state
                        .rotations
                        .get(&rotation_id)
                        .map(|entry| entry.value().clone())
                        .and_then(|rotation| on_call_member(&state, &rotation, Utc::now()))
                });
                match rotation_member {
                    Some(member) => member,
                    None => incident
                        .responders
                        .first()
                        .cloned()
                        .ok_or_else(|| UnifiedError::conflict(
                            "No responder to page: the incident has no rotation and no responders",
                        ))?,
                }
            }
        };
        let attempt = page_user(&state, &incident, &target, &channel);
        incident.current_responder = Some(target.clone());
        incident.updated_at = Utc::now();
        state.incidents.insert(incident.id, incident.clone());
        Ok(json!({
            "paged": true,
            "incident_id": incident.id.to_string(),
            "paged_user_id": target,
            "page_attempt_id": attempt.id.to_string(),
            "channel": channel,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn escalate_incident(
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
        let incident = load_incident(&state, &path.into_inner())?;
        escalate_one(&state, &incident, &caller.actor)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// Run one escalation step for `incident`: move its escalation
/// state to the next step, page the step's target, and emit
/// `incidents.escalated`. Returns an error when the incident has
/// no active escalation (no policy, already acknowledged, or all
/// steps exhausted).
pub(crate) fn escalate_one(state: &IncidentsState, incident: &Incident, actor: &str) -> UnifiedResult<Value> {
    if matches!(
        incident.status,
        IncidentStatus::Resolved | IncidentStatus::Closed
    ) {
        return Err(UnifiedError::conflict(format!(
            "A {} incident cannot be escalated",
            incident.status.as_str()
        )));
    }
    let Some(policy_id) = incident.escalation_policy_id else {
        return Err(UnifiedError::conflict(
            "Incident has no escalation policy",
        ));
    };
    let policy = state
        .escalation_policies
        .get(&policy_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Escalation policy not found: {policy_id}")))?;
    let mut escalation = state
        .escalations
        .iter_mut()
        .map(|entry| entry.value().clone())
        .find(|entry| entry.incident_id == incident.id && entry.status == "active")
        .ok_or_else(|| {
            UnifiedError::conflict("Incident has no active escalation (already acknowledged or all steps exhausted)")
        })?;
    if escalation.step_index >= policy.steps.len() {
        return Err(UnifiedError::conflict("All escalation steps are exhausted"));
    }
    let step = &policy.steps[escalation.step_index];
    let target = escalation_target_user(state, incident, step).ok_or_else(|| {
        UnifiedError::conflict("Escalation step has no reachable target user")
    })?;
    let attempt = page_user(state, incident, &target, "notification");
    let now = Utc::now();
    let step_index = escalation.step_index;
    escalation.step_index += 1;
    if escalation.step_index >= policy.steps.len() {
        escalation.status = "completed".to_string();
    } else {
        let next = &policy.steps[escalation.step_index];
        escalation.next_escalation_at =
            now + chrono::Duration::minutes(next.after_minutes - step.after_minutes);
    }
    escalation.updated_at = now;
    state.escalations.insert(escalation.id, escalation.clone());
    let mut updated = incident.clone();
    updated.current_responder = Some(target.clone());
    updated.updated_at = now;
    state.incidents.insert(updated.id, updated);
    state.add_timeline(
        incident.id,
        actor,
        "escalation",
        format!(
            "Escalated to step {} (target {target}) by {actor}",
            step_index + 1
        ),
    );
    state.emit(
        "incidents.escalated",
        json!({
            "incident_id": incident.id.to_string(),
            "policy_id": policy.id.to_string(),
            "step": step_index + 1,
            "paged_user_id": target,
            "page_attempt_id": attempt.id.to_string(),
            "escalated_by": actor,
        }),
    );
    Ok(json!({
        "escalated": true,
        "incident_id": incident.id.to_string(),
        "step": step_index + 1,
        "paged_user_id": target,
        "page_attempt_id": attempt.id.to_string(),
        "escalation_status": escalation.status,
    }))
}


pub(crate) async fn list_timeline(
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
        let incident = load_incident(&state, &path.into_inner())?;
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let actor_filter = params.filters.get("actor_id").cloned();
        let mut items = timeline_of(&state, incident.id);
        if let Some(kind) = &kind_filter {
            items.retain(|entry| &entry.kind == kind);
        }
        if let Some(actor) = &actor_filter {
            items.retain(|entry| &entry.actor_id == actor);
        }
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn add_timeline_note(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddTimelineRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let incident = load_incident(&state, &path.into_inner())?;
        let input = body.into_inner();
        let message = require_text(&input.message, "message", MAX_MESSAGE_LEN)?;
        state.add_timeline(incident.id, &caller.actor, "note", message.clone());
        state.emit(
            "incidents.timeline.added",
            json!({
                "incident_id": incident.id.to_string(),
                "actor_id": caller.actor,
                "kind": "note",
            }),
        );
        Ok(json!({
            "added": true,
            "incident_id": incident.id.to_string(),
            "kind": "note",
            "message": message,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn add_responder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddResponderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let mut incident = load_incident(&state, &path.into_inner())?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", 255)?;
        if incident.responders.contains(&user_id) {
            return Err(UnifiedError::conflict(format!(
                "User \"{user_id}\" is already a responder"
            )));
        }
        if incident.responders.len() >= MAX_RESPONDERS {
            return Err(UnifiedError::conflict(format!(
                "An incident may have at most {MAX_RESPONDERS} responders"
            )));
        }
        incident.responders.push(user_id.clone());
        incident.updated_at = Utc::now();
        state.incidents.insert(incident.id, incident.clone());
        state.add_timeline(
            incident.id,
            &caller.actor,
            "responder_action",
            format!("Added responder {user_id}"),
        );
        Ok(incident)
    })();
    match result {
        Ok(incident) => ok(incident),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn remove_responder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let (incident_id, user_id) = path.into_inner();
        let mut incident = load_incident(&state, &incident_id)?;
        let user_id = user_id.trim().to_string();
        if !incident.responders.contains(&user_id) {
            return Err(UnifiedError::not_found(format!(
                "User \"{user_id}\" is not a responder of this incident"
            )));
        }
        incident.responders.retain(|id| id != &user_id);
        if incident.current_responder.as_deref() == Some(user_id.as_str()) {
            incident.current_responder = None;
        }
        incident.updated_at = Utc::now();
        state.incidents.insert(incident.id, incident.clone());
        state.add_timeline(
            incident.id,
            &caller.actor,
            "responder_action",
            format!("Removed responder {user_id}"),
        );
        Ok(incident)
    })();
    match result {
        Ok(incident) => ok(incident),
        Err(error) => err(error, debug),
    }
}
