//! incidents - handlers policies.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
use super::{constants::*, types::*, inputs::*, state::*, helpers::*, handlers_incidents::*, handlers_rotations::*, handlers_postmortems::*, handlers_auto_open::*, module::*};



// Escalation-policy handlers
pub(crate) async fn list_policies(
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
        let mut items: Vec<EscalationPolicy> = state
            .escalation_policies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|policy| search.is_empty() || policy.name.to_lowercase().contains(&search))
            .collect();
        items.sort_by_key(|policy| policy.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) fn validate_severities(values: &[String]) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for raw in values {
        let trimmed = raw.trim().to_lowercase();
        if IncidentSeverity::from_str(&trimmed).is_none() {
            return Err(UnifiedError::bad_request(format!(
                "applies_to_severities must contain only critical, high, medium, low - got \"{raw}\""
            )));
        }
        if !out.contains(&trimmed) {
            out.push(trimmed);
        }
    }
    Ok(out)
}


pub(crate) async fn create_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    body: web::Json<CreateEscalationPolicyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EscalationPolicy> = (|| {
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
        let severities = validate_severities(&input.applies_to_severities)?;
        validate_escalation_steps(&input.steps)?;
        let now = Utc::now();
        let policy = EscalationPolicy {
            id: Uuid::now_v7(),
            name,
            description,
            applies_to_severities: severities,
            steps: input.steps,
            created_at: now,
            updated_at: now,
        };
        state.escalation_policies.insert(policy.id, policy.clone());
        Ok(policy)
    })();
    match result {
        Ok(policy) => created(policy),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EscalationPolicy> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        load_policy(&state, &path.into_inner())
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn update_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateEscalationPolicyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EscalationPolicy> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let policy = load_policy(&state, &path.into_inner())?;
        let input = body.into_inner();
        let mut updated = policy.clone();
        if let Some(name) = input.name {
            updated.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = input.description {
            updated.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(severities) = input.applies_to_severities {
            updated.applies_to_severities = validate_severities(&severities)?;
        }
        if let Some(steps) = input.steps {
            validate_escalation_steps(&steps)?;
            updated.steps = steps;
        }
        updated.updated_at = Utc::now();
        state.escalation_policies.insert(updated.id, updated.clone());
        Ok(updated)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn delete_policy(
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
        let policy = load_policy(&state, &path.into_inner())?;
        if state
            .incidents
            .iter()
            .any(|entry| entry.value().escalation_policy_id == Some(policy.id))
        {
            return Err(UnifiedError::conflict(
                "Escalation policy is still used by incidents; detach it first",
            ));
        }
        state.escalation_policies.remove(&policy.id);
        Ok(json!({ "deleted": true, "policy_id": policy.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// Process due escalations: every active escalation whose
/// `next_escalation_at` has passed is advanced one step (page the
/// next target, emit `incidents.escalated`). This is the honest
/// boundary for time-based escalation: the module does not run a
/// background timer - the scheduler (or an operator) calls this
/// endpoint, exactly like the events module's reminder scheduler
/// note. Returns the count of escalations processed.
pub(crate) async fn run_escalations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let now = Utc::now();
        let due: Vec<IncidentEscalation> = state
            .escalations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.status == "active" && entry.next_escalation_at <= now)
            .collect();
        let mut processed = 0usize;
        let mut skipped = 0usize;
        for escalation in due {
            let Some(incident) = state
                .incidents
                .get(&escalation.incident_id)
                .map(|entry| entry.value().clone())
            else {
                skipped += 1;
                continue;
            };
            if matches!(
                incident.status,
                IncidentStatus::Resolved | IncidentStatus::Closed | IncidentStatus::Acknowledged
            ) {
                // Stale escalation: the incident moved on without
                // stopping it (acknowledge/resolve stop it, but a
                // direct status change through the listener path may
                // not have). Stop it now instead of paging.
                stop_escalation(&state, incident.id);
                skipped += 1;
                continue;
            }
            match escalate_one(&state, &incident, "escalation-runner") {
                Ok(_) => processed += 1,
                Err(_) => skipped += 1,
            }
        }
        Ok(json!({
            "processed": processed,
            "skipped": skipped,
            "checked_at": now.to_rfc3339(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
