//! quality module - HTTP handlers for points and criteria.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

use super::constants::MAX_DESCRIPTION_LEN;
use super::constants::MAX_REF_LEN;
use super::constants::MAX_SLUG_LEN;
use super::constants::MAX_TEXT_LEN;
use super::constants::MAX_TITLE_LEN;
use super::constants::PERM_QUALITY_MANAGE;
use super::constants::PERM_QUALITY_VIEW;
use super::evaluation::validate_criterion_shape;
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
use super::store::load_criterion;
use super::store::load_point;
use super::types::CreateCriterionInput;
use super::types::CreatePointInput;
use super::types::QualityCriterion;
use super::types::QualityPoint;
use super::types::QualitySettings;
use super::types::UpdateCriterionInput;
use super::types::UpdatePointInput;

// ---------------------------------------------------------------------------
// Pure update application (kept separate from the handlers so the
// nullable-update semantics are unit-testable)
// ---------------------------------------------------------------------------

/// Apply a point update. Clearable fields (`target_ref`,
/// `target_kind`) use nullable-update semantics: absent leaves
/// the value alone, `null` clears it, a value sets it.
pub(crate) fn apply_point_update(
    point: &mut QualityPoint,
    input: &UpdatePointInput,
    settings: &QualitySettings,
) -> UnifiedResult<()> {
    if let Some(name) = &input.name {
        point.name = require_text(name, "name", MAX_TITLE_LEN)?;
    }
    if let Some(description) = &input.description {
        point.description = require_text(description, "description", MAX_DESCRIPTION_LEN)?;
    }
    if let Some(scope) = &input.scope {
        point.scope = require_in_list(scope, "scope", &settings.point_scopes)?;
    }
    if let Some(check_kind) = &input.check_kind {
        point.check_kind = require_in_list(check_kind, "check_kind", &settings.check_kinds)?;
    }
    if let Some(policy) = &input.on_fail_policy {
        point.on_fail_policy =
            require_in_list(policy, "on_fail_policy", &settings.fail_policies)?;
    }
    if let Some(severity) = &input.alert_severity {
        point.alert_severity =
            require_in_list(severity, "alert_severity", &settings.alert_severities)?;
    }
    if let Some(target_ref) = &input.target_ref {
        point.target_ref = match target_ref {
            Some(raw) => Some(require_text(raw, "target_ref", MAX_REF_LEN)?),
            None => None,
        };
    }
    if let Some(target_kind) = &input.target_kind {
        point.target_kind = match target_kind {
            Some(raw) => Some(require_text(raw, "target_kind", MAX_SLUG_LEN)?),
            None => None,
        };
    }
    if let Some(active) = input.active {
        point.active = active;
    }
    Ok(())
}

