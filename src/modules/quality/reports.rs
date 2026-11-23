//! quality module - Inspection reports and settings handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

use super::constants::ALERT_ACKNOWLEDGED;
use super::constants::ALERT_OPEN;
use super::constants::ALERT_RESOLVED;
use super::constants::CAPA_CLOSED;
use super::constants::CAPA_IN_PROGRESS;
use super::constants::CAPA_OPEN;
use super::constants::INSP_COMPLETED;
use super::constants::INSP_REQUESTED;
use super::constants::MAX_REF_LEN;
use super::constants::PERM_QUALITY_MANAGE;
use super::constants::PERM_QUALITY_VIEW;
use super::constants::RESULT_CONDITIONAL;
use super::constants::RESULT_FAIL;
use super::constants::RESULT_PASS;
use super::helpers::caller;
use super::helpers::err;
use super::helpers::ok;
use super::helpers::parse_date_filter;
use super::helpers::require_permission;
use super::helpers::require_settings_list;
use super::helpers::require_text;
use super::helpers::tenant_matches;
use super::state::QualityState;
use super::types::Inspection;
use super::types::UpdateSettingsInput;

// ---------------------------------------------------------------------------
// Report handlers
// ---------------------------------------------------------------------------

/// The timestamp a windowed report attributes an inspection to:
/// when it was actually inspected. Completed inspections always
/// carry `inspected_at`; the `created_at` fallback covers only
/// records constructed before that field was populated.
pub(crate) fn inspection_report_at(row: &Inspection) -> DateTime<Utc> {
    row.inspected_at.unwrap_or(row.created_at)
}

/// Completed inspections in the optional window, optionally
/// filtered by target / point / scope (scope resolves through the
/// point). Requested and cancelled inspections never count. The
/// window filters on `inspected_at` (falling back to `created_at`
/// when null), so an inspection requested in one period and
/// submitted in the next counts in the period it was inspected,
/// not the period it was created. Scoped to the caller's tenant.
pub(crate) fn report_inspections(
    state: &QualityState,
    tenant_id: &str,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    target_ref: &Option<String>,
    point_id: &Option<String>,
) -> Vec<Inspection> {
    state
        .inspections
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|row| {
            if row.status != INSP_COMPLETED {
                return false;
            }
            if !tenant_matches(&row.tenant_id, tenant_id) {
                return false;
            }
            let at = inspection_report_at(row);
            if let Some(from) = from {
                if at < from {
                    return false;
                }
            }
            if let Some(to) = to {
                if at > to {
                    return false;
                }
            }
            if let Some(target) = target_ref {
                if &row.target_ref != target {
                    return false;
                }
            }
            if let Some(point) = point_id {
                if row.point_id.to_string() != *point {
                    return false;
                }
            }
            true
        })
        .collect()
}

