//! quality module - HTTP handlers for CAPAs.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

use super::constants::CAPA_CLOSED;
use super::constants::CAPA_OPEN;
use super::constants::CAPA_VERIFIED;
use super::constants::MAX_DESCRIPTION_LEN;
use super::constants::MAX_REF_LEN;
use super::constants::MAX_TEXT_LEN;
use super::constants::MAX_TITLE_LEN;
use super::constants::PERM_QUALITY_MANAGE;
use super::constants::PERM_QUALITY_VIEW;
use super::constants::RESULT_FAIL;
use super::evaluation::valid_capa_transition;
use super::helpers::caller;
use super::helpers::created;
use super::helpers::err;
use super::helpers::ok;
use super::helpers::optional_text;
use super::helpers::paginate;
use super::helpers::require_in_list;
use super::helpers::require_permission;
use super::helpers::require_text;
use super::helpers::tenant_matches;
use super::state::QualityState;
use super::store::alert_for_inspection;
use super::store::load_alert;
use super::store::load_capa;
use super::store::load_inspection;
use super::types::Capa;
use super::types::CreateCapaInput;
use super::types::TransitionCapaInput;
use super::types::UpdateCapaInput;

/// A CAPA due date, when given, must be in the future — the same
/// rule on create and on update, so an update cannot smuggle in a
/// past date that create would have rejected.
pub(crate) fn validate_due_date(due_date: Option<DateTime<Utc>>) -> UnifiedResult<()> {
    if let Some(due) = due_date {
        if due < Utc::now() {
            return Err(UnifiedError::bad_request("due_date must be in the future"));
        }
    }
    Ok(())
}