/// Apply a criterion update with nullable-update semantics, then
/// re-validate the resulting shape — clearing the stale bounds is
/// what lets a criterion change kind (numeric to boolean, or the
/// reverse) without failing validation against values that no
/// longer apply.
pub(crate) fn apply_criterion_update(
    criterion: &mut QualityCriterion,
    input: &UpdateCriterionInput,
    settings: &QualitySettings,
) -> UnifiedResult<()> {
    if let Some(name) = &input.name {
        criterion.name = require_text(name, "name", MAX_TITLE_LEN)?;
    }
    if let Some(kind) = &input.kind {
        criterion.kind = require_in_list(kind, "kind", &settings.criterion_kinds)?;
    }
    if let Some(unit) = &input.unit {
        criterion.unit = match unit {
            Some(raw) => Some(require_text(raw, "unit", MAX_SLUG_LEN)?),
            None => None,
        };
    }
    if let Some(min_value) = input.min_value {
        criterion.min_value = min_value;
    }
    if let Some(max_value) = input.max_value {
        criterion.max_value = max_value;
    }
    if let Some(expected_bool) = input.expected_bool {
        criterion.expected_bool = expected_bool;
    }
    if let Some(expected_text) = &input.expected_text {
        criterion.expected_text = match expected_text {
            Some(raw) => Some(require_text(raw, "expected_text", MAX_TEXT_LEN)?),
            None => None,
        };
    }
    if let Some(failure_cause) = &input.failure_cause {
        criterion.failure_cause = match failure_cause {
            Some(raw) => Some(require_in_list(
                raw,
                "failure_cause",
                &settings.failure_causes,
            )?),
            None => None,
        };
    }
    if let Some(weight) = input.weight {
        criterion.weight = weight;
    }
    if let Some(required) = input.required {
        criterion.required = required;
    }
    validate_criterion_shape(
        &criterion.kind,
        criterion.min_value,
        criterion.max_value,
        criterion.expected_bool,
        &criterion.expected_text,
        criterion.weight,
        settings,
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Quality point handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_points(
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
        let scope_filter = params.filters.get("scope").cloned();
        let kind_filter = params.filters.get("check_kind").cloned();
        let policy_filter = params.filters.get("on_fail_policy").cloned();
        let target_filter = params.filters.get("target_ref").cloned();
        let active_filter = params.filters.get("active").cloned();
        let search = params.search.clone().map(|value| value.to_lowercase());
        let mut items: Vec<QualityPoint> = state
            .points
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|point| {
                if !tenant_matches(&point.tenant_id, &caller.tenant_id) {
                    return false;
                }
                if let Some(scope) = &scope_filter {
                    if &point.scope != scope {
                        return false;
                    }
                }
                if let Some(kind) = &kind_filter {
                    if &point.check_kind != kind {
                        return false;
                    }
                }
                if let Some(policy) = &policy_filter {
                    if &point.on_fail_policy != policy {
                        return false;
                    }
                }
                if let Some(target) = &target_filter {
                    if point.target_ref.as_deref() != Some(target.as_str()) {
                        return false;
                    }
                }
                if let Some(active) = &active_filter {
                    let wanted = active == "true";
                    if point.active != wanted {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    let haystack = format!("{} {}", point.name, point.description).to_lowercase();
                    if !haystack.contains(needle.as_str()) {
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

pub(crate) async fn create_point(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    body: web::Json<CreatePointInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_MANAGE)?;
        let settings = state.settings();
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_TITLE_LEN)?;
        let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)
            .unwrap_or_default();
        let scope = require_in_list(&input.scope, "scope", &settings.point_scopes)?;
        let check_kind = require_in_list(&input.check_kind, "check_kind", &settings.check_kinds)?;
        let on_fail_policy = match &input.on_fail_policy {
            Some(policy) => require_in_list(policy, "on_fail_policy", &settings.fail_policies)?,
            None => settings.default_fail_policy.clone(),
        };
        let alert_severity = match &input.alert_severity {
            Some(severity) => {
                require_in_list(severity, "alert_severity", &settings.alert_severities)?
            }
            None => settings.default_alert_severity.clone(),
        };
        let target_ref = optional_text(&input.target_ref, "target_ref", MAX_REF_LEN)?;
        let target_kind = optional_text(&input.target_kind, "target_kind", MAX_SLUG_LEN)?;
        let company_id = optional_text(&input.company_id, "company_id", MAX_REF_LEN)?;
        let now = Utc::now();
        let point = QualityPoint {
            id: Uuid::new_v4(),
            name,
            description,
            scope,
            target_ref,
            target_kind,
            check_kind,
            on_fail_policy,
            alert_severity,
            active: true,
            company_id,
            tenant_id: Some(caller.tenant_id.clone()),
            created_at: now,
            updated_at: now,
        };
        state.points.insert(point.id, point.clone());
        state.emit(
            "quality.point.created",
            json!({
                "point_id": point.id,
                "scope": point.scope,
                "check_kind": point.check_kind,
                "on_fail_policy": point.on_fail_policy,
                "actor": caller.actor,
            }),
        );
        Ok(json!(point))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_point(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_VIEW)?;
        let point = load_point(&state, &path.into_inner(), &caller.tenant_id)?;
        let criteria = state.criteria_for_point(point.id);
        Ok(json!({ "point": point, "criteria": criteria }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_point(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePointInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_MANAGE)?;
        let settings = state.settings();
        let input = body.into_inner();
        let mut point = load_point(&state, &path.into_inner(), &caller.tenant_id)?;
        apply_point_update(&mut point, &input, &settings)?;
        point.updated_at = Utc::now();
        state.points.insert(point.id, point.clone());
        state.emit(
            "quality.point.updated",
            json!({ "point_id": point.id, "actor": caller.actor }),
        );
        Ok(json!(point))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_point(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_MANAGE)?;
        let point = load_point(&state, &path.into_inner(), &caller.tenant_id)?;
        let has_inspections = state
            .inspections
            .iter()
            .any(|entry| {
                entry.value().point_id == point.id
                    && tenant_matches(&entry.value().tenant_id, &caller.tenant_id)
            });
        if has_inspections {
            return Err(UnifiedError::conflict(
                "Quality point has inspections and cannot be deleted; deactivate it instead",
            ));
        }
        // Alerts and CAPAs reference the point through their
        // inspection, so the inspection check above already blocks
        // deletion while any exist — but check them directly too,
        // so a point is never deleted out from under a live alert
        // or CAPA even if the inspection link is ever removed.
        let has_alerts = state
            .alerts
            .iter()
            .any(|entry| {
                entry.value().point_id == point.id
                    && tenant_matches(&entry.value().tenant_id, &caller.tenant_id)
            });
        if has_alerts {
            return Err(UnifiedError::conflict(
                "Quality point has alerts and cannot be deleted; deactivate it instead",
            ));
        }
        let has_capas = state.capas.iter().any(|entry| {
            let row = entry.value();
            if !tenant_matches(&row.tenant_id, &caller.tenant_id) {
                return false;
            }
            state
                .inspections
                .get(&row.inspection_id)
                .map(|insp| insp.value().point_id == point.id)
                .unwrap_or(false)
        });
        if has_capas {
            return Err(UnifiedError::conflict(
                "Quality point has CAPAs and cannot be deleted; deactivate it instead",
            ));
        }
        for criterion in state.criteria_for_point(point.id) {
            state.criteria.remove(&criterion.id);
        }
        state.points.remove(&point.id);
        state.emit(
            "quality.point.deleted",
            json!({ "point_id": point.id, "actor": caller.actor }),
        );
        Ok(json!({ "deleted": true, "id": point.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn set_point_active(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
    active: bool,
    event: &'static str,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_MANAGE)?;
        let mut point = load_point(&state, &path.into_inner(), &caller.tenant_id)?;
        point.active = active;
        point.updated_at = Utc::now();
        state.points.insert(point.id, point.clone());
        state.emit(
            event,
            json!({ "point_id": point.id, "active": active, "actor": caller.actor }),
        );
        Ok(json!(point))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn activate_point(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    set_point_active(ctx, state, req, path, true, "quality.point.activated").await
}

pub(crate) async fn deactivate_point(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    set_point_active(ctx, state, req, path, false, "quality.point.deactivated").await
}

// ---------------------------------------------------------------------------
// Criterion handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_criteria(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_VIEW)?;
        let point = load_point(&state, &path.into_inner(), &caller.tenant_id)?;
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let mut items = state.criteria_for_point(point.id);
        if let Some(kind) = &kind_filter {
            items.retain(|row| &row.kind == kind);
        }
        Ok(json!(paginate(items, &params)))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_criterion(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateCriterionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_MANAGE)?;
        let settings = state.settings();
        let point = load_point(&state, &path.into_inner(), &caller.tenant_id)?;
        let input = body.into_inner();
        if state.criteria_for_point(point.id).len() >= settings.max_criteria_per_point {
            return Err(UnifiedError::bad_request(format!(
                "Quality point already has the maximum of {} criteria",
                settings.max_criteria_per_point
            )));
        }
        let name = require_text(&input.name, "name", MAX_TITLE_LEN)?;
        let kind = require_in_list(&input.kind, "kind", &settings.criterion_kinds)?;
        let unit = optional_text(&input.unit, "unit", MAX_SLUG_LEN)?;
        let expected_text = optional_text(&input.expected_text, "expected_text", MAX_TEXT_LEN)?;
        let failure_cause = match &input.failure_cause {
            Some(cause) => Some(require_in_list(
                cause,
                "failure_cause",
                &settings.failure_causes,
            )?),
            None => None,
        };
        validate_criterion_shape(
            &kind,
            input.min_value,
            input.max_value,
            input.expected_bool,
            &expected_text,
            input.weight,
            &settings,
        )?;
        let now = Utc::now();
        let criterion = QualityCriterion {
            id: Uuid::new_v4(),
            point_id: point.id,
            name,
            kind,
            unit,
            min_value: input.min_value,
            max_value: input.max_value,
            expected_bool: input.expected_bool,
            expected_text,
            failure_cause,
            weight: input.weight,
            required: input.required,
            // A criterion belongs to the same tenant as its point.
            tenant_id: point.tenant_id.clone(),
            created_at: now,
            updated_at: now,
        };
        state.criteria.insert(criterion.id, criterion.clone());
        state.emit(
            "quality.criterion.created",
            json!({
                "criterion_id": criterion.id,
                "point_id": point.id,
                "kind": criterion.kind,
                "actor": caller.actor,
            }),
        );
        Ok(json!(criterion))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_criterion(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCriterionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_MANAGE)?;
        let settings = state.settings();
        let input = body.into_inner();
        let mut criterion = load_criterion(&state, &path.into_inner(), &caller.tenant_id)?;
        apply_criterion_update(&mut criterion, &input, &settings)?;
        criterion.updated_at = Utc::now();
        state.criteria.insert(criterion.id, criterion.clone());
        state.emit(
            "quality.criterion.updated",
            json!({ "criterion_id": criterion.id, "actor": caller.actor }),
        );
        Ok(json!(criterion))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_criterion(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_MANAGE)?;
        let criterion = load_criterion(&state, &path.into_inner(), &caller.tenant_id)?;
        let used = state.inspections.iter().any(|entry| {
            let row = entry.value();
            tenant_matches(&row.tenant_id, &caller.tenant_id)
                && row
                    .measurements
                    .iter()
                    .any(|item| item.criterion_id == criterion.id)
        });
        if used {
            return Err(UnifiedError::conflict(
                "Criterion is referenced by completed inspections and cannot be deleted",
            ));
        }
        state.criteria.remove(&criterion.id);
        state.emit(
            "quality.criterion.deleted",
            json!({ "criterion_id": criterion.id, "actor": caller.actor }),
        );
        Ok(json!({ "deleted": true, "id": criterion.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::apply_criterion_update;
    use super::apply_point_update;
    use crate::modules::quality::QualityCriterion;
    use crate::modules::quality::QualityPoint;
    use crate::modules::quality::QualitySettings;
    use crate::modules::quality::UpdateCriterionInput;
    use crate::modules::quality::UpdatePointInput;
    use chrono::Utc;
    use uuid::Uuid;

    fn settings() -> QualitySettings {
        QualitySettings::default()
    }

    fn numeric_criterion() -> QualityCriterion {
        let now = Utc::now();
        QualityCriterion {
            id: Uuid::new_v4(),
            point_id: Uuid::new_v4(),
            name: "Thickness".to_string(),
            kind: "numeric".to_string(),
            unit: Some("mm".to_string()),
            min_value: Some(0.0),
            max_value: Some(10.0),
            expected_bool: None,
            expected_text: None,
            failure_cause: None,
            weight: 1.0,
            required: true,
            tenant_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn point() -> QualityPoint {
        let now = Utc::now();
        QualityPoint {
            id: Uuid::new_v4(),
            name: "Point".to_string(),
            description: String::new(),
            scope: "product".to_string(),
            target_ref: Some("prod-1".to_string()),
            target_kind: Some("product".to_string()),
            check_kind: "visual".to_string(),
            on_fail_policy: "alert_only".to_string(),
            alert_severity: "medium".to_string(),
            active: true,
            company_id: None,
            tenant_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn criterion_update(json: &str) -> UpdateCriterionInput {
        serde_json::from_str(json).unwrap()
    }

    fn point_update(json: &str) -> UpdatePointInput {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn criterion_kind_change_clears_stale_bounds() {
        // numeric -> boolean used to fail validation against the
        // stale min/max that could never be cleared.
        let mut item = numeric_criterion();
        let input = criterion_update(
            r#"{"kind": "boolean", "min_value": null, "max_value": null, "unit": null, "expected_bool": true}"#,
        );
        assert!(apply_criterion_update(&mut item, &input, &settings()).is_ok());
        assert_eq!(item.kind, "boolean");
        assert_eq!(item.min_value, None);
        assert_eq!(item.max_value, None);
        assert_eq!(item.unit, None);
        assert_eq!(item.expected_bool, Some(true));
    }

    #[test]
    fn criterion_kind_change_without_clearing_still_rejected() {
        let mut item = numeric_criterion();
        let input =
            criterion_update(r#"{"kind": "boolean", "expected_bool": true}"#);
        assert!(apply_criterion_update(&mut item, &input, &settings()).is_err());
    }

    #[test]
    fn criterion_absent_fields_are_left_alone() {
        let mut item = numeric_criterion();
        let input = criterion_update(r#"{"name": "Width"}"#);
        assert!(apply_criterion_update(&mut item, &input, &settings()).is_ok());
        assert_eq!(item.name, "Width");
        assert_eq!(item.min_value, Some(0.0));
        assert_eq!(item.max_value, Some(10.0));
        assert_eq!(item.unit, Some("mm".to_string()));
    }

    #[test]
    fn criterion_boolean_to_numeric_clears_expectation() {
        let mut item = numeric_criterion();
        item.kind = "boolean".to_string();
        item.min_value = None;
        item.max_value = None;
        item.expected_bool = Some(true);
        item.unit = None;
        let input = criterion_update(
            r#"{"kind": "numeric", "expected_bool": null, "min_value": 1.0, "max_value": 5.0}"#,
        );
        assert!(apply_criterion_update(&mut item, &input, &settings()).is_ok());
        assert_eq!(item.kind, "numeric");
        assert_eq!(item.expected_bool, None);
        assert_eq!(item.min_value, Some(1.0));
    }

    #[test]
    fn point_target_can_be_cleared_and_set() {
        let mut item = point();
        let clear = point_update(r#"{"target_ref": null, "target_kind": null}"#);
        assert!(apply_point_update(&mut item, &clear, &settings()).is_ok());
        assert_eq!(item.target_ref, None);
        assert_eq!(item.target_kind, None);
        let set = point_update(r#"{"target_ref": "prod-2", "target_kind": "product"}"#);
        assert!(apply_point_update(&mut item, &set, &settings()).is_ok());
        assert_eq!(item.target_ref, Some("prod-2".to_string()));
        let untouched = point_update(r#"{"name": "Renamed"}"#);
        assert!(apply_point_update(&mut item, &untouched, &settings()).is_ok());
        assert_eq!(item.target_ref, Some("prod-2".to_string()));
    }
}
