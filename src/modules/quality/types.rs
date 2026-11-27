//! quality module - Settings, points, criteria, inspections, alerts and CAPAs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Configurable settings (admin-editable; defaults are defaults only)

/// All behaviour-shaping lists and defaults of this module. An
/// administrator edits these through `PUT /settings`; every
/// validation and default in this file reads the live value, so
/// changing a list here changes what the module accepts without
/// any code change. The values below are CONFIGURABLE DEFAULTS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualitySettings {
    /// Scopes a quality point may attach to. A scope names the
    /// kind of record the point checks (a product, an operation,
    /// a supplier, an incoming-goods receipt, a work order, a
    /// shipment). Admins add new scopes here; the module never
    /// invents one.
    pub point_scopes: Vec<String>,
    /// Kinds of check a point performs.
    pub check_kinds: Vec<String>,
    /// Kinds of criterion a point may define.
    pub criterion_kinds: Vec<String>,
    /// Fail policies a point may choose. `none` records the
    /// result only, `alert_only` raises an alert, `block`
    /// raises an alert and blocks the related record until the
    /// alert is resolved.
    pub fail_policies: Vec<String>,
    /// Result vocabulary an inspection may carry.
    pub inspection_results: Vec<String>,
    /// Failure causes an inspector or a criterion may name.
    /// A criterion names its own cause so a failed measurement
    /// is classified by the definition, not by code.
    pub failure_causes: Vec<String>,
    /// CAPA lifecycle states, in order.
    pub capa_states: Vec<String>,
    /// Alert severities.
    pub alert_severities: Vec<String>,
    /// Fail policy used when a point is created without one.
    pub default_fail_policy: String,
    /// Alert severity used when a point is created without one.
    pub default_alert_severity: String,
    /// Default look-back window in days for the supplier score.
    pub default_score_window_days: i64,
    /// Minimum completed inspections before a supplier score is
    /// reported instead of `insufficient_data`.
    pub default_min_inspections_for_score: i64,
    /// Points subtracted from 100 per failed inspection in the
    /// supplier score.
    pub fail_penalty_points: f64,
    /// Points subtracted from 100 per conditional inspection in
    /// the supplier score.
    pub conditional_penalty_points: f64,
    /// When true, a CAPA must pass through `verified` (with
    /// verification notes) before it may close.
    pub capa_verification_required: bool,
    /// Maximum criteria one quality point may hold.
    pub max_criteria_per_point: usize,
    /// Maximum measurements one inspection submission may carry.
    pub max_measurements_per_inspection: usize,
    /// Maximum evidence references one inspection may carry.
    pub max_evidence_refs: usize,
}

impl Default for QualitySettings {
    fn default() -> Self {
        Self {
            point_scopes: vec![
                "product".to_string(),
                "operation".to_string(),
                "supplier".to_string(),
                "incoming_goods".to_string(),
                "work_order".to_string(),
                "shipment".to_string(),
            ],
            check_kinds: vec![
                "measurement".to_string(),
                "visual".to_string(),
                "functional".to_string(),
                "document".to_string(),
            ],
            criterion_kinds: vec![
                "numeric".to_string(),
                "boolean".to_string(),
                "text".to_string(),
            ],
            fail_policies: vec![
                "none".to_string(),
                "alert_only".to_string(),
                "block".to_string(),
            ],
            inspection_results: vec![
                "pass".to_string(),
                "fail".to_string(),
                "conditional".to_string(),
            ],
            failure_causes: vec![
                "dimension_out_of_tolerance".to_string(),
                "surface_defect".to_string(),
                "missing_component".to_string(),
                "wrong_material".to_string(),
                "documentation_missing".to_string(),
                "contamination".to_string(),
                "functional_failure".to_string(),
                "other".to_string(),
            ],
            capa_states: vec![
                "open".to_string(),
                "in_progress".to_string(),
                "verified".to_string(),
                "closed".to_string(),
            ],
            alert_severities: vec![
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
                "critical".to_string(),
            ],
            default_fail_policy: "alert_only".to_string(),
            default_alert_severity: "medium".to_string(),
            default_score_window_days: 90,
            default_min_inspections_for_score: 5,
            fail_penalty_points: 10.0,
            conditional_penalty_points: 4.0,
            capa_verification_required: true,
            max_criteria_per_point: 50,
            max_measurements_per_inspection: 200,
            max_evidence_refs: 20,
        }
    }
}

// Types

/// One quality point: a control-point definition attached to a
/// scope (product, operation, supplier, incoming goods, work
/// order, shipment) and optionally to one concrete target
/// (`target_ref`, for example a product id or a supplier id; None
/// means the point applies to every target in its scope).
/// `on_fail_policy` decides what a failed inspection does:
/// `none`, `alert_only`, or `block`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityPoint {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_kind: Option<String>,
    pub check_kind: String,
    pub on_fail_policy: String,
    pub alert_severity: String,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    /// Tenant that owns this record, taken from the resolved
    /// tenant context at creation time (never from a client
    /// header). Reads are scoped to it; `None` marks a record
    /// created before tenant scoping (or by an event without a
    /// tenant), which stays visible until it is backfilled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One criterion of a quality point: what is checked and how the
