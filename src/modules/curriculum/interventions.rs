//! Curriculum module — competency-based school curricula. — Intervention handlers (remedial + enrichment).
//!
//! Split from `src/modules/curriculum.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::assessment_domains::*;
use super::basic_competencies::*;
use super::constants::*;
use super::core_competencies::*;
use super::frameworks::*;
use super::grade_component_mappings::*;
use super::grade_descriptors::*;
use super::helpers::*;
use super::hooks::*;
use super::inputs::*;
use super::lesson_plans::*;
use super::levels::*;
use super::module::*;
use super::report_card_entries::*;
use super::report_cards::*;
use super::settings::*;
use super::state::*;
use super::structures::*;
use super::subjects::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn list_interventions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let items: Vec<Intervention> =
            state.interventions.iter().map(|entry| entry.value().clone()).collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

pub(super) async fn create_intervention(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<InterventionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let input = body.into_inner();
        let settings = state.settings();
        let student_ref = require_text(
            input.student_ref.as_deref().unwrap_or(""),
            "student_ref",
            MAX_REF_LEN,
        )?;
        check_student_reference(&ctx, &state, &student_ref)?;
        let basic_competency_id =
            required_uuid(&input.basic_competency_id, "basic competency")?;
        get_record(
            &state.basic_competencies,
            &basic_competency_id,
            "Basic competency",
        )?;
        let kind = match &input.kind {
            Some(raw) => require_in_list(raw, "kind", &settings.intervention_kinds)?,
            None => settings
                .intervention_kinds
                .first()
                .cloned()
                .unwrap_or_else(|| "remedial".to_string()),
        };
        let status = match &input.status {
            Some(raw) => require_in_list(raw, "status", &settings.intervention_statuses)?,
            None => settings
                .intervention_statuses
                .first()
                .cloned()
                .unwrap_or_else(|| "scheduled".to_string()),
        };
        let scheduled_at = match &input.scheduled_at {
            Some(raw) => {
                let parsed = DateTime::parse_from_rfc3339(raw.trim()).map_err(|_| {
                    UnifiedError::bad_request(
                        "scheduled_at must be an RFC 3339 date-time",
                    )
                })?;
                Some(parsed.with_timezone(&Utc))
            }
            None => None,
        };
        let intervention = Intervention {
            id: Uuid::new_v4(),
            student_ref,
            basic_competency_id,
            kind,
            status,
            teacher_id: require_text(
                input.teacher_id.as_deref().unwrap_or(&caller.actor),
                "teacher_id",
                MAX_REF_LEN,
            )?,
            scheduled_at,
            completed_at: None,
            notes: optional_text(&input.notes, "notes", MAX_TEXT_LEN)?.unwrap_or_default(),
            created_at: now(),
            updated_at: now(),
        };
        state.interventions.insert(intervention.id, intervention.clone());
        state.emit(
            "curriculum.intervention.created",
            json!({ "intervention": intervention }),
        );
        Ok(json!(intervention))
    })();
    json::respond(result, debug)
}

pub(super) async fn get_intervention(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "intervention")?;
        Ok(json!(get_record(&state.interventions, &id, "Intervention")?))
    })();
    json::respond(result, debug)
}

pub(super) async fn update_intervention(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<InterventionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let id = parse_uuid(&path.into_inner(), "intervention")?;
        let input = body.into_inner();
        let settings = state.settings();
        let mut intervention = get_record(&state.interventions, &id, "Intervention")?;
        if let Some(raw) = &input.kind {
            intervention.kind = require_in_list(raw, "kind", &settings.intervention_kinds)?;
        }
        if let Some(raw) = &input.status {
            intervention.status =
                require_in_list(raw, "status", &settings.intervention_statuses)?;
        }
        if let Some(raw) = &input.scheduled_at {
            let parsed = DateTime::parse_from_rfc3339(raw.trim()).map_err(|_| {
                UnifiedError::bad_request("scheduled_at must be an RFC 3339 date-time")
            })?;
            intervention.scheduled_at = Some(parsed.with_timezone(&Utc));
        }
        if let Some(raw) = &input.notes {
            intervention.notes = require_text(raw, "notes", MAX_TEXT_LEN)?;
        }
        intervention.updated_at = now();
        state.interventions.insert(intervention.id, intervention.clone());
        state.emit(
            "curriculum.intervention.updated",
            json!({ "intervention": intervention }),
        );
        Ok(json!(intervention))
    })();
    json::respond(result, debug)
}