pub(crate) async fn report_pass_rate(
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
        let target_ref = params.filters.get("target_ref").cloned();
        let point_id = params.filters.get("point_id").cloned();
        let target_kind = params.filters.get("target_kind").cloned();
        let from = match params.filters.get("from") {
            Some(raw) => Some(parse_date_filter(raw, "from")?),
            None => None,
        };
        let to = match params.filters.get("to") {
            Some(raw) => Some(parse_date_filter(raw, "to")?),
            None => None,
        };
        let mut rows = report_inspections(&state, &caller.tenant_id, from, to, &target_ref, &point_id);
        if let Some(kind) = &target_kind {
            rows.retain(|row| &row.target_kind == kind);
        }
        let total = rows.len() as i64;
        let passed = rows
            .iter()
            .filter(|row| row.result.as_deref() == Some(RESULT_PASS))
            .count() as i64;
        let failed = rows
            .iter()
            .filter(|row| row.result.as_deref() == Some(RESULT_FAIL))
            .count() as i64;
        let conditional = rows
            .iter()
            .filter(|row| row.result.as_deref() == Some(RESULT_CONDITIONAL))
            .count() as i64;
        let pass_rate = if total > 0 {
            Some((passed as f64 / total as f64) * 100.0)
        } else {
            None
        };
        Ok(json!({
            "period": { "from": from, "to": to },
            "filters": { "target_ref": target_ref, "point_id": point_id, "target_kind": target_kind },
            "total_completed": total,
            "passed": passed,
            "failed": failed,
            "conditional": conditional,
            "pass_rate_percent": pass_rate,
            "method": "pass_rate_percent = passed / total_completed * 100 over completed inspections only; requested and cancelled inspections are excluded; a null pass rate means no completed inspection matched the filters.",
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn report_failure_causes(
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
        let target_ref = params.filters.get("target_ref").cloned();
        let point_id = params.filters.get("point_id").cloned();
        let from = match params.filters.get("from") {
            Some(raw) => Some(parse_date_filter(raw, "from")?),
            None => None,
        };
        let to = match params.filters.get("to") {
            Some(raw) => Some(parse_date_filter(raw, "to")?),
            None => None,
        };
        let rows = report_inspections(&state, &caller.tenant_id, from, to, &target_ref, &point_id);
        let failed: Vec<&Inspection> = rows
            .iter()
            .filter(|row| row.result.as_deref() == Some(RESULT_FAIL))
            .collect();
        let mut counts: HashMap<String, i64> = HashMap::new();
        for row in &failed {
            for cause in &row.failure_causes {
                *counts.entry(cause.clone()).or_insert(0) += 1;
            }
        }
        let total_cause_mentions: i64 = counts.values().sum();
        let mut causes: Vec<Value> = counts
            .into_iter()
            .map(|(cause, count)| {
                let share = if total_cause_mentions > 0 {
                    Some((count as f64 / total_cause_mentions as f64) * 100.0)
                } else {
                    None
                };
                json!({ "cause": cause, "count": count, "share_percent": share })
            })
            .collect();
        causes.sort_by(|a, b| {
            let count_a = a.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
            let count_b = b.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
            count_b.cmp(&count_a).then_with(|| {
                let name_a = a.get("cause").and_then(|v| v.as_str()).unwrap_or("");
                let name_b = b.get("cause").and_then(|v| v.as_str()).unwrap_or("");
                name_a.cmp(name_b)
            })
        });
        let failed_total = failed.len() as i64;
        Ok(json!({
            "period": { "from": from, "to": to },
            "filters": { "target_ref": target_ref, "point_id": point_id },
            "failed_inspections": failed_total,
            "total_cause_mentions": total_cause_mentions,
            "causes": causes,
            "method": "Counts failure_causes recorded on failed completed inspections in the period; one inspection may name several causes, so total_cause_mentions can exceed failed_inspections; share_percent is the cause count over total_cause_mentions.",
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn report_backlog(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    _query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_VIEW)?;
        let now = Utc::now();
        let tenant_id = caller.tenant_id.clone();
        let open_alerts = state
            .alerts
            .iter()
            .filter(|entry| {
                entry.value().status == ALERT_OPEN
                    && tenant_matches(&entry.value().tenant_id, &tenant_id)
            })
            .count() as i64;
        let acknowledged_alerts = state
            .alerts
            .iter()
            .filter(|entry| {
                entry.value().status == ALERT_ACKNOWLEDGED
                    && tenant_matches(&entry.value().tenant_id, &tenant_id)
            })
            .count() as i64;
        let blocking_alerts = state
            .alerts
            .iter()
            .filter(|entry| {
                let row = entry.value();
                row.blocked
                    && row.status != ALERT_RESOLVED
                    && tenant_matches(&row.tenant_id, &tenant_id)
            })
            .count() as i64;
        let open_capas = state
            .capas
            .iter()
            .filter(|entry| {
                entry.value().state == CAPA_OPEN
                    && tenant_matches(&entry.value().tenant_id, &tenant_id)
            })
            .count() as i64;
        let in_progress_capas = state
            .capas
            .iter()
            .filter(|entry| {
                entry.value().state == CAPA_IN_PROGRESS
                    && tenant_matches(&entry.value().tenant_id, &tenant_id)
            })
            .count() as i64;
        let overdue_capas = state
            .capas
            .iter()
            .filter(|entry| {
                let row = entry.value();
                if !tenant_matches(&row.tenant_id, &tenant_id) {
                    return false;
                }
                match row.due_date {
                    Some(due) => due < now && row.state != CAPA_CLOSED,
                    None => false,
                }
            })
            .count() as i64;
        let requested_inspections = state
            .inspections
            .iter()
            .filter(|entry| {
                entry.value().status == INSP_REQUESTED
                    && tenant_matches(&entry.value().tenant_id, &tenant_id)
            })
            .count() as i64;
        Ok(json!({
            "open_alerts": open_alerts,
            "acknowledged_alerts": acknowledged_alerts,
            "blocking_alerts": blocking_alerts,
            "open_capas": open_capas,
            "in_progress_capas": in_progress_capas,
            "overdue_capas": overdue_capas,
            "requested_inspections": requested_inspections,
            "method": "Counts are live: open/acknowledged alerts by status, blocking alerts are open or acknowledged alerts with blocked=true, overdue CAPAs are non-closed CAPAs whose due_date is past, requested inspections are inspections still awaiting submission.",
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn report_supplier_score(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_VIEW)?;
        let settings = state.settings();
        let params = query.into_inner();
        let supplier_id = match params.filters.get("supplier_id") {
            Some(raw) => require_text(raw, "supplier_id", MAX_REF_LEN)?,
            None => {
                return Err(UnifiedError::bad_request(
                    "supplier_id filter is required for the supplier score",
                ));
            }
        };
        let window_days = match params.filters.get("window_days") {
            Some(raw) => raw.parse::<i64>().map_err(|_| {
                UnifiedError::bad_request("window_days must be a whole number of days")
            })?,
            None => settings.default_score_window_days,
        };
        if window_days <= 0 || window_days > 3650 {
            return Err(UnifiedError::bad_request(
                "window_days must be between 1 and 3650",
            ));
        }
        let min_inspections = match params.filters.get("min_inspections") {
            Some(raw) => raw
                .parse::<i64>()
                .map_err(|_| UnifiedError::bad_request("min_inspections must be a whole number"))?,
            None => settings.default_min_inspections_for_score,
        };
        if min_inspections < 0 {
            return Err(UnifiedError::bad_request(
                "min_inspections must not be negative",
            ));
        }
        let since = Utc::now() - chrono::Duration::days(window_days);
        let rows = report_inspections(
            &state,
            &caller.tenant_id,
            Some(since),
            None,
            &Some(supplier_id.clone()),
            &None,
        );
        // Supplier scope only: inspections whose point scope is
        // `supplier` or `incoming_goods` for this supplier. The
        // scope names come from the live settings list, so an
        // admin-added scope is never silently excluded — the
        // caller may also pass scope= to widen the set.
        let scope_filter = params.filters.get("scope").cloned();
        let mut scoped: Vec<Inspection> = Vec::new();
        for row in rows {
            let point_scope = state
                .points
                .get(&row.point_id)
                .map(|entry| entry.value().scope.clone());
            let in_scope = match &scope_filter {
                Some(wanted) => point_scope.as_deref() == Some(wanted.as_str()),
                None => matches!(
                    point_scope.as_deref(),
                    Some("supplier") | Some("incoming_goods")
                ),
            };
            if in_scope {
                scoped.push(row);
            }
        }
        let total = scoped.len() as i64;
        let passed = scoped
            .iter()
            .filter(|row| row.result.as_deref() == Some(RESULT_PASS))
            .count() as i64;
        let failed = scoped
            .iter()
            .filter(|row| row.result.as_deref() == Some(RESULT_FAIL))
            .count() as i64;
        let conditional = scoped
            .iter()
            .filter(|row| row.result.as_deref() == Some(RESULT_CONDITIONAL))
            .count() as i64;
        let score = if total >= min_inspections && total > 0 {
            let raw = 100.0
                - (failed as f64 * settings.fail_penalty_points)
                - (conditional as f64 * settings.conditional_penalty_points);
            Some(raw.clamp(0.0, 100.0))
        } else {
            None
        };
        Ok(json!({
            "supplier_id": supplier_id,
            "inputs": {
                "window_days": window_days,
                "window_since": since,
                "min_inspections_for_score": min_inspections,
                "fail_penalty_points": settings.fail_penalty_points,
                "conditional_penalty_points": settings.conditional_penalty_points,
                "scope": scope_filter,
            },
            "inspections": total,
            "passed": passed,
            "failed": failed,
            "conditional": conditional,
            "score": score,
            "status": if score.is_some() { "sufficient_data" } else { "insufficient_data" },
            "formula": "score = clamp(100 - failed * fail_penalty_points - conditional * conditional_penalty_points, 0, 100), computed over completed inspections for this supplier in the window; null when fewer than min_inspections_for_score inspections exist. Penalty points come from settings, never from code.",
            "method": "Only completed inspections count; the default scopes are supplier and incoming_goods, overridable with scope=. The score is a transparent arithmetic result of the inputs above, not a black box.",
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Settings handlers
// ---------------------------------------------------------------------------

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_VIEW)?;
        Ok(json!(state.settings()))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(list) = &input.point_scopes {
            settings.point_scopes = require_settings_list(list, "point_scopes")?;
        }
        if let Some(list) = &input.check_kinds {
            settings.check_kinds = require_settings_list(list, "check_kinds")?;
        }
        if let Some(list) = &input.criterion_kinds {
            settings.criterion_kinds = require_settings_list(list, "criterion_kinds")?;
        }
        if let Some(list) = &input.fail_policies {
            settings.fail_policies = require_settings_list(list, "fail_policies")?;
        }
        if let Some(list) = &input.inspection_results {
            settings.inspection_results = require_settings_list(list, "inspection_results")?;
        }
        if let Some(list) = &input.failure_causes {
            settings.failure_causes = require_settings_list(list, "failure_causes")?;
        }
        if let Some(list) = &input.capa_states {
            settings.capa_states = require_settings_list(list, "capa_states")?;
        }
        if let Some(list) = &input.alert_severities {
            settings.alert_severities = require_settings_list(list, "alert_severities")?;
        }
        if let Some(policy) = &input.default_fail_policy {
            if !settings.fail_policies.iter().any(|item| item == policy) {
                return Err(UnifiedError::bad_request(
                    "default_fail_policy must be one of the configured fail_policies",
                ));
            }
            settings.default_fail_policy = policy.trim().to_string();
        }
        if let Some(severity) = &input.default_alert_severity {
            if !settings
                .alert_severities
                .iter()
                .any(|item| item == severity)
            {
                return Err(UnifiedError::bad_request(
                    "default_alert_severity must be one of the configured alert_severities",
                ));
            }
            settings.default_alert_severity = severity.trim().to_string();
        }
        if let Some(days) = input.default_score_window_days {
            if days <= 0 || days > 3650 {
                return Err(UnifiedError::bad_request(
                    "default_score_window_days must be between 1 and 3650",
                ));
            }
            settings.default_score_window_days = days;
        }
        if let Some(min) = input.default_min_inspections_for_score {
            if min < 0 {
                return Err(UnifiedError::bad_request(
                    "default_min_inspections_for_score must not be negative",
                ));
            }
            settings.default_min_inspections_for_score = min;
        }
        if let Some(penalty) = input.fail_penalty_points {
            if !penalty.is_finite() || penalty < 0.0 {
                return Err(UnifiedError::bad_request(
                    "fail_penalty_points must be a non-negative number",
                ));
            }
            settings.fail_penalty_points = penalty;
        }
        if let Some(penalty) = input.conditional_penalty_points {
            if !penalty.is_finite() || penalty < 0.0 {
                return Err(UnifiedError::bad_request(
                    "conditional_penalty_points must be a non-negative number",
                ));
            }
            settings.conditional_penalty_points = penalty;
        }
        if let Some(required) = input.capa_verification_required {
            settings.capa_verification_required = required;
        }
        if let Some(max) = input.max_criteria_per_point {
            if max == 0 || max > 1000 {
                return Err(UnifiedError::bad_request(
                    "max_criteria_per_point must be between 1 and 1000",
                ));
            }
            settings.max_criteria_per_point = max;
        }
        if let Some(max) = input.max_measurements_per_inspection {
            if max == 0 || max > 5000 {
                return Err(UnifiedError::bad_request(
                    "max_measurements_per_inspection must be between 1 and 5000",
                ));
            }
            settings.max_measurements_per_inspection = max;
        }
        if let Some(max) = input.max_evidence_refs {
            if max > 500 {
                return Err(UnifiedError::bad_request(
                    "max_evidence_refs must be between 0 and 500",
                ));
            }
            settings.max_evidence_refs = max;
        }
        *state.settings.write().expect("settings lock poisoned") = settings.clone();
        state.emit("quality.settings.updated", json!({ "actor": caller.actor }));
        Ok(json!(settings))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::inspection_report_at;
    use super::report_inspections;
    use crate::modules::quality::Inspection;
    use crate::modules::quality::QualityState;
    use chrono::{Duration, Utc};
    use uuid::Uuid;

    fn inspection(
        tenant_id: Option<String>,
        created_at: chrono::DateTime<Utc>,
        inspected_at: Option<chrono::DateTime<Utc>>,
    ) -> Inspection {
        Inspection {
            id: Uuid::new_v4(),
            point_id: Uuid::new_v4(),
            target_ref: "target-1".to_string(),
            target_kind: "product".to_string(),
            inspector_id: None,
            status: "completed".to_string(),
            result: Some("pass".to_string()),
            measurements: Vec::new(),
            failure_causes: Vec::new(),
            notes: None,
            evidence_refs: Vec::new(),
            inspected_at,
            company_id: None,
            tenant_id,
            created_at,
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn report_window_uses_inspected_at_not_created_at() {
        let state = QualityState::default();
        let now = Utc::now();
        // Created 30 days ago, inspected yesterday: belongs to
        // the last-7-days window by inspection date only.
        let row = inspection(
            Some("tenant-a".to_string()),
            now - Duration::days(30),
            Some(now - Duration::days(1)),
        );
        state.inspections.insert(row.id, row);
        let from = now - Duration::days(7);
        let rows = report_inspections(&state, "tenant-a", Some(from), None, &None, &None);
        assert_eq!(rows.len(), 1);
        // ...and is excluded from a window that ended before the
        // inspection, even though it was created long before that
        // too — the inspected date decides both bounds.
        let rows = report_inspections(
            &state,
            "tenant-a",
            Some(now - Duration::days(40)),
            Some(now - Duration::days(2)),
            &None,
            &None,
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn report_window_falls_back_to_created_at_when_uninspected() {
        let state = QualityState::default();
        let now = Utc::now();
        let row = inspection(Some("tenant-a".to_string()), now - Duration::days(3), None);
        state.inspections.insert(row.id, row);
        assert_eq!(
            report_inspections(
                &state,
                "tenant-a",
                Some(now - Duration::days(7)),
                None,
                &None,
                &None
            )
            .len(),
            1
        );
        assert_eq!(inspection_report_at(&inspection(None, now, None)), now);
    }

    #[test]
    fn report_excludes_other_tenant_and_non_completed() {
        let state = QualityState::default();
        let now = Utc::now();
        let other = inspection(Some("tenant-b".to_string()), now, Some(now));
        state.inspections.insert(other.id, other);
        let mut requested = inspection(Some("tenant-a".to_string()), now, Some(now));
        requested.status = "requested".to_string();
        state.inspections.insert(requested.id, requested);
        assert!(report_inspections(&state, "tenant-a", None, None, &None, &None).is_empty());
    }
}
