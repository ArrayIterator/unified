//! incidents - handlers postmortems.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;
use super::{constants::*, types::*, inputs::*, state::*, helpers::*, handlers_incidents::*, handlers_rotations::*, handlers_policies::*, handlers_auto_open::*, module::*};



// Postmortem handlers
pub(crate) fn load_postmortem(state: &IncidentsState, incident_id: Uuid) -> UnifiedResult<IncidentPostmortem> {
    state
        .postmortems
        .iter()
        .map(|entry| entry.value().clone())
        .find(|postmortem| postmortem.incident_id == incident_id)
        .ok_or_else(|| {
            UnifiedError::not_found(format!("Postmortem not found for incident {incident_id}"))
        })
}


pub(crate) fn action_item_from(
    input: CreateActionItemRequest,
    incident: &Incident,
    state: &IncidentsState,
) -> UnifiedResult<PostmortemActionItem> {
    let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
    let assignee = match &input.assignee_id {
        Some(raw) => Some(require_text(raw, "assignee_id", 255)?),
        None => None,
    };
    let now = Utc::now();
    let item = PostmortemActionItem {
        id: Uuid::now_v7(),
        title: title.clone(),
        assignee_id: assignee.clone(),
        status: "open".to_string(),
        task_reference: None,
        created_at: now,
        updated_at: now,
    };
    // Action items become tasks via events: emit a task-create
    // request with the full task payload for the tasks module.
    // Honest boundary (see the module header): no listener in the
    // tasks module consumes `tasks.create.requested` yet, so the
    // item is stored here with status `open` and `task_reference`
    // stays empty until the tasks side picks the request up.
    state.emit(
        "tasks.create.requested",
        json!({
            "title": title,
            "description": format!(
                "Postmortem action item for incident \"{}\" ({})",
                incident.title, incident.id
            ),
            "assignee_id": assignee,
            "source_module": "incidents",
            "source_incident_id": incident.id.to_string(),
            "action_item_id": item.id.to_string(),
        }),
    );
    state.emit(
        "incidents.postmortem.action_item.created",
        json!({
            "incident_id": incident.id.to_string(),
            "action_item_id": item.id.to_string(),
            "title": title,
            "assignee_id": assignee,
        }),
    );
    Ok(item)
}


pub(crate) async fn get_postmortem(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<IncidentPostmortem> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        let incident = load_incident(&state, &path.into_inner())?;
        load_postmortem(&state, incident.id)
    })();
    match result {
        Ok(postmortem) => ok(postmortem),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn create_postmortem(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreatePostmortemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<IncidentPostmortem> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let incident = load_incident(&state, &path.into_inner())?;
        if !matches!(
            incident.status,
            IncidentStatus::Resolved | IncidentStatus::Closed
        ) {
            return Err(UnifiedError::conflict(format!(
                "A postmortem can only be created for a resolved or closed incident (status is \"{}\")",
                incident.status.as_str()
            )));
        }
        if load_postmortem(&state, incident.id).is_ok() {
            return Err(UnifiedError::conflict(
                "A postmortem already exists for this incident",
            ));
        }
        let input = body.into_inner();
        let summary = require_text(&input.summary, "summary", MAX_SUMMARY_LEN)?;
        let timeline_summary = match &input.timeline_summary {
            Some(raw) => Some(require_text(raw, "timeline_summary", MAX_SUMMARY_LEN)?),
            None => None,
        };
        let root_cause = match &input.root_cause {
            Some(raw) => Some(require_text(raw, "root_cause", MAX_ROOT_CAUSE_LEN)?),
            None => None,
        };
        let mut items: Vec<PostmortemActionItem> = Vec::new();
        for item_input in input.action_items {
            items.push(action_item_from(item_input, &incident, &state)?);
        }
        let now = Utc::now();
        let postmortem = IncidentPostmortem {
            id: Uuid::now_v7(),
            incident_id: incident.id,
            summary,
            timeline_summary,
            root_cause,
            action_items: items,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.postmortems.insert(postmortem.id, postmortem.clone());
        state.add_timeline(
            incident.id,
            &caller.actor,
            "system",
            format!("Postmortem created by {}", caller.actor),
        );
        state.emit(
            "incidents.postmortem.created",
            json!({
                "incident_id": incident.id.to_string(),
                "postmortem_id": postmortem.id.to_string(),
                "created_by": caller.actor,
                "action_items": postmortem.action_items.len(),
            }),
        );
        Ok(postmortem)
    })();
    match result {
        Ok(postmortem) => created(postmortem),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn add_action_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateActionItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<IncidentPostmortem> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let incident = load_incident(&state, &path.into_inner())?;
        let mut postmortem = load_postmortem(&state, incident.id)?;
        let item = action_item_from(body.into_inner(), &incident, &state)?;
        postmortem.action_items.push(item);
        postmortem.updated_at = Utc::now();
        state.postmortems.insert(postmortem.id, postmortem.clone());
        Ok(postmortem)
    })();
    match result {
        Ok(postmortem) => ok(postmortem),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn update_action_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateActionItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<IncidentPostmortem> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let (incident_id, item_id) = path.into_inner();
        let incident = load_incident(&state, &incident_id)?;
        let mut postmortem = load_postmortem(&state, incident.id)?;
        let item_uuid = parse_uuid(&item_id, "action item")?;
        let position = postmortem
            .action_items
            .iter()
            .position(|item| item.id == item_uuid)
            .ok_or_else(|| UnifiedError::not_found(format!("Action item not found: {item_id}")))?;
        let input = body.into_inner();
        if let Some(title) = input.title {
            postmortem.action_items[position].title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(assignee) = input.assignee_id {
            postmortem.action_items[position].assignee_id = match assignee {
                Some(raw) => Some(require_text(&raw, "assignee_id", 255)?),
                None => None,
            };
        }
        if let Some(status) = input.status {
            match status.as_str() {
                "open" | "in_progress" | "done" => {
                    postmortem.action_items[position].status = status;
                }
                other => {
                    return Err(UnifiedError::bad_request(format!(
                        "status must be \"open\", \"in_progress\", or \"done\", got \"{other}\""
                    )))
                }
            }
        }
        postmortem.action_items[position].updated_at = Utc::now();
        postmortem.updated_at = Utc::now();
        state.postmortems.insert(postmortem.id, postmortem.clone());
        Ok(postmortem)
    })();
    match result {
        Ok(postmortem) => ok(postmortem),
        Err(error) => err(error, debug),
    }
}