pub(super) async fn delete_intervention(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let id = parse_uuid(&path.into_inner(), "intervention")?;
        get_record(&state.interventions, &id, "Intervention")?;
        state.interventions.remove(&id);
        state.emit(
            "curriculum.intervention.deleted",
            json!({ "intervention_id": id }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

pub(super) fn set_intervention_status(
    ctx: &AppContext,
    state: &CurriculumState,
    caller: &Caller,
    id: Uuid,
    status: &str,
    event: &str,
    mark_completed: bool,
) -> UnifiedResult<Intervention> {
    require_teach(ctx, &caller.role)?;
    let settings = state.settings();
    let status = require_in_list(status, "status", &settings.intervention_statuses)?;
    let mut intervention = get_record(&state.interventions, &id, "Intervention")?;
    // `completed` and `cancelled` are terminal: a finished
    // intervention can never change status again, so completing an
    // already-cancelled one cannot resurrect it as completed (with
    // a fresh `completed_at`), and cancelling a completed one is
    // refused. Re-completing or re-cancelling is likewise refused
    // with 409 instead of re-emitting the event.
    if intervention.status == "completed" || intervention.status == "cancelled" {
        return Err(UnifiedError::conflict(format!(
            "An intervention with status \"{}\" cannot change status",
            intervention.status
        )));
    }
    if intervention.status == status {
        return Err(UnifiedError::conflict(format!(
            "Intervention is already {status}"
        )));
    }
    intervention.status = status;
    if mark_completed {
        intervention.completed_at = Some(now());
    }
    intervention.updated_at = now();
    state.interventions.insert(intervention.id, intervention.clone());
    state.emit(event, json!({ "intervention": intervention }));
    Ok(intervention)
}

pub(super) async fn complete_intervention(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let id = parse_uuid(&path.into_inner(), "intervention")?;
        let intervention = set_intervention_status(
            &ctx,
            &state,
            &caller,
            id,
            "completed",
            "curriculum.intervention.completed",
            true,
        )?;
        Ok(json!(intervention))
    })();
    json::respond(result, debug)
}

pub(super) async fn cancel_intervention(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let id = parse_uuid(&path.into_inner(), "intervention")?;
        let intervention = set_intervention_status(
            &ctx,
            &state,
            &caller,
            id,
            "cancelled",
            "curriculum.intervention.cancelled",
            false,
        )?;
        Ok(json!(intervention))
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;

    /// A context whose `role.check` handler allows every permission.
    fn allowing_context() -> AppContext {
        let ctx = AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        );
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        ctx
    }

    fn teacher() -> Caller {
        Caller {
            actor: "teacher-1".to_string(),
            role: "teacher".to_string(),
        }
    }

    fn intervention_with_status(state: &CurriculumState, status: &str) -> Uuid {
        let id = Uuid::new_v4();
        let timestamp = now();
        state.interventions.insert(
            id,
            Intervention {
                id,
                student_ref: "student-1".to_string(),
                basic_competency_id: Uuid::new_v4(),
                kind: "remedial".to_string(),
                status: status.to_string(),
                teacher_id: "teacher-1".to_string(),
                scheduled_at: None,
                completed_at: if status == "completed" {
                    Some(timestamp)
                } else {
                    None
                },
                notes: String::new(),
                created_at: timestamp,
                updated_at: timestamp,
            },
        );
        id
    }

    #[test]
    fn complete_scheduled_intervention_sets_completed_at() {
        let ctx = allowing_context();
        let state = CurriculumState::default();
        let id = intervention_with_status(&state, "scheduled");
        let done = set_intervention_status(
            &ctx,
            &state,
            &teacher(),
            id,
            "completed",
            "curriculum.intervention.completed",
            true,
        )
        .unwrap();
        assert_eq!(done.status, "completed");
        assert!(done.completed_at.is_some());
    }

    #[test]
    fn recomplete_completed_intervention_is_rejected_and_keeps_completed_at() {
        let ctx = allowing_context();
        let state = CurriculumState::default();
        let id = intervention_with_status(&state, "completed");
        let first_completed_at = state
            .interventions
            .get(&id)
            .unwrap()
            .completed_at;
        let err = set_intervention_status(
            &ctx,
            &state,
            &teacher(),
            id,
            "completed",
            "curriculum.intervention.completed",
            true,
        )
        .unwrap_err();
        assert_eq!(err.code, 409);
        let stored = state.interventions.get(&id).unwrap();
        assert_eq!(stored.status, "completed");
        assert_eq!(stored.completed_at, first_completed_at);
    }

    #[test]
    fn cancel_completed_intervention_is_rejected() {
        let ctx = allowing_context();
        let state = CurriculumState::default();
        let id = intervention_with_status(&state, "completed");
        let err = set_intervention_status(
            &ctx,
            &state,
            &teacher(),
            id,
            "cancelled",
            "curriculum.intervention.cancelled",
            false,
        )
        .unwrap_err();
        assert_eq!(err.code, 409);
        assert_eq!(state.interventions.get(&id).unwrap().status, "completed");
    }

    #[test]
    fn cancel_scheduled_intervention_succeeds() {
        let ctx = allowing_context();
        let state = CurriculumState::default();
        let id = intervention_with_status(&state, "scheduled");
        let done = set_intervention_status(
            &ctx,
            &state,
            &teacher(),
            id,
            "cancelled",
            "curriculum.intervention.cancelled",
            false,
        )
        .unwrap();
        assert_eq!(done.status, "cancelled");
        assert!(done.completed_at.is_none());
    }

    #[test]
    fn cancelled_intervention_cannot_be_completed_or_recancelled() {
        let ctx = allowing_context();
        let state = CurriculumState::default();
        let id = intervention_with_status(&state, "cancelled");
        let err = set_intervention_status(
            &ctx,
            &state,
            &teacher(),
            id,
            "completed",
            "curriculum.intervention.completed",
            true,
        )
        .unwrap_err();
        assert_eq!(err.code, 409);
        let err = set_intervention_status(
            &ctx,
            &state,
            &teacher(),
            id,
            "cancelled",
            "curriculum.intervention.cancelled",
            false,
        )
        .unwrap_err();
        assert_eq!(err.code, 409);
        let stored = state.interventions.get(&id).unwrap();
        assert_eq!(stored.status, "cancelled");
        assert!(stored.completed_at.is_none());
    }
}
