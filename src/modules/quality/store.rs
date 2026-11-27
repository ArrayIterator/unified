//! quality module - Loaders and block lookups over the state.

use unified_core::error::{UnifiedError, UnifiedResult};
use uuid::Uuid;

use super::constants::ALERT_RESOLVED;
use super::helpers::parse_uuid;
use super::helpers::tenant_matches;
use super::state::QualityState;
use super::types::Capa;
use super::types::Inspection;
use super::types::QualityAlert;
use super::types::QualityCriterion;
use super::types::QualityPoint;

// Loaders

/// Loaders are tenant-scoped: a record owned by another tenant
/// is reported as not found, never as a forbidden record - the
/// caller learns nothing about records outside its tenant.
fn not_found(what: &str, id: Uuid) -> UnifiedError {
    UnifiedError::not_found(format!("{what} not found: {id}"))
}

pub(crate) fn load_point(
    state: &QualityState,
    raw_id: &str,
    tenant_id: &str,
) -> UnifiedResult<QualityPoint> {
    let id = parse_uuid(raw_id, "quality point")?;
    state
        .points
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|row| tenant_matches(&row.tenant_id, tenant_id))
        .ok_or_else(|| not_found("Quality point", id))
}

pub(crate) fn load_criterion(
    state: &QualityState,
    raw_id: &str,
    tenant_id: &str,
) -> UnifiedResult<QualityCriterion> {
    let id = parse_uuid(raw_id, "criterion")?;
    state
        .criteria
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|row| tenant_matches(&row.tenant_id, tenant_id))
        .ok_or_else(|| not_found("Criterion", id))
}

pub(crate) fn load_inspection(
    state: &QualityState,
    raw_id: &str,
    tenant_id: &str,
) -> UnifiedResult<Inspection> {
    let id = parse_uuid(raw_id, "inspection")?;
    state
        .inspections
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|row| tenant_matches(&row.tenant_id, tenant_id))
        .ok_or_else(|| not_found("Inspection", id))
}

pub(crate) fn load_alert(
    state: &QualityState,
    raw_id: &str,
    tenant_id: &str,
) -> UnifiedResult<QualityAlert> {
    let id = parse_uuid(raw_id, "alert")?;
    state
        .alerts
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|row| tenant_matches(&row.tenant_id, tenant_id))
        .ok_or_else(|| not_found("Quality alert", id))
}

pub(crate) fn load_capa(
    state: &QualityState,
    raw_id: &str,
    tenant_id: &str,
) -> UnifiedResult<Capa> {
    let id = parse_uuid(raw_id, "CAPA")?;
    state
        .capas
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|row| tenant_matches(&row.tenant_id, tenant_id))
        .ok_or_else(|| not_found("CAPA", id))
}

/// Find the open alert raised for an inspection, if one exists.
/// The inspection is already tenant-scoped by its loader, so the
/// alert found through it belongs to the same tenant; the tenant
/// filter here is defence in depth.
pub(crate) fn alert_for_inspection(
    state: &QualityState,
    inspection_id: Uuid,
    tenant_id: &str,
) -> Option<QualityAlert> {
    state
        .alerts
        .iter()
        .map(|entry| entry.value().clone())
        .find(|row| {
            row.inspection_id == inspection_id
                && row.status != ALERT_RESOLVED
                && tenant_matches(&row.tenant_id, tenant_id)
        })
}

/// True when the target is currently blocked by an open blocking
/// alert. Owning modules consult this (through `GET /blocks` or
/// the `quality.alert.raised` event) before letting a blocked
/// record proceed. Scoped to the caller's tenant: another
/// tenant's block never blocks this tenant's record.
pub(crate) fn target_is_blocked(
    state: &QualityState,
    target_ref: &str,
    tenant_id: &str,
) -> bool {
    state.alerts.iter().any(|entry| {
        let row = entry.value();
        row.target_ref == target_ref
            && row.blocked
            && row.status != ALERT_RESOLVED
            && tenant_matches(&row.tenant_id, tenant_id)
    })
}

#[cfg(test)]
mod tests {
    use super::load_inspection;
    use super::load_point;
    use crate::modules::quality::Inspection;
    use crate::modules::quality::QualityPoint;
    use crate::modules::quality::QualityState;
    use chrono::Utc;
    use uuid::Uuid;

    fn point(tenant_id: Option<String>) -> QualityPoint {
        let now = Utc::now();
        QualityPoint {
            id: Uuid::new_v4(),
            name: "Point".to_string(),
            description: String::new(),
            scope: "product".to_string(),
            target_ref: None,
            target_kind: None,
            check_kind: "visual".to_string(),
            on_fail_policy: "alert_only".to_string(),
            alert_severity: "medium".to_string(),
            active: true,
            company_id: None,
            tenant_id,
            created_at: now,
            updated_at: now,
        }
    }

    fn inspection(tenant_id: Option<String>) -> Inspection {
        let now = Utc::now();
        Inspection {
            id: Uuid::new_v4(),
            point_id: Uuid::new_v4(),
            target_ref: "target-1".to_string(),
            target_kind: "product".to_string(),
            inspector_id: None,
            status: "requested".to_string(),
            result: None,
            measurements: Vec::new(),
            failure_causes: Vec::new(),
            notes: None,
            evidence_refs: Vec::new(),
            inspected_at: None,
            company_id: None,
            tenant_id,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn loader_serves_own_tenant_record() {
        let state = QualityState::default();
        let row = point(Some("tenant-a".to_string()));
        state.points.insert(row.id, row.clone());
        assert_eq!(
            load_point(&state, &row.id.to_string(), "tenant-a")
                .unwrap()
                .id,
            row.id
        );
    }

    #[test]
    fn loader_hides_other_tenant_record_as_not_found() {
        let state = QualityState::default();
        let row = point(Some("tenant-a".to_string()));
        state.points.insert(row.id, row.clone());
        assert!(load_point(&state, &row.id.to_string(), "tenant-b").is_err());
    }

    #[test]
    fn loader_serves_legacy_unscoped_record_to_any_tenant() {
        let state = QualityState::default();
        let row = inspection(None);
        state.inspections.insert(row.id, row.clone());
        assert!(load_inspection(&state, &row.id.to_string(), "tenant-a").is_ok());
        assert!(load_inspection(&state, &row.id.to_string(), "tenant-b").is_ok());
    }

    #[test]
    fn loader_rejects_unknown_id() {
        let state = QualityState::default();
        assert!(load_point(&state, &Uuid::new_v4().to_string(), "tenant-a").is_err());
    }
}
