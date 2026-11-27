//! quality module - HTTP handlers for inspections.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use super::constants::ALERT_OPEN;
use super::constants::INSP_CANCELLED;
use super::constants::INSP_COMPLETED;
use super::constants::INSP_REQUESTED;
use super::constants::MAX_REF_LEN;
use super::constants::MAX_SLUG_LEN;
use super::constants::MAX_TEXT_LEN;
use super::constants::PERM_QUALITY_VIEW;
use super::constants::RESULT_CONDITIONAL;
use super::constants::RESULT_FAIL;
use super::constants::RESULT_PASS;
use super::evaluation::evaluate_measurement;
use super::evaluation::valid_inspection_transition;
use super::helpers::caller;
use super::helpers::created;
use super::helpers::err;
use super::helpers::is_supervisor;
use super::helpers::ok;
use super::helpers::optional_text;
use super::helpers::paginate;
use super::helpers::parse_uuid;
use super::helpers::require_all_in_list;
use super::helpers::require_inspect;
use super::helpers::require_permission;
use super::helpers::require_text;
use super::helpers::tenant_matches;
use super::state::QualityState;
use super::store::alert_for_inspection;
use super::store::load_inspection;
use super::store::load_point;
use super::types::CreateInspectionInput;
use super::types::Inspection;
use super::types::Measurement;
use super::types::MeasurementInput;
use super::types::QualityAlert;
use super::types::QualityPoint;
use super::types::SubmitInspectionInput;

// Inspection handlers