/// measurement is judged. The tolerance lives here as data, never
/// in code. Numeric criteria judge `value_number` against
/// `min_value` / `max_value` (at least one bound is required);
/// boolean criteria judge `value_bool` against `expected_bool`;
/// text criteria judge `value_text` against `expected_text`
/// (None means any non-empty text passes). `failure_cause` names
/// the cause recorded when this criterion fails; `weight` is
/// kept for reporting and future weighted scoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityCriterion {
    pub id: Uuid,
    pub point_id: Uuid,
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_value: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_value: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_bool: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_cause: Option<String>,
    pub weight: f64,
    pub required: bool,
    /// Tenant that owns this record; see `QualityPoint::tenant_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One recorded measurement inside an inspection, already
/// evaluated: `passed` is the verdict for this criterion and
/// `deviation` is the numeric distance outside the band (None
/// when inside the band or when the criterion is not numeric).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Measurement {
    pub criterion_id: Uuid,
    pub criterion_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_number: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_bool: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_text: Option<String>,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deviation: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One inspection run against a quality point. It starts as
/// `requested` (by an API call or by an ERP event), becomes
/// `completed` when measurements are submitted, and may be
/// `cancelled` while still requested. `result` is set on
/// completion. `evidence_refs` are opaque storage / document
/// references only - the bytes live in the storage and documents
/// modules, never here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inspection {
    pub id: Uuid,
    pub point_id: Uuid,
    pub target_ref: String,
    pub target_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspector_id: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default)]
    pub measurements: Vec<Measurement>,
    #[serde(default)]
    pub failure_causes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspected_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    /// Tenant that owns this record; see `QualityPoint::tenant_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One quality alert, raised when an inspection fails under a
/// point whose fail policy is `alert_only` or `block`. When
/// `blocked` is true the related record (`target_ref`) must not
/// proceed until this alert is resolved; `GET /blocks` lists
/// exactly the open alerts with `blocked = true`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityAlert {
    pub id: Uuid,
    pub inspection_id: Uuid,
    pub point_id: Uuid,
    pub target_ref: String,
    pub target_kind: String,
    pub severity: String,
    pub status: String,
    pub message: String,
    pub blocked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    /// Tenant that owns this record; see `QualityPoint::tenant_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One CAPA record linked to a failed inspection (and, when one
/// exists, to its alert). State machine: `open` -> `in_progress`
/// -> `verified` -> `closed`; when verification is required in
/// settings, closing without passing through `verified` is
/// rejected. Verification records who confirmed the fix worked
/// and how (`verification_notes`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capa {
    pub id: Uuid,
    pub inspection_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alert_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_cause: Option<String>,
    pub owner_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<DateTime<Utc>>,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<DateTime<Utc>>,
    /// Tenant that owns this record; see `QualityPoint::tenant_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// Request bodies

#[derive(Debug, Deserialize)]
pub struct CreatePointInput {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub scope: String,
    #[serde(default)]
    pub target_ref: Option<String>,
    #[serde(default)]
    pub target_kind: Option<String>,
    pub check_kind: String,
    #[serde(default)]
    pub on_fail_policy: Option<String>,
    #[serde(default)]
    pub alert_severity: Option<String>,
    #[serde(default)]
    pub company_id: Option<String>,
}

/// Nullable-update semantics: every clearable field is
/// `Option<Option<T>>` - absent means "leave unchanged", `null`
/// means "clear this value", a value means "set it". Plain
/// `Option<T>` could only set, never clear, so changing a
/// criterion's kind (for example numeric to boolean) used to fail
/// validation against stale bounds that could not be removed.
#[derive(Debug, Deserialize)]
pub struct UpdatePointInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub target_ref: Option<Option<String>>,
    #[serde(default)]
    pub target_kind: Option<Option<String>>,
    #[serde(default)]
    pub check_kind: Option<String>,
    #[serde(default)]
    pub on_fail_policy: Option<String>,
    #[serde(default)]
    pub alert_severity: Option<String>,
    #[serde(default)]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCriterionInput {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub min_value: Option<f64>,
    #[serde(default)]
    pub max_value: Option<f64>,
    #[serde(default)]
    pub expected_bool: Option<bool>,
    #[serde(default)]
    pub expected_text: Option<String>,
    #[serde(default)]
    pub failure_cause: Option<String>,
    #[serde(default = "default_weight")]
    pub weight: f64,
    #[serde(default = "default_required")]
    pub required: bool,
}

pub(crate) fn default_weight() -> f64 {
    1.0
}

pub(crate) fn default_required() -> bool {
    true
}