/// Separation of duties for CAPA verification: the owner of a
/// CAPA must never verify their own fix — effectiveness has to be
/// confirmed by someone else. A CAPA carries no separate assignee
/// field; the owner is the accountable person, so the check is
/// `verified_by != owner_id`.
pub(crate) fn check_verify_separation(capa: &Capa, actor: &str) -> UnifiedResult<()> {
    if capa.owner_id == actor {
        return Err(UnifiedError::forbidden(
            "A CAPA owner cannot verify their own CAPA; another manager must verify it",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// CAPA handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_capas(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_VIEW)?;
        let params = query.into_inner();
        let state_filter = params.filters.get("state").cloned();
        let owner_filter = params.filters.get("owner_id").cloned();
        let inspection_filter = params.filters.get("inspection_id").cloned();
        let overdue_filter = params.filters.get("overdue").cloned();
        let now = Utc::now();
        let mut items: Vec<Capa> = state
            .capas
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if !tenant_matches(&row.tenant_id, &caller.tenant_id) {
                    return false;
                }
                if let Some(wanted) = &state_filter {
                    if &row.state != wanted {
                        return false;
                    }
                }
                if let Some(owner) = &owner_filter {
                    if &row.owner_id != owner {
                        return false;
                    }
                }
                if let Some(inspection_id) = &inspection_filter {
                    if row.inspection_id.to_string() != *inspection_id {
                        return false;
                    }
                }
                if let Some(overdue) = &overdue_filter {
                    let is_overdue = match row.due_date {
                        Some(due) => due < now && row.state != CAPA_CLOSED,
                        None => false,
                    };
                    if is_overdue != (overdue == "true") {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(json!(paginate(items, &params)))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_capa(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    body: web::Json<CreateCapaInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_MANAGE)?;
        let input = body.into_inner();
        let inspection = load_inspection(&state, &input.inspection_id, &caller.tenant_id)?;
        if inspection.result.as_deref() != Some(RESULT_FAIL) {
            return Err(UnifiedError::unprocessable(
                "A CAPA can only be opened for a failed inspection",
            ));
        }
        let alert_id = match &input.alert_id {
            Some(raw) => {
                let alert = load_alert(&state, raw, &caller.tenant_id)?;
                if alert.inspection_id != inspection.id {
                    return Err(UnifiedError::bad_request(
                        "alert_id does not belong to the given inspection",
                    ));
                }
                Some(alert.id)
            }
            None => alert_for_inspection(&state, inspection.id, &caller.tenant_id).map(|row| row.id),
        };
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)
            .unwrap_or_default();
        let root_cause = optional_text(&input.root_cause, "root_cause", MAX_TEXT_LEN)?;
        let owner_id = require_text(&input.owner_id, "owner_id", MAX_REF_LEN)?;
        validate_due_date(input.due_date)?;
        let now = Utc::now();
        let capa = Capa {
            id: Uuid::new_v4(),
            inspection_id: inspection.id,
            alert_id,
            title,
            description,
            root_cause,
            owner_id,
            due_date: input.due_date,
            state: CAPA_OPEN.to_string(),
            verification_notes: None,
            verified_by: None,
            verified_at: None,
            // The CAPA belongs to the same tenant as the failed
            // inspection it corrects.
            tenant_id: inspection.tenant_id.clone(),
            created_at: now,
            updated_at: now,
        };
        state.capas.insert(capa.id, capa.clone());
        state.emit(
            "quality.capa.created",
            json!({
                "capa_id": capa.id,
                "inspection_id": capa.inspection_id,
                "owner_id": capa.owner_id,
                "state": capa.state,
                "actor": caller.actor,
            }),
        );
        Ok(json!(capa))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_capa(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_VIEW)?;
        let capa = load_capa(&state, &path.into_inner(), &caller.tenant_id)?;
        let inspection = state
            .inspections
            .get(&capa.inspection_id)
            .map(|entry| entry.value().clone())
            .filter(|row| tenant_matches(&row.tenant_id, &caller.tenant_id));
        Ok(json!({ "capa": capa, "inspection": inspection }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_capa(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCapaInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_MANAGE)?;
        let input = body.into_inner();
        let mut capa = load_capa(&state, &path.into_inner(), &caller.tenant_id)?;
        if capa.state == CAPA_CLOSED {
            return Err(UnifiedError::unprocessable(
                "A closed CAPA cannot be edited",
            ));
        }
        if let Some(title) = &input.title {
            capa.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            capa.description = require_text(description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(root_cause) = &input.root_cause {
            capa.root_cause = Some(require_text(root_cause, "root_cause", MAX_TEXT_LEN)?);
        }
        if let Some(owner_id) = &input.owner_id {
            capa.owner_id = require_text(owner_id, "owner_id", MAX_REF_LEN)?;
        }
        if input.due_date.is_some() {
            // Same rule as create: a past due date is rejected.
            validate_due_date(input.due_date)?;
            capa.due_date = input.due_date;
        }
        capa.updated_at = Utc::now();
        state.capas.insert(capa.id, capa.clone());
        state.emit(
            "quality.capa.updated",
            json!({ "capa_id": capa.id, "state": capa.state, "actor": caller.actor }),
        );
        Ok(json!(capa))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn transition_capa(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TransitionCapaInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_MANAGE)?;
        let settings = state.settings();
        let input = body.into_inner();
        let to_state = require_in_list(&input.to_state, "to_state", &settings.capa_states)?;
        let mut capa = load_capa(&state, &path.into_inner(), &caller.tenant_id)?;
        if !valid_capa_transition(&capa.state, &to_state, settings.capa_verification_required) {
            return Err(UnifiedError::unprocessable(format!(
                "CAPA in state \"{}\" cannot move to \"{to_state}\"{}",
                capa.state,
                if settings.capa_verification_required {
                    " (verification is required: in_progress -> verified -> closed)"
                } else {
                    ""
                }
            )));
        }
        if to_state == CAPA_VERIFIED {
            // Separation of duties: the owner cannot verify their
            // own CAPA.
            check_verify_separation(&capa, &caller.actor)?;
            let notes = match &input.verification_notes {
                Some(raw) => require_text(raw, "verification_notes", MAX_TEXT_LEN)?,
                None => {
                    return Err(UnifiedError::bad_request(
                        "Verifying a CAPA needs verification_notes describing how effectiveness was checked",
                    ));
                }
            };
            capa.verification_notes = Some(notes);
            capa.verified_by = Some(caller.actor.clone());
            capa.verified_at = Some(Utc::now());
        }
        capa.state = to_state.clone();
        capa.updated_at = Utc::now();
        // Commit under the entry lock: re-check the live state (and
        // the separation-of-duties rule against the live owner)
        // before swapping in place, so two concurrent transitions
        // cannot both pass the check from the same loaded state.
        {
            let mut entry = state
                .capas
                .get_mut(&capa.id)
                .ok_or_else(|| UnifiedError::not_found(format!("CAPA not found: {}", capa.id)))?;
            if !valid_capa_transition(
                &entry.state,
                &to_state,
                settings.capa_verification_required,
            ) {
                return Err(UnifiedError::unprocessable(format!(
                    "CAPA in state \"{}\" cannot move to \"{to_state}\"",
                    entry.state
                )));
            }
            if to_state == CAPA_VERIFIED {
                check_verify_separation(&entry, &caller.actor)?;
            }
            *entry = capa.clone();
        }
        let event = if to_state == CAPA_CLOSED {
            "quality.capa.closed"
        } else {
            "quality.capa.transitioned"
        };
        state.emit(
            event,
            json!({
                "capa_id": capa.id,
                "inspection_id": capa.inspection_id,
                "state": capa.state,
                "actor": caller.actor,
            }),
        );
        Ok(json!(capa))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::check_verify_separation;
    use super::validate_due_date;
    use crate::modules::quality::Capa;
    use chrono::{Duration, Utc};
    use uuid::Uuid;

    fn capa(owner_id: &str) -> Capa {
        let now = Utc::now();
        Capa {
            id: Uuid::new_v4(),
            inspection_id: Uuid::new_v4(),
            alert_id: None,
            title: "Fix".to_string(),
            description: String::new(),
            root_cause: None,
            owner_id: owner_id.to_string(),
            due_date: None,
            state: "in_progress".to_string(),
            verification_notes: None,
            verified_by: None,
            verified_at: None,
            tenant_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn owner_cannot_verify_own_capa() {
        assert!(check_verify_separation(&capa("owner-1"), "owner-1").is_err());
    }

    #[test]
    fn other_manager_may_verify_capa() {
        assert!(check_verify_separation(&capa("owner-1"), "manager-2").is_ok());
    }

    #[test]
    fn due_date_must_be_future() {
        assert!(validate_due_date(Some(Utc::now() - Duration::hours(1))).is_err());
        assert!(validate_due_date(Some(Utc::now() + Duration::days(7))).is_ok());
        assert!(validate_due_date(None).is_ok());
    }
}