pub(crate) async fn list_inspections(
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
        let point_filter = params.filters.get("point_id").cloned();
        let target_filter = params.filters.get("target_ref").cloned();
        let kind_filter = params.filters.get("target_kind").cloned();
        let status_filter = params.filters.get("status").cloned();
        let result_filter = params.filters.get("result").cloned();
        let inspector_filter = params.filters.get("inspector_id").cloned();
        let mut items: Vec<Inspection> = state
            .inspections
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if !tenant_matches(&row.tenant_id, &caller.tenant_id) {
                    return false;
                }
                if let Some(point_id) = &point_filter {
                    if row.point_id.to_string() != *point_id {
                        return false;
                    }
                }
                if let Some(target) = &target_filter {
                    if &row.target_ref != target {
                        return false;
                    }
                }
                if let Some(kind) = &kind_filter {
                    if &row.target_kind != kind {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &row.status != status {
                        return false;
                    }
                }
                if let Some(result) = &result_filter {
                    if row.result.as_deref() != Some(result.as_str()) {
                        return false;
                    }
                }
                if let Some(inspector) = &inspector_filter {
                    if row.inspector_id.as_deref() != Some(inspector.as_str()) {
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

/// Create a requested inspection. Shared by the HTTP handler and
/// by the ERP event listeners, so an event and an API call create
/// exactly the same record. `tenant_id` is the owning tenant when
/// known (the HTTP caller, or the tenant carried by the event);
/// `None` marks an event-created record with no tenant, which
/// stays visible until it is backfilled (see `tenant_matches`).
pub(crate) fn request_inspection(
    state: &QualityState,
    point: &QualityPoint,
    target_ref: String,
    target_kind: String,
    inspector_id: Option<String>,
    company_id: Option<String>,
    tenant_id: Option<String>,
) -> Inspection {
    let now = Utc::now();
    let inspection = Inspection {
        id: Uuid::new_v4(),
        point_id: point.id,
        target_ref,
        target_kind,
        inspector_id,
        status: INSP_REQUESTED.to_string(),
        result: None,
        measurements: Vec::new(),
        failure_causes: Vec::new(),
        notes: None,
        evidence_refs: Vec::new(),
        inspected_at: None,
        company_id,
        tenant_id,
        created_at: now,
        updated_at: now,
    };
    state.inspections.insert(inspection.id, inspection.clone());
    state.emit(
        "quality.inspection.requested",
        json!({
            "inspection_id": inspection.id,
            "point_id": point.id,
            "target_ref": inspection.target_ref,
            "target_kind": inspection.target_kind,
            "tenant_id": inspection.tenant_id,
        }),
    );
    inspection
}

/// Pure assignment rule for submissions, kept separate from the
/// handler so it is unit-testable: the assigned inspector may
/// submit; a supervisor (quality.manage holder) may submit on
/// their behalf; anyone else may not. An unassigned inspection
/// may be submitted by any inspector, who then becomes the
/// assigned inspector.
pub(crate) fn may_submit_inspection(
    assigned_inspector: &Option<String>,
    actor: &str,
    is_supervisor: bool,
) -> bool {
    match assigned_inspector {
        Some(assigned) => assigned == actor || is_supervisor,
        None => true,
    }
}

pub(crate) async fn create_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    body: web::Json<CreateInspectionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_inspect(&ctx, &caller.role)?;
        let input = body.into_inner();
        let point = load_point(&state, &input.point_id, &caller.tenant_id)?;
        if !point.active {
            return Err(UnifiedError::conflict(
                "Quality point is deactivated; inspections cannot be requested against it",
            ));
        }
        let target_ref = require_text(&input.target_ref, "target_ref", MAX_REF_LEN)?;
        let target_kind = require_text(&input.target_kind, "target_kind", MAX_SLUG_LEN)?;
        let inspector_id = match &input.inspector_id {
            Some(raw) => Some(require_text(raw, "inspector_id", MAX_REF_LEN)?),
            None => Some(caller.actor.clone()),
        };
        let company_id = optional_text(&input.company_id, "company_id", MAX_REF_LEN)?;
        let inspection = request_inspection(
            &state,
            &point,
            target_ref,
            target_kind,
            inspector_id,
            company_id,
            Some(caller.tenant_id.clone()),
        );
        Ok(json!(inspection))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_VIEW)?;
        let inspection = load_inspection(&state, &path.into_inner(), &caller.tenant_id)?;
        let point = state
            .points
            .get(&inspection.point_id)
            .map(|entry| entry.value().clone())
            .filter(|row| tenant_matches(&row.tenant_id, &caller.tenant_id));
        let alert = alert_for_inspection(&state, inspection.id, &caller.tenant_id);
        Ok(json!({ "inspection": inspection, "point": point, "alert": alert }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn submit_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SubmitInspectionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_inspect(&ctx, &caller.role)?;
        let settings = state.settings();
        let input = body.into_inner();
        let mut inspection = load_inspection(&state, &path.into_inner(), &caller.tenant_id)?;
        // Only the assigned inspector (or a supervisor) may submit
        // this inspection. Any quality.inspect holder used to be
        // able to submit anyone's inspection, and the stored
        // inspector was then silently overwritten with the caller.
        let supervisor = is_supervisor(&ctx, &caller.role);
        if !may_submit_inspection(&inspection.inspector_id, &caller.actor, supervisor) {
            return Err(UnifiedError::forbidden(
                "Only the assigned inspector (or a supervisor) may submit this inspection",
            ));
        }
        if !valid_inspection_transition(&inspection.status, INSP_COMPLETED) {
            return Err(UnifiedError::unprocessable(format!(
                "Inspection in status \"{}\" cannot be submitted",
                inspection.status
            )));
        }
        if input.measurements.len() > settings.max_measurements_per_inspection {
            return Err(UnifiedError::bad_request(format!(
                "At most {} measurements per submission",
                settings.max_measurements_per_inspection
            )));
        }
        if input.evidence_refs.len() > settings.max_evidence_refs {
            return Err(UnifiedError::bad_request(format!(
                "At most {} evidence references per inspection",
                settings.max_evidence_refs
            )));
        }
        let point = load_point(&state, &inspection.point_id.to_string(), &caller.tenant_id)?;
        let criteria = state.criteria_for_point(point.id);
        let mut by_criterion: HashMap<Uuid, &MeasurementInput> = HashMap::new();
        for item in &input.measurements {
            let criterion_id = parse_uuid(&item.criterion_id, "criterion")?;
            if by_criterion.insert(criterion_id, item).is_some() {
                return Err(UnifiedError::bad_request(format!(
                    "Duplicate measurement for criterion {criterion_id}"
                )));
            }
        }
        let mut measurements: Vec<Measurement> = Vec::new();
        let mut auto_causes: Vec<String> = Vec::new();
        for criterion in &criteria {
            let evaluated =
                evaluate_measurement(criterion, by_criterion.get(&criterion.id).copied())?;
            if let Some((measurement, cause)) = evaluated {
                if let Some(cause) = cause {
                    if !auto_causes.contains(&cause) {
                        auto_causes.push(cause);
                    }
                }
                measurements.push(measurement);
            }
        }
        // Measurements naming a criterion that does not belong to
        // this point are rejected - they cannot be evaluated.
        for item in &input.measurements {
            let criterion_id = parse_uuid(&item.criterion_id, "criterion")?;
            if !criteria.iter().any(|row| row.id == criterion_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Criterion {criterion_id} does not belong to quality point {}",
                    point.id
                )));
            }
        }
        let any_fail = measurements.iter().any(|row| !row.passed);
        let notes = optional_text(&input.notes, "notes", MAX_TEXT_LEN)?;
        let mut failure_causes = require_all_in_list(
            &input.failure_causes,
            "failure_cause",
            &settings.failure_causes,
        )?;
        for cause in auto_causes {
            if !failure_causes.contains(&cause) {
                failure_causes.push(cause);
            }
        }
        let evidence_refs: Vec<String> = input
            .evidence_refs
            .iter()
            .map(|value| require_text(value, "evidence_ref", MAX_REF_LEN))
            .collect::<UnifiedResult<Vec<String>>>()?;
        // Decide the result. A failure anywhere is a failure; a
        // conditional result is only available when everything
        // passed and the inspector asked for it with a written
        // justification. The result vocabulary comes from settings.
        let result = if any_fail {
            RESULT_FAIL.to_string()
        } else if input.request_conditional {
            match &notes {
                Some(text) if !text.trim().is_empty() => RESULT_CONDITIONAL.to_string(),
                _ => {
                    return Err(UnifiedError::bad_request(
                        "A conditional result needs notes explaining the reservation",
                    ));
                }
            }
        } else {
            RESULT_PASS.to_string()
        };
        for wanted in [RESULT_PASS, RESULT_FAIL, RESULT_CONDITIONAL] {
            if result == wanted
                && !settings
                    .inspection_results
                    .iter()
                    .any(|item| item == wanted)
            {
                return Err(UnifiedError::unprocessable(format!(
                    "Result \"{wanted}\" is not in the configured inspection_results list"
                )));
            }
        }
        // Fill in the completed inspection on the local copy. The
        // assigned inspector stays recorded; only an unassigned
        // inspection takes the submitter as its inspector.
        inspection.measurements = measurements;
        inspection.failure_causes = failure_causes;
        inspection.notes = notes;
        inspection.evidence_refs = evidence_refs;
        inspection.status = INSP_COMPLETED.to_string();
        inspection.result = Some(result.clone());
        if inspection.inspector_id.is_none() {
            inspection.inspector_id = Some(caller.actor.clone());
        }
        inspection.inspected_at = Some(Utc::now());
        inspection.updated_at = Utc::now();
        // Decide the alert / block outcome BEFORE anything is
        // stored or emitted, so the single completion event below
        // already carries the final block state.
        let mut alert: Option<QualityAlert> = None;
        let mut blocked = false;
        if result == RESULT_FAIL && point.on_fail_policy != "none" {
            blocked = point.on_fail_policy == "block";
            let now = Utc::now();
            alert = Some(QualityAlert {
                id: Uuid::new_v4(),
                inspection_id: inspection.id,
                point_id: point.id,
                target_ref: inspection.target_ref.clone(),
                target_kind: inspection.target_kind.clone(),
                severity: point.alert_severity.clone(),
                status: ALERT_OPEN.to_string(),
                message: format!(
                    "Inspection {} failed quality point \"{}\" ({}). Causes: {}.",
                    inspection.id,
                    point.name,
                    point.scope,
                    if inspection.failure_causes.is_empty() {
                        "unspecified".to_string()
                    } else {
                        inspection.failure_causes.join(", ")
                    }
                ),
                blocked,
                acknowledged_by: None,
                resolved_by: None,
                resolved_at: None,
                tenant_id: inspection.tenant_id.clone(),
                created_at: now,
                updated_at: now,
            });
        }
        // Commit under the entry lock: re-check the status (and
        // the assignment) against the live record, then swap it in
        // place. Two concurrent submits can no longer both pass
        // the check - the second finds the record already
        // completed and fails instead of completing twice and
        // raising a duplicate alert. The lock is released before
        // any event is emitted, so a listener reading the store
        // never blocks on this entry.
        {
            let mut entry = state
                .inspections
                .get_mut(&inspection.id)
                .ok_or_else(|| {
                    UnifiedError::not_found(format!("Inspection not found: {}", inspection.id))
                })?;
            if !valid_inspection_transition(&entry.status, INSP_COMPLETED) {
                return Err(UnifiedError::unprocessable(format!(
                    "Inspection in status \"{}\" cannot be submitted",
                    entry.status
                )));
            }
            if !may_submit_inspection(&entry.inspector_id, &caller.actor, supervisor) {
                return Err(UnifiedError::forbidden(
                    "Only the assigned inspector (or a supervisor) may submit this inspection",
                ));
            }
            *entry = inspection.clone();
        }
        // A failed inspection raises an alert under the point's
        // fail policy, and blocks the related record when the
        // policy is `block`. The alert (not a direct call) is how
        // the owning module learns the record must not proceed.
        if let Some(raised) = &alert {
            state.alerts.insert(raised.id, raised.clone());
            state.emit(
                "quality.alert.raised",
                json!({
                    "alert_id": raised.id,
                    "inspection_id": inspection.id,
                    "point_id": point.id,
                    "target_ref": raised.target_ref,
                    "target_kind": raised.target_kind,
                    "severity": raised.severity,
                    "blocked": blocked,
                }),
            );
            state.notify(
                "quality_alert",
                &caller.actor,
                "Quality inspection failed",
                &format!(
                    "Inspection of {} failed quality point \"{}\".{}",
                    inspection.target_ref,
                    point.name,
                    if blocked {
                        " The related record is blocked until the alert is resolved."
                    } else {
                        ""
                    }
                ),
                None,
            );
        }
        // Exactly one completion event, carrying the final block
        // state and the alert id when one was raised. Listeners
        // (manufacturing records the outcome and the block on the
        // work order) must never see a `blocked: false` event for
        // an inspection that in fact blocked its record.
        state.emit(
            "quality.inspection.completed",
            json!({
                "inspection_id": inspection.id,
                "point_id": point.id,
                "target_ref": inspection.target_ref,
                "target_kind": inspection.target_kind,
                "result": result,
                "inspector_id": inspection.inspector_id,
                "blocked": blocked,
                "alert_id": alert.as_ref().map(|row| row.id),
            }),
        );
        Ok(json!({ "inspection": inspection, "alert": alert }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn cancel_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_inspect(&ctx, &caller.role)?;
        let mut inspection = load_inspection(&state, &path.into_inner(), &caller.tenant_id)?;
        if !valid_inspection_transition(&inspection.status, INSP_CANCELLED) {
            return Err(UnifiedError::unprocessable(format!(
                "Inspection in status \"{}\" cannot be cancelled",
                inspection.status
            )));
        }
        inspection.status = INSP_CANCELLED.to_string();
        inspection.updated_at = Utc::now();
        // Same compare-and-swap commit as submit: re-check the
        // live status under the entry lock so a concurrent submit
        // cannot slip in between the check and the store.
        {
            let mut entry = state
                .inspections
                .get_mut(&inspection.id)
                .ok_or_else(|| {
                    UnifiedError::not_found(format!("Inspection not found: {}", inspection.id))
                })?;
            if !valid_inspection_transition(&entry.status, INSP_CANCELLED) {
                return Err(UnifiedError::unprocessable(format!(
                    "Inspection in status \"{}\" cannot be cancelled",
                    entry.status
                )));
            }
            *entry = inspection.clone();
        }
        state.emit(
            "quality.inspection.cancelled",
            json!({
                "inspection_id": inspection.id,
                "point_id": inspection.point_id,
                "target_ref": inspection.target_ref,
                "actor": caller.actor,
            }),
        );
        Ok(json!(inspection))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::may_submit_inspection;

    #[test]
    fn assigned_inspector_may_submit() {
        assert!(may_submit_inspection(
            &Some("inspector-1".to_string()),
            "inspector-1",
            false
        ));
    }

    #[test]
    fn other_inspector_may_not_submit() {
        assert!(!may_submit_inspection(
            &Some("inspector-1".to_string()),
            "inspector-2",
            false
        ));
    }

    #[test]
    fn supervisor_may_submit_for_assigned_inspector() {
        assert!(may_submit_inspection(
            &Some("inspector-1".to_string()),
            "manager-1",
            true
        ));
    }

    #[test]
    fn unassigned_inspection_may_be_submitted_by_any_inspector() {
        assert!(may_submit_inspection(&None, "inspector-9", false));
    }
}