/// Nullable-update semantics, as on `UpdatePointInput`: absent
/// means "leave unchanged", `null` means "clear this value", a
/// value means "set it". Clearing is what makes a kind change
/// possible - a numeric criterion becoming boolean must clear
/// `min_value` / `max_value`, and a boolean one becoming numeric
/// must clear `expected_bool`.
#[derive(Debug, Deserialize)]
pub struct UpdateCriterionInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub unit: Option<Option<String>>,
    #[serde(default)]
    pub min_value: Option<Option<f64>>,
    #[serde(default)]
    pub max_value: Option<Option<f64>>,
    #[serde(default)]
    pub expected_bool: Option<Option<bool>>,
    #[serde(default)]
    pub expected_text: Option<Option<String>>,
    #[serde(default)]
    pub failure_cause: Option<Option<String>>,
    #[serde(default)]
    pub weight: Option<f64>,
    #[serde(default)]
    pub required: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateInspectionInput {
    pub point_id: String,
    pub target_ref: String,
    pub target_kind: String,
    #[serde(default)]
    pub inspector_id: Option<String>,
    #[serde(default)]
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MeasurementInput {
    pub criterion_id: String,
    #[serde(default)]
    pub value_number: Option<f64>,
    #[serde(default)]
    pub value_bool: Option<bool>,
    #[serde(default)]
    pub value_text: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SubmitInspectionInput {
    #[serde(default)]
    pub measurements: Vec<MeasurementInput>,
    #[serde(default)]
    pub failure_causes: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    /// Request a `conditional` result instead of `pass` when
    /// every criterion passed but the inspector still has a
    /// reservation. Requires `notes` (the justification).
    /// Ignored when any criterion failed.
    #[serde(default)]
    pub request_conditional: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateCapaInput {
    pub inspection_id: String,
    #[serde(default)]
    pub alert_id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub root_cause: Option<String>,
    pub owner_id: String,
    #[serde(default)]
    pub due_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCapaInput {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub root_cause: Option<String>,
    #[serde(default)]
    pub owner_id: Option<String>,
    #[serde(default)]
    pub due_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct TransitionCapaInput {
    pub to_state: String,
    #[serde(default)]
    pub verification_notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsInput {
    #[serde(default)]
    pub point_scopes: Option<Vec<String>>,
    #[serde(default)]
    pub check_kinds: Option<Vec<String>>,
    #[serde(default)]
    pub criterion_kinds: Option<Vec<String>>,
    #[serde(default)]
    pub fail_policies: Option<Vec<String>>,
    #[serde(default)]
    pub inspection_results: Option<Vec<String>>,
    #[serde(default)]
    pub failure_causes: Option<Vec<String>>,
    #[serde(default)]
    pub capa_states: Option<Vec<String>>,
    #[serde(default)]
    pub alert_severities: Option<Vec<String>>,
    #[serde(default)]
    pub default_fail_policy: Option<String>,
    #[serde(default)]
    pub default_alert_severity: Option<String>,
    #[serde(default)]
    pub default_score_window_days: Option<i64>,
    #[serde(default)]
    pub default_min_inspections_for_score: Option<i64>,
    #[serde(default)]
    pub fail_penalty_points: Option<f64>,
    #[serde(default)]
    pub conditional_penalty_points: Option<f64>,
    #[serde(default)]
    pub capa_verification_required: Option<bool>,
    #[serde(default)]
    pub max_criteria_per_point: Option<usize>,
    #[serde(default)]
    pub max_measurements_per_inspection: Option<usize>,
    #[serde(default)]
    pub max_evidence_refs: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::UpdateCriterionInput;
    use super::UpdatePointInput;

    #[test]
    fn criterion_update_absent_means_unchanged() {
        let input: UpdateCriterionInput = serde_json::from_str("{}").unwrap();
        assert!(input.min_value.is_none());
        assert!(input.unit.is_none());
        assert!(input.expected_bool.is_none());
    }

    #[test]
    fn criterion_update_null_means_clear() {
        let input: UpdateCriterionInput =
            serde_json::from_str(r#"{"min_value": null, "max_value": null, "unit": null}"#)
                .unwrap();
        assert_eq!(input.min_value, Some(None));
        assert_eq!(input.max_value, Some(None));
        assert_eq!(input.unit, Some(None));
    }

    #[test]
    fn criterion_update_value_means_set() {
        let input: UpdateCriterionInput =
            serde_json::from_str(r#"{"min_value": 1.5, "expected_bool": true}"#).unwrap();
        assert_eq!(input.min_value, Some(Some(1.5)));
        assert_eq!(input.expected_bool, Some(Some(true)));
    }

    #[test]
    fn point_update_null_clears_target() {
        let input: UpdatePointInput =
            serde_json::from_str(r#"{"target_ref": null, "target_kind": null}"#).unwrap();
        assert_eq!(input.target_ref, Some(None));
        assert_eq!(input.target_kind, Some(None));
        let kept: UpdatePointInput =
            serde_json::from_str(r#"{"target_ref": "prod-1"}"#).unwrap();
        assert_eq!(input.target_ref, Some(None));
        assert_eq!(kept.target_ref, Some(Some("prod-1".to_string())));
        assert!(kept.target_kind.is_none());
    }
}
