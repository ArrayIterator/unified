//! quality module - HTTP handlers for alerts and blocks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;

use super::constants::ALERT_ACKNOWLEDGED;
use super::constants::ALERT_RESOLVED;
use super::constants::PERM_QUALITY_MANAGE;
use super::constants::PERM_QUALITY_VIEW;
use super::evaluation::valid_alert_transition;
use super::helpers::caller;
use super::helpers::err;
use super::helpers::ok;
use super::helpers::paginate;
use super::helpers::require_permission;
use super::helpers::tenant_matches;
use super::state::QualityState;
use super::store::load_alert;
use super::types::Capa;
use super::types::QualityAlert;

// ---------------------------------------------------------------------------
// Alert + block handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_alerts(
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
        let status_filter = params.filters.get("status").cloned();
        let severity_filter = params.filters.get("severity").cloned();
        let point_filter = params.filters.get("point_id").cloned();
        let target_filter = params.filters.get("target_ref").cloned();
        let blocked_filter = params.filters.get("blocked").cloned();
        let mut items: Vec<QualityAlert> = state
            .alerts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if !tenant_matches(&row.tenant_id, &caller.tenant_id) {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &row.status != status {
                        return false;
                    }
                }
                if let Some(severity) = &severity_filter {
                    if &row.severity != severity {
                        return false;
                    }
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
                if let Some(blocked) = &blocked_filter {
                    if row.blocked != (blocked == "true") {
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

pub(crate) async fn get_alert(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_VIEW)?;
        let alert = load_alert(&state, &path.into_inner(), &caller.tenant_id)?;
        let inspection = state
            .inspections
            .get(&alert.inspection_id)
            .map(|entry| entry.value().clone())
            .filter(|row| tenant_matches(&row.tenant_id, &caller.tenant_id));
        let capas: Vec<Capa> = state
            .capas
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                row.alert_id == Some(alert.id)
                    && tenant_matches(&row.tenant_id, &caller.tenant_id)
            })
            .collect();
        Ok(json!({ "alert": alert, "inspection": inspection, "capas": capas }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Apply an alert transition to a loaded record: validate the
/// move, stamp the actor, and — on resolve — clear `blocked`.
/// Resolving an alert unblocks its target, so the stored record
/// (and the emitted event, which carries the same payload) must
/// say `blocked: false`; leaving `blocked: true` on a resolved
/// alert contradicts the resolution even though the block queries
/// already exclude resolved alerts by status. Kept pure so the
/// rule is unit-testable.
pub(crate) fn apply_alert_transition(
    alert: &mut QualityAlert,
    to: &str,
    actor: &str,
) -> UnifiedResult<()> {
    if !valid_alert_transition(&alert.status, to) {
        return Err(UnifiedError::unprocessable(format!(
            "Alert in status \"{}\" cannot move to \"{to}\"",
            alert.status
        )));
    }
    match to {
        ALERT_ACKNOWLEDGED => {
            alert.acknowledged_by = Some(actor.to_string());
        }
        ALERT_RESOLVED => {
            alert.resolved_by = Some(actor.to_string());
            alert.resolved_at = Some(Utc::now());
            // Resolving lifts the block.
            alert.blocked = false;
        }
        _ => {}
    }
    alert.status = to.to_string();
    alert.updated_at = Utc::now();
    Ok(())
}

pub(crate) async fn transition_alert(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
    to: &'static str,
    event: &'static str,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_MANAGE)?;
        let mut alert = load_alert(&state, &path.into_inner(), &caller.tenant_id)?;
        apply_alert_transition(&mut alert, to, &caller.actor)?;
        // Commit under the entry lock: re-check the live status
        // before swapping in place, so two concurrent transitions
        // cannot both pass the check from the same loaded state.
        {
            let mut entry = state
                .alerts
                .get_mut(&alert.id)
                .ok_or_else(|| {
                    UnifiedError::not_found(format!("Quality alert not found: {}", alert.id))
                })?;
            if !valid_alert_transition(&entry.status, to) {
                return Err(UnifiedError::unprocessable(format!(
                    "Alert in status \"{}\" cannot move to \"{to}\"",
                    entry.status
                )));
            }
            *entry = alert.clone();
        }
        state.emit(
            event,
            json!({
                "alert_id": alert.id,
                "inspection_id": alert.inspection_id,
                "point_id": alert.point_id,
                "target_ref": alert.target_ref,
                "target_kind": alert.target_kind,
                "status": alert.status,
                "blocked": alert.blocked,
                "actor": caller.actor,
            }),
        );
        Ok(json!(alert))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn acknowledge_alert(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    transition_alert(
        ctx,
        state,
        req,
        path,
        ALERT_ACKNOWLEDGED,
        "quality.alert.acknowledged",
    )
    .await
}

pub(crate) async fn resolve_alert(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    transition_alert(
        ctx,
        state,
        req,
        path,
        ALERT_RESOLVED,
        "quality.alert.resolved",
    )
    .await
}

/// The currently blocked targets: open alerts whose point policy
/// was `block`. Owning modules consult this before letting a
/// record proceed; resolving the alert unblocks the target.
pub(crate) async fn list_blocks(
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
        let target_filter = params.filters.get("target_ref").cloned();
        let kind_filter = params.filters.get("target_kind").cloned();
        let mut items: Vec<QualityAlert> = state
            .alerts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if !tenant_matches(&row.tenant_id, &caller.tenant_id) {
                    return false;
                }
                if !row.blocked || row.status == ALERT_RESOLVED {
                    return false;
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
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let blocked_targets: Vec<String> = items
            .iter()
            .map(|row| row.target_ref.clone())
            .collect::<std::collections::HashSet<String>>()
            .into_iter()
            .collect();
        let page = paginate(items, &params);
        Ok(json!({
            "blocks": page,
            "blocked_targets": blocked_targets,
            "method": "A target is blocked while an alert with blocked=true and status open/acknowledged exists for it; resolving the alert unblocks it.",
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::apply_alert_transition;
    use crate::modules::quality::QualityAlert;
    use chrono::Utc;
    use uuid::Uuid;

    fn alert(blocked: bool) -> QualityAlert {
        let now = Utc::now();
        QualityAlert {
            id: Uuid::new_v4(),
            inspection_id: Uuid::new_v4(),
            point_id: Uuid::new_v4(),
            target_ref: "target-1".to_string(),
            target_kind: "product".to_string(),
            severity: "high".to_string(),
            status: "open".to_string(),
            message: "Failed".to_string(),
            blocked,
            acknowledged_by: None,
            resolved_by: None,
            resolved_at: None,
            tenant_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn resolve_clears_blocked() {
        let mut row = alert(true);
        assert!(apply_alert_transition(&mut row, "resolved", "manager-1").is_ok());
        assert_eq!(row.status, "resolved");
        assert!(!row.blocked);
        assert_eq!(row.resolved_by, Some("manager-1".to_string()));
        assert!(row.resolved_at.is_some());
    }

    #[test]
    fn acknowledge_keeps_block() {
        let mut row = alert(true);
        assert!(apply_alert_transition(&mut row, "acknowledged", "manager-1").is_ok());
        assert_eq!(row.status, "acknowledged");
        assert!(row.blocked);
        assert_eq!(row.acknowledged_by, Some("manager-1".to_string()));
    }

    #[test]
    fn invalid_transition_rejected() {
        let mut row = alert(false);
        row.status = "resolved".to_string();
        assert!(apply_alert_transition(&mut row, "open", "manager-1").is_err());
        assert_eq!(row.status, "resolved");
    }
}
