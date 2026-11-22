// Quality Management module — quality points, inspections,
// quality alerts, CAPA (corrective and preventive actions), and
// quality reports, in the style of a manufacturing / ERP quality
// system.
//
// Routes live under `/api/v1/quality`:
//
//   Quality points (control-point definitions):
//     GET/POST /points — list (paginated, filter by scope,
//          check_kind, on_fail_policy, active, target_ref) / create.
//     GET/PATCH/DELETE /points/{id}.
//     POST /points/{id}/activate — active = true.
//     POST /points/{id}/deactivate — active = false.
//   Criteria (what one point checks):
//     GET/POST /points/{id}/criteria — list / add a criterion.
//     PATCH/DELETE /criteria/{id}.
//   Inspections:
//     GET/POST /inspections — list (paginated, filter by point_id,
//          target_ref, target_kind, status, result, inspector_id)
//          / request a new inspection.
//     GET /inspections/{id}.
//     POST /inspections/{id}/submit — submit measurements; the
//          module evaluates every criterion against its stored
//          tolerance band and records pass / fail / conditional.
//     POST /inspections/{id}/cancel — requested -> cancelled.
//   Alerts:
//     GET /alerts — list (paginated, filter by status, severity,
//          point_id, target_ref, blocked).
//     GET /alerts/{id}.
//     POST /alerts/{id}/acknowledge — open -> acknowledged.
//     POST /alerts/{id}/resolve — open/acknowledged -> resolved;
//          resolving a blocking alert unblocks its target.
//   Blocks:
//     GET /blocks — open blocking alerts, i.e. the targets that
//          are currently blocked by a failed inspection
//          (filter by target_ref / target_kind).
//   CAPA:
//     GET/POST /capas — list (paginated, filter by state,
//          owner_id, inspection_id, overdue) / create.
//     GET/PATCH /capas/{id}.
//     POST /capas/{id}/transition — move the state machine
//          (open -> in_progress -> verified -> closed).
//   Reports:
//     GET /reports/pass-rate — pass rate per product / point /
//          period, with the method shown in the response.
//     GET /reports/failure-causes — top failure causes.
//     GET /reports/backlog — open alerts, blocking alerts,
//          open and overdue CAPAs, requested inspections.
//     GET /reports/supplier-score — supplier quality score
//          computed from inspection history, with every input
//          and the formula shown in the response.
//   Settings:
//     GET/PUT /settings — the configurable defaults (manager).
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory quality point / criterion / inspection / alert /
// CAPA management; tolerance evaluation from stored criterion
// data (numeric bands, boolean expectations, text expectations);
// failure alerts with a per-point blocking policy; the CAPA
// state machine with verification before closure; pass-rate,
// failure-cause, backlog, and supplier-score reports computed
// from the stored inspection history with the method and the
// inputs returned alongside the numbers; event wiring that
// answers inspection requests from manufacturing, inventory,
// procurement, and logistics and emits completion, alert, and
// CAPA events. What is NOT built here: durable storage (the
// DashMap stores are authoritative; migration
// `2026-11-22-000082_quality` is the persistence target);
// enforcement inside the manufacturing, inventory, procurement,
// or logistics modules themselves — this module records a block
// and emits `quality.alert.raised`; the owning module must
// consult `GET /blocks` (or the event) before it lets a blocked
// record proceed; photo/evidence bytes (evidence references are
// opaque strings handed to the storage / documents modules —
// this module never stores bytes); and notification delivery
// itself (the notifications module owns delivery — this module
// only emits `notification.requested`).
//
// CONFIG-DRIVEN — READ THIS FIRST. Nothing behavioural is
// hard-coded: the allowed point scopes, check kinds, criterion
// kinds, fail policies, inspection results, failure causes,
// CAPA states, and alert severities, the default fail policy
// and alert severity, the supplier-score window, minimum
// inspection count, and penalty points, and the CAPA
// verification switch all live in `QualitySettings`, editable
// by an administrator through `PUT /settings`. The values in
// `QualitySettings::default` are CONFIGURABLE DEFAULTS only —
// an admin change applies to every validation and default that
// reads them, and no code path consults a baked-in list instead.
// A numeric tolerance band lives on the criterion row itself,
// never in code. Blocking is a per-point policy (`none`,
// `alert_only`, `block`), never automatic everywhere.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `quality.point.created`, `quality.point.updated`,
//     `quality.point.deleted`, `quality.point.activated`,
//     `quality.point.deactivated`
//   - `quality.criterion.created`, `quality.criterion.updated`,
//     `quality.criterion.deleted`
//   - `quality.inspection.requested`,
//     `quality.inspection.completed`
//     (`{"inspection_id", "point_id", "target_ref",
//     "target_kind", "result", "inspector_id", "blocked"}`),
//     `quality.inspection.cancelled`
//   - `quality.alert.raised`
//     (`{"alert_id", "inspection_id", "point_id", "target_ref",
//     "target_kind", "severity", "blocked"}`),
//     `quality.alert.acknowledged`, `quality.alert.resolved`
//   - `quality.capa.created`, `quality.capa.updated`,
//     `quality.capa.transitioned`, `quality.capa.closed`
//     (`{"capa_id", "inspection_id", "state"}`)
//   - `notification.requested` (failed inspection: the alert
//     is sent to the inspector in the notifications module's
//     payload shape).
// Events this module listens to (all read defensively — a missing
// field, an unknown point, or no matching active point is skipped,
// never an error):
//   - `manufacturing.inspection.requested` — creates a requested
//     inspection for the named point, or for every active point
//     whose scope and target match the payload.
//   - `inventory.goods_received` — incoming-goods inspection:
//     the same matching rule with scope `incoming_goods`.
//   - `procurement.supplier_quality_check.requested` — supplier
//     quality: the same matching rule with scope `supplier`.
//   - `logistics.shipment.inspection.requested` — shipment
//     inspection: the same matching rule with scope `shipment`.
//   - `quality.blocks.query` — answered by this module: the hook
//     form of `GET /blocks`. Given `{"target_ref", "target_kind"}`
//     it answers with the blocking alerts covering that target
//     (`{"target_ref", "target_kind", "blocks": [...],
//     "blocked": true/false}`); owning modules consult it before
//     letting a blocked record proceed.
//
// Security: every endpoint requires a capability (`quality.view`
// for reads, `quality.manage` for definitions, alerts, CAPA, and
// settings, `quality.inspect` for requesting and submitting
// inspections). All input is validated (lengths, UUIDs, settings
// lists, numeric ranges); output travels through the standard
// JSON envelope, so no raw markup is ever rendered from user
// input. No raw SQL anywhere — the module holds no database
// handle at all; the migration is the persistence target only.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::tenancy::TenantContext as TenancyContext;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_TITLE_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_SLUG_LEN: usize = 64;
const MAX_REF_LEN: usize = 500;
const MAX_TEXT_LEN: usize = 20000;
const MAX_NOTE_LEN: usize = 2000;

/// Capability names checked through the `role.check` hook.
const PERM_QUALITY_VIEW: &str = "quality.view";
const PERM_QUALITY_MANAGE: &str = "quality.manage";
const PERM_QUALITY_INSPECT: &str = "quality.inspect";

/// Event names this module listens to.
const MANUFACTURING_INSPECTION_REQUESTED: &str = "manufacturing.inspection.requested";
const INVENTORY_GOODS_RECEIVED: &str = "inventory.goods_received";
const PROCUREMENT_QUALITY_CHECK_REQUESTED: &str =
    "procurement.supplier_quality_check.requested";
const LOGISTICS_SHIPMENT_INSPECTION_REQUESTED: &str = "logistics.shipment.inspection.requested";
/// Block query (hook form of `GET /blocks`): owning modules ask
/// whether a target is blocked before letting it proceed.
const QUALITY_BLOCKS_QUERY: &str = "quality.blocks.query";

/// Inspection lifecycle statuses. These are the stored status
/// values; the allowed transitions live in
/// `valid_inspection_transition`.
const INSP_REQUESTED: &str = "requested";
const INSP_COMPLETED: &str = "completed";
const INSP_CANCELLED: &str = "cancelled";

/// Inspection result values. The vocabulary is the live settings
/// list; these constants are the values the evaluation logic
/// produces, and they are only accepted when the settings list
/// contains them (validated at evaluation time).
const RESULT_PASS: &str = "pass";
const RESULT_FAIL: &str = "fail";
const RESULT_CONDITIONAL: &str = "conditional";

/// Alert lifecycle statuses.
const ALERT_OPEN: &str = "open";
const ALERT_ACKNOWLEDGED: &str = "acknowledged";
const ALERT_RESOLVED: &str = "resolved";

/// CAPA states. The vocabulary is the live settings list; the
/// allowed transitions live in `valid_capa_transition`.
const CAPA_OPEN: &str = "open";
const CAPA_IN_PROGRESS: &str = "in_progress";
const CAPA_VERIFIED: &str = "verified";
const CAPA_CLOSED: &str = "closed";

// ---------------------------------------------------------------------------
// Configurable settings (admin-editable; defaults are defaults only)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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
/// references only — the bytes live in the storage and documents
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

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

#[derive(Debug, Deserialize)]
pub struct UpdatePointInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub target_ref: Option<String>,
    #[serde(default)]
    pub target_kind: Option<String>,
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

fn default_weight() -> f64 {
    1.0
}

fn default_required() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct UpdateCriterionInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
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

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct QualityState {
    pub points: Arc<DashMap<Uuid, QualityPoint>>,
    pub criteria: Arc<DashMap<Uuid, QualityCriterion>>,
    pub inspections: Arc<DashMap<Uuid, Inspection>>,
    pub alerts: Arc<DashMap<Uuid, QualityAlert>>,
    pub capas: Arc<DashMap<Uuid, Capa>>,
    pub settings: Arc<RwLock<QualitySettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for QualityState {
    fn default() -> Self {
        Self {
            points: Arc::new(DashMap::new()),
            criteria: Arc::new(DashMap::new()),
            inspections: Arc::new(DashMap::new()),
            alerts: Arc::new(DashMap::new()),
            capas: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(QualitySettings::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl QualityState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn settings(&self) -> QualitySettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "quality",
                "category": "quality",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    fn criteria_for_point(&self, point_id: Uuid) -> Vec<QualityCriterion> {
        let mut rows: Vec<QualityCriterion> = self
            .criteria
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.point_id == point_id)
            .collect();
        rows.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        rows
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the events module.
struct Caller {
    actor: String,
    role: String,
}

/// Resolve the tenant context for a request. THE single
/// convergence point for tenant-scoped data access in this file
/// (see `crate::core::tenancy`): this function returns the tenancy
/// `TenantContext` (tenant id as Uuid + schema name derived only
/// by `tenancy::schema_name()`), and the Diesel persistence wiring
/// — when it lands — takes the Uuid from here into
/// `tenancy::with_tenant_connection`. Fail closed:
/// `from_http_request` answers 401 when the request did not
/// resolve to a tenant and 403 when the resolved tenant id is not
/// a valid UUID; no resolved tenant means the request is refused
/// here, never served from `public`.
///
/// OPEN (not closed by this guard): the in-memory DashMap stores
/// in this module carry no `tenant_id` on their records yet, so
/// there is no per-record tenant filtering to apply once the
/// context is resolved — that needs a store-shape change (tenant
/// id on every record + a migration column) and is integrator
/// work. What this guard does close is the context requirement
/// itself: every tenant-data endpoint resolves the tenant first
/// through `caller()`, and a request with no tenant context is
/// rejected before any business data is read.
fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<TenancyContext> {
    TenancyContext::from_http_request(req)
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    // Tenant first: no resolved tenant context means fail closed,
    // never fall back to the public schema. The resolved context
    // is bound for the Diesel persistence layer (see
    // `resolve_tenant`); the in-memory stores below carry no
    // tenant id yet, so per-record filtering is still open.
    let _tenant = resolve_tenant(req)?;
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the events module's capability check).
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// True when the caller may inspect: the manage capability or the
/// inspector capability.
fn can_inspect(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_QUALITY_MANAGE)
        || check_capability(ctx, role, PERM_QUALITY_INSPECT)
}

fn require_inspect(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    if can_inspect(ctx, role) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{PERM_QUALITY_MANAGE}\" or \"{PERM_QUALITY_INSPECT}\""
        )))
    }
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

/// Validate a value against a settings list. The list is the live
/// admin-configured one — never a constant in this file.
fn require_in_list(value: &str, field: &str, allowed: &[String]) -> UnifiedResult<String> {
    let checked = require_text(value, field, MAX_SLUG_LEN)?;
    if !allowed.iter().any(|item| item == &checked) {
        return Err(UnifiedError::bad_request(format!(
            "{field} \"{checked}\" is not allowed; allowed values: {}",
            allowed.join(", ")
        )));
    }
    Ok(checked)
}

/// Validate a whole list of values against a settings list.
fn require_all_in_list(
    values: &[String],
    field: &str,
    allowed: &[String],
) -> UnifiedResult<Vec<String>> {
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        out.push(require_in_list(value, field, allowed)?);
    }
    Ok(out)
}

/// Validate an admin-supplied settings list: non-empty, each item
/// a short slug, no duplicates.
fn require_settings_list(values: &[String], field: &str) -> UnifiedResult<Vec<String>> {
    if values.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
    }
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let checked = require_text(value, field, MAX_SLUG_LEN)?;
        if out.contains(&checked) {
            return Err(UnifiedError::bad_request(format!(
                "{field} contains duplicate value \"{checked}\""
            )));
        }
        out.push(checked);
    }
    Ok(out)
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn parse_date_filter(raw: &str, field: &str) -> UnifiedResult<DateTime<Utc>> {
    raw.parse::<DateTime<Utc>>()
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be an RFC 3339 timestamp")))
}

/// Validate one criterion definition against the live settings.
/// The tolerance shape is checked per kind: numeric criteria need
/// at least one bound and no boolean/text expectation; boolean
/// criteria need `expected_bool`; text criteria need no numeric
/// bounds. A min above a max is rejected.
fn validate_criterion_shape(
    kind: &str,
    min_value: Option<f64>,
    max_value: Option<f64>,
    expected_bool: Option<bool>,
    expected_text: &Option<String>,
    weight: f64,
    settings: &QualitySettings,
) -> UnifiedResult<()> {
    match kind {
        "numeric" => {
            if min_value.is_none() && max_value.is_none() {
                return Err(UnifiedError::bad_request(
                    "A numeric criterion needs at least one of min_value / max_value",
                ));
            }
            if let (Some(min), Some(max)) = (min_value, max_value) {
                if min > max {
                    return Err(UnifiedError::bad_request(
                        "min_value must not be greater than max_value",
                    ));
                }
            }
            if expected_bool.is_some() || expected_text.is_some() {
                return Err(UnifiedError::bad_request(
                    "A numeric criterion must not carry expected_bool / expected_text",
                ));
            }
        }
        "boolean" => {
            if expected_bool.is_none() {
                return Err(UnifiedError::bad_request(
                    "A boolean criterion needs expected_bool",
                ));
            }
            if min_value.is_some() || max_value.is_some() || expected_text.is_some() {
                return Err(UnifiedError::bad_request(
                    "A boolean criterion must not carry min_value / max_value / expected_text",
                ));
            }
        }
        "text" => {
            if min_value.is_some() || max_value.is_some() || expected_bool.is_some() {
                return Err(UnifiedError::bad_request(
                    "A text criterion must not carry min_value / max_value / expected_bool",
                ));
            }
        }
        _ => {
            return Err(UnifiedError::bad_request(format!(
                "Unknown criterion kind \"{kind}\"; allowed kinds: {}",
                settings.criterion_kinds.join(", ")
            )));
        }
    }
    if !weight.is_finite() || weight <= 0.0 {
        return Err(UnifiedError::bad_request("weight must be a positive number"));
    }
    Ok(())
}

/// Evaluate one measurement against its criterion. Returns the
/// evaluated measurement plus the failure cause to record when
/// the criterion failed (the criterion's own configured cause).
/// A missing measurement for a required criterion fails; a
/// missing measurement for an optional criterion is skipped
/// (returns None).
fn evaluate_measurement(
    criterion: &QualityCriterion,
    input: Option<&MeasurementInput>,
) -> UnifiedResult<Option<(Measurement, Option<String>)>> {
    let Some(input) = input else {
        if criterion.required {
            let measurement = Measurement {
                criterion_id: criterion.id,
                criterion_name: criterion.name.clone(),
                value_number: None,
                value_bool: None,
                value_text: None,
                passed: false,
                deviation: None,
                note: Some("Missing required measurement".to_string()),
            };
            return Ok(Some((measurement, criterion.failure_cause.clone())));
        }
        return Ok(None);
    };
    let note = optional_text(&input.note, "measurement note", MAX_NOTE_LEN)?;
    let (passed, deviation) = match criterion.kind.as_str() {
        "numeric" => {
            let value = input.value_number.ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Criterion \"{}\" needs a numeric value",
                    criterion.name
                ))
            })?;
            if !value.is_finite() {
                return Err(UnifiedError::bad_request(format!(
                    "Criterion \"{}\" needs a finite numeric value",
                    criterion.name
                )));
            }
            let below = criterion.min_value.map_or(false, |min| value < min);
            let above = criterion.max_value.map_or(false, |max| value > max);
            let deviation = if below {
                Some(value - criterion.min_value.unwrap_or(value))
            } else if above {
                Some(value - criterion.max_value.unwrap_or(value))
            } else {
                None
            };
            (!below && !above, deviation)
        }
        "boolean" => {
            let value = input.value_bool.ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Criterion \"{}\" needs a boolean value",
                    criterion.name
                ))
            })?;
            let expected = criterion.expected_bool.unwrap_or(false);
            (value == expected, None)
        }
        "text" => {
            let raw = input.value_text.as_deref().unwrap_or("").trim();
            if raw.is_empty() {
                return Err(UnifiedError::bad_request(format!(
                    "Criterion \"{}\" needs a text value",
                    criterion.name
                )));
            }
            let passed = match &criterion.expected_text {
                Some(expected) => raw.eq_ignore_ascii_case(expected.trim()),
                None => true,
            };
            (passed, None)
        }
        other => {
            return Err(UnifiedError::bad_request(format!(
                "Unknown criterion kind \"{other}\""
            )));
        }
    };
    let measurement = Measurement {
        criterion_id: criterion.id,
        criterion_name: criterion.name.clone(),
        value_number: input.value_number,
        value_bool: input.value_bool,
        value_text: input
            .value_text
            .as_ref()
            .map(|value| value.trim().to_string()),
        passed,
        deviation,
        note,
    };
    let cause = if passed {
        None
    } else {
        criterion.failure_cause.clone()
    };
    Ok(Some((measurement, cause)))
}

/// Allowed inspection status transitions. Anything not listed is
/// rejected with a 422 so an inspection can never jump backwards.
fn valid_inspection_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        (INSP_REQUESTED, INSP_COMPLETED) | (INSP_REQUESTED, INSP_CANCELLED)
    )
}

/// Allowed alert status transitions.
fn valid_alert_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        (ALERT_OPEN, ALERT_ACKNOWLEDGED)
            | (ALERT_OPEN, ALERT_RESOLVED)
            | (ALERT_ACKNOWLEDGED, ALERT_RESOLVED)
    )
}

/// Allowed CAPA state transitions. When verification is required,
/// `in_progress` may only move to `verified`; when it is not
/// required, `in_progress` may also close directly.
fn valid_capa_transition(from: &str, to: &str, verification_required: bool) -> bool {
    match (from, to) {
        (CAPA_OPEN, CAPA_IN_PROGRESS) => true,
        (CAPA_IN_PROGRESS, CAPA_VERIFIED) => true,
        (CAPA_VERIFIED, CAPA_CLOSED) => true,
        (CAPA_IN_PROGRESS, CAPA_CLOSED) => !verification_required,
        _ => false,
    }
}

// --- Loaders ---------------------------------------------------------------

fn load_point(state: &QualityState, raw_id: &str) -> UnifiedResult<QualityPoint> {
    let id = parse_uuid(raw_id, "quality point")?;
    state
        .points
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Quality point not found: {id}")))
}

fn load_criterion(state: &QualityState, raw_id: &str) -> UnifiedResult<QualityCriterion> {
    let id = parse_uuid(raw_id, "criterion")?;
    state
        .criteria
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Criterion not found: {id}")))
}

fn load_inspection(state: &QualityState, raw_id: &str) -> UnifiedResult<Inspection> {
    let id = parse_uuid(raw_id, "inspection")?;
    state
        .inspections
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Inspection not found: {id}")))
}

fn load_alert(state: &QualityState, raw_id: &str) -> UnifiedResult<QualityAlert> {
    let id = parse_uuid(raw_id, "alert")?;
    state
        .alerts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Quality alert not found: {id}")))
}

fn load_capa(state: &QualityState, raw_id: &str) -> UnifiedResult<Capa> {
    let id = parse_uuid(raw_id, "CAPA")?;
    state
        .capas
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("CAPA not found: {id}")))
}

/// Find the open alert raised for an inspection, if one exists.
fn alert_for_inspection(state: &QualityState, inspection_id: Uuid) -> Option<QualityAlert> {
    state
        .alerts
        .iter()
        .map(|entry| entry.value().clone())
        .find(|row| row.inspection_id == inspection_id && row.status != ALERT_RESOLVED)
}

/// True when the target is currently blocked by an open blocking
/// alert. Owning modules consult this (through `GET /blocks` or
/// the `quality.alert.raised` event) before letting a blocked
/// record proceed.
fn target_is_blocked(state: &QualityState, target_ref: &str) -> bool {
    state.alerts.iter().any(|entry| {
        let row = entry.value();
        row.target_ref == target_ref && row.blocked && row.status != ALERT_RESOLVED
    })
}

// ---------------------------------------------------------------------------
// Quality point handlers
// ---------------------------------------------------------------------------

async fn list_points(
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

async fn create_point(
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

async fn get_point(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_VIEW)?;
        let point = load_point(&state, &path.into_inner())?;
        let criteria = state.criteria_for_point(point.id);
        Ok(json!({ "point": point, "criteria": criteria }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_point(
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
        let mut point = load_point(&state, &path.into_inner())?;
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
            point.target_ref = Some(require_text(target_ref, "target_ref", MAX_REF_LEN)?);
        }
        if let Some(target_kind) = &input.target_kind {
            point.target_kind = Some(require_text(target_kind, "target_kind", MAX_SLUG_LEN)?);
        }
        if let Some(active) = input.active {
            point.active = active;
        }
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

async fn delete_point(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_MANAGE)?;
        let point = load_point(&state, &path.into_inner())?;
        let has_inspections = state
            .inspections
            .iter()
            .any(|entry| entry.value().point_id == point.id);
        if has_inspections {
            return Err(UnifiedError::conflict(
                "Quality point has inspections and cannot be deleted; deactivate it instead",
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
        Ok(json!({ "deleted": point.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn set_point_active(
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
        let mut point = load_point(&state, &path.into_inner())?;
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

async fn activate_point(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    set_point_active(ctx, state, req, path, true, "quality.point.activated").await
}

async fn deactivate_point(
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

async fn list_criteria(
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
        let point = load_point(&state, &path.into_inner())?;
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

async fn create_criterion(
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
        let point = load_point(&state, &path.into_inner())?;
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

async fn update_criterion(
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
        let mut criterion = load_criterion(&state, &path.into_inner())?;
        if let Some(name) = &input.name {
            criterion.name = require_text(name, "name", MAX_TITLE_LEN)?;
        }
        if let Some(kind) = &input.kind {
            criterion.kind = require_in_list(kind, "kind", &settings.criterion_kinds)?;
        }
        if let Some(unit) = &input.unit {
            criterion.unit = Some(require_text(unit, "unit", MAX_SLUG_LEN)?);
        }
        if input.min_value.is_some() {
            criterion.min_value = input.min_value;
        }
        if input.max_value.is_some() {
            criterion.max_value = input.max_value;
        }
        if input.expected_bool.is_some() {
            criterion.expected_bool = input.expected_bool;
        }
        if let Some(expected) = &input.expected_text {
            criterion.expected_text = Some(require_text(expected, "expected_text", MAX_TEXT_LEN)?);
        }
        if let Some(cause) = &input.failure_cause {
            criterion.failure_cause = Some(require_in_list(
                cause,
                "failure_cause",
                &settings.failure_causes,
            )?);
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
            &settings,
        )?;
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

async fn delete_criterion(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_MANAGE)?;
        let criterion = load_criterion(&state, &path.into_inner())?;
        let used = state.inspections.iter().any(|entry| {
            entry
                .value()
                .measurements
                .iter()
                .any(|row| row.criterion_id == criterion.id)
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
        Ok(json!({ "deleted": criterion.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Inspection handlers
// ---------------------------------------------------------------------------

async fn list_inspections(
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
/// exactly the same record.
fn request_inspection(
    state: &QualityState,
    point: &QualityPoint,
    target_ref: String,
    target_kind: String,
    inspector_id: Option<String>,
    company_id: Option<String>,
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
        created_at: now,
        updated_at: now,
    };
    state
        .inspections
        .insert(inspection.id, inspection.clone());
    state.emit(
        "quality.inspection.requested",
        json!({
            "inspection_id": inspection.id,
            "point_id": point.id,
            "target_ref": inspection.target_ref,
            "target_kind": inspection.target_kind,
        }),
    );
    inspection
}

async fn create_inspection(
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
        let point = load_point(&state, &input.point_id)?;
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
        );
        Ok(json!(inspection))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_VIEW)?;
        let inspection = load_inspection(&state, &path.into_inner())?;
        let point = state
            .points
            .get(&inspection.point_id)
            .map(|entry| entry.value().clone());
        let alert = alert_for_inspection(&state, inspection.id);
        Ok(json!({ "inspection": inspection, "point": point, "alert": alert }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn submit_inspection(
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
        let mut inspection = load_inspection(&state, &path.into_inner())?;
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
        let point = load_point(&state, &inspection.point_id.to_string())?;
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
            let evaluated = evaluate_measurement(criterion, by_criterion.get(&criterion.id))?;
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
        // this point are rejected — they cannot be evaluated.
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
        let mut failure_causes =
            require_all_in_list(&input.failure_causes, "failure_cause", &settings.failure_causes)?;
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
            if result == wanted && !settings.inspection_results.iter().any(|item| item == wanted)
            {
                return Err(UnifiedError::unprocessable(format!(
                    "Result \"{wanted}\" is not in the configured inspection_results list"
                )));
            }
        }
        inspection.measurements = measurements;
        inspection.failure_causes = failure_causes;
        inspection.notes = notes;
        inspection.evidence_refs = evidence_refs;
        inspection.status = INSP_COMPLETED.to_string();
        inspection.result = Some(result.clone());
        inspection.inspector_id = Some(caller.actor.clone());
        inspection.inspected_at = Some(Utc::now());
        inspection.updated_at = Utc::now();
        state
            .inspections
            .insert(inspection.id, inspection.clone());
        state.emit(
            "quality.inspection.completed",
            json!({
                "inspection_id": inspection.id,
                "point_id": point.id,
                "target_ref": inspection.target_ref,
                "target_kind": inspection.target_kind,
                "result": result,
                "inspector_id": caller.actor,
                "blocked": false,
            }),
        );
        // A failed inspection raises an alert under the point's
        // fail policy, and blocks the related record when the
        // policy is `block`. The alert (not a direct call) is how
        // the owning module learns the record must not proceed.
        let mut alert: Option<QualityAlert> = None;
        if result == RESULT_FAIL && point.on_fail_policy != "none" {
            let blocked = point.on_fail_policy == "block";
            let now = Utc::now();
            let raised = QualityAlert {
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
                created_at: now,
                updated_at: now,
            };
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
            // The completion event above was emitted with
            // blocked=false before the alert existed; emit the
            // corrected completion view so listeners see the block.
            if blocked {
                state.emit(
                    "quality.inspection.completed",
                    json!({
                        "inspection_id": inspection.id,
                        "point_id": point.id,
                        "target_ref": inspection.target_ref,
                        "target_kind": inspection.target_kind,
                        "result": result,
                        "inspector_id": caller.actor,
                        "blocked": true,
                        "alert_id": raised.id,
                    }),
                );
            }
            alert = Some(raised);
        }
        Ok(json!({ "inspection": inspection, "alert": alert }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn cancel_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_inspect(&ctx, &caller.role)?;
        let mut inspection = load_inspection(&state, &path.into_inner())?;
        if !valid_inspection_transition(&inspection.status, INSP_CANCELLED) {
            return Err(UnifiedError::unprocessable(format!(
                "Inspection in status \"{}\" cannot be cancelled",
                inspection.status
            )));
        }
        inspection.status = INSP_CANCELLED.to_string();
        inspection.updated_at = Utc::now();
        state
            .inspections
            .insert(inspection.id, inspection.clone());
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

// ---------------------------------------------------------------------------
// Alert + block handlers
// ---------------------------------------------------------------------------

async fn list_alerts(
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

async fn get_alert(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_VIEW)?;
        let alert = load_alert(&state, &path.into_inner())?;
        let inspection = state
            .inspections
            .get(&alert.inspection_id)
            .map(|entry| entry.value().clone());
        let capas: Vec<Capa> = state
            .capas
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.alert_id == Some(alert.id))
            .collect();
        Ok(json!({ "alert": alert, "inspection": inspection, "capas": capas }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn transition_alert(
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
        let mut alert = load_alert(&state, &path.into_inner())?;
        if !valid_alert_transition(&alert.status, to) {
            return Err(UnifiedError::unprocessable(format!(
                "Alert in status \"{}\" cannot move to \"{to}\"",
                alert.status
            )));
        }
        match to {
            ALERT_ACKNOWLEDGED => {
                alert.acknowledged_by = Some(caller.actor.clone());
            }
            ALERT_RESOLVED => {
                alert.resolved_by = Some(caller.actor.clone());
                alert.resolved_at = Some(Utc::now());
            }
            _ => {}
        }
        alert.status = to.to_string();
        alert.updated_at = Utc::now();
        state.alerts.insert(alert.id, alert.clone());
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

async fn acknowledge_alert(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    transition_alert(ctx, state, req, path, ALERT_ACKNOWLEDGED, "quality.alert.acknowledged").await
}

async fn resolve_alert(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    transition_alert(ctx, state, req, path, ALERT_RESOLVED, "quality.alert.resolved").await
}

/// The currently blocked targets: open alerts whose point policy
/// was `block`. Owning modules consult this before letting a
/// record proceed; resolving the alert unblocks the target.
async fn list_blocks(
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

// ---------------------------------------------------------------------------
// CAPA handlers
// ---------------------------------------------------------------------------

async fn list_capas(
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

async fn create_capa(
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
        let inspection = load_inspection(&state, &input.inspection_id)?;
        if inspection.result.as_deref() != Some(RESULT_FAIL) {
            return Err(UnifiedError::unprocessable(
                "A CAPA can only be opened for a failed inspection",
            ));
        }
        let alert_id = match &input.alert_id {
            Some(raw) => {
                let alert = load_alert(&state, raw)?;
                if alert.inspection_id != inspection.id {
                    return Err(UnifiedError::bad_request(
                        "alert_id does not belong to the given inspection",
                    ));
                }
                Some(alert.id)
            }
            None => alert_for_inspection(&state, inspection.id).map(|row| row.id),
        };
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)
            .unwrap_or_default();
        let root_cause = optional_text(&input.root_cause, "root_cause", MAX_TEXT_LEN)?;
        let owner_id = require_text(&input.owner_id, "owner_id", MAX_REF_LEN)?;
        if let Some(due) = input.due_date {
            if due < Utc::now() {
                return Err(UnifiedError::bad_request("due_date must be in the future"));
            }
        }
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

async fn get_capa(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<QualityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_QUALITY_VIEW)?;
        let capa = load_capa(&state, &path.into_inner())?;
        let inspection = state
            .inspections
            .get(&capa.inspection_id)
            .map(|entry| entry.value().clone());
        Ok(json!({ "capa": capa, "inspection": inspection }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_capa(
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
        let mut capa = load_capa(&state, &path.into_inner())?;
        if capa.state == CAPA_CLOSED {
            return Err(UnifiedError::unprocessable("A closed CAPA cannot be edited"));
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

async fn transition_capa(
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
        let mut capa = load_capa(&state, &path.into_inner())?;
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
        state.capas.insert(capa.id, capa.clone());
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

// ---------------------------------------------------------------------------
// Report handlers
// ---------------------------------------------------------------------------

/// Completed inspections in the optional window, optionally
/// filtered by target / point / scope (scope resolves through the
/// point). Requested and cancelled inspections never count.
fn report_inspections(
    state: &QualityState,
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
            if let Some(from) = from {
                if row.created_at < from {
                    return false;
                }
            }
            if let Some(to) = to {
                if row.created_at > to {
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

async fn report_pass_rate(
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
        let mut rows = report_inspections(&state, from, to, &target_ref, &point_id);
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

async fn report_failure_causes(
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
        let rows = report_inspections(&state, from, to, &target_ref, &point_id);
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
            count_b
                .cmp(&count_a)
                .then_with(|| {
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

async fn report_backlog(
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
        let open_alerts = state
            .alerts
            .iter()
            .filter(|entry| entry.value().status == ALERT_OPEN)
            .count() as i64;
        let acknowledged_alerts = state
            .alerts
            .iter()
            .filter(|entry| entry.value().status == ALERT_ACKNOWLEDGED)
            .count() as i64;
        let blocking_alerts = state
            .alerts
            .iter()
            .filter(|entry| {
                let row = entry.value();
                row.blocked && row.status != ALERT_RESOLVED
            })
            .count() as i64;
        let open_capas = state
            .capas
            .iter()
            .filter(|entry| entry.value().state == CAPA_OPEN)
            .count() as i64;
        let in_progress_capas = state
            .capas
            .iter()
            .filter(|entry| entry.value().state == CAPA_IN_PROGRESS)
            .count() as i64;
        let overdue_capas = state
            .capas
            .iter()
            .filter(|entry| {
                let row = entry.value();
                match row.due_date {
                    Some(due) => due < now && row.state != CAPA_CLOSED,
                    None => false,
                }
            })
            .count() as i64;
        let requested_inspections = state
            .inspections
            .iter()
            .filter(|entry| entry.value().status == INSP_REQUESTED)
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

async fn report_supplier_score(
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
            Some(raw) => raw.parse::<i64>().map_err(|_| {
                UnifiedError::bad_request("min_inspections must be a whole number")
            })?,
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

async fn get_settings(
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

async fn update_settings(
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
            if !settings.alert_severities.iter().any(|item| item == severity) {
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
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        state.emit(
            "quality.settings.updated",
            json!({ "actor": caller.actor }),
        );
        Ok(json!(settings))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// ERP event listeners (events only — no direct calls into other
// modules, and no other module is called from here)
// ---------------------------------------------------------------------------

/// Points that match an incoming ERP event: active points whose
/// scope is the event's scope and whose `target_ref` is either
/// unset (the point applies to every target in the scope) or
/// equal to the event's target.
fn matching_points(state: &QualityState, scope: &str, target_ref: &str) -> Vec<QualityPoint> {
    let mut rows: Vec<QualityPoint> = state
        .points
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|point| {
            point.active
                && point.scope == scope
                && match &point.target_ref {
                    Some(target) => target == target_ref,
                    None => true,
                }
        })
        .collect();
    rows.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    rows
}

/// Answer one ERP inspection request. When the payload names a
/// point (`point_id`), that point is used; otherwise every active
/// point matching the scope and target gets a requested inspection.
/// A missing field, an unknown point, or no matching point is
/// skipped silently — an event must never fail the emitter.
fn answer_inspection_request(state: QualityState, ctx: &mut HookContext, scope: &str) {
    let target_ref = ctx
        .get("target_ref")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| opt_string(&ctx.data, "work_order_ref"))
        .or_else(|| opt_string(&ctx.data, "product_ref"))
        .or_else(|| opt_string(&ctx.data, "supplier_id"))
        .or_else(|| opt_string(&ctx.data, "shipment_ref"))
        .or_else(|| opt_string(&ctx.data, "receipt_ref"));
    let Some(target_ref) = target_ref else {
        return;
    };
    if target_ref.trim().is_empty() {
        return;
    }
    let target_kind = ctx
        .get("target_kind")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| scope.to_string());
    let inspector_id = ctx
        .get("inspector_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let points = match ctx.get("point_id").and_then(|value| value.as_str()) {
        Some(raw) => match Uuid::parse_str(raw) {
            Ok(id) => state
                .points
                .get(&id)
                .map(|entry| entry.value().clone())
                .filter(|point| point.active && point.scope == scope)
                .into_iter()
                .collect::<Vec<QualityPoint>>(),
            Err(_) => return,
        },
        None => matching_points(&state, scope, &target_ref),
    };
    for point in points {
        request_inspection(
            &state,
            &point,
            target_ref.clone(),
            target_kind.clone(),
            inspector_id.clone(),
            company_id.clone(),
        );
    }
}

/// Answer `quality.blocks.query` — the hook form of `GET /blocks`.
/// Given `{"target_ref", "target_kind?"}` it answers with the
/// blocking alerts covering that target, using the same rule as
/// `list_blocks`: an alert blocks while `blocked` is true and its
/// status is not resolved. The answer is set on the `blocks` key
/// and added as a result row (`{"target_ref", "target_kind",
/// "blocks", "blocked"}`), so owning modules can refuse to proceed
/// with a blocked record. A query without a `target_ref` gets no
/// answer — there is nothing to match against.
fn on_blocks_query(state: QualityState, ctx: &mut HookContext) {
    let Some(target_ref) = ctx
        .get("target_ref")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    else {
        return;
    };
    let target_kind = ctx
        .get("target_kind")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut blocks: Vec<Value> = state
        .alerts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|row| {
            if !row.blocked || row.status == ALERT_RESOLVED {
                return false;
            }
            if row.target_ref != target_ref {
                return false;
            }
            if let Some(kind) = &target_kind {
                if &row.target_kind != kind {
                    return false;
                }
            }
            true
        })
        .map(|row| json!(row))
        .collect();
    blocks.sort_by(|a, b| {
        let at = a.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        let bt = b.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        bt.cmp(at)
    });
    let blocked = !blocks.is_empty();
    let answer = json!({
        "target_ref": target_ref,
        "target_kind": target_kind,
        "blocks": blocks,
        "blocked": blocked,
    });
    ctx.set("blocks", answer["blocks"].clone());
    ctx.set("blocked", json!(blocked));
    ctx.add_result(answer);
}

fn on_manufacturing_inspection_requested(state: QualityState, ctx: &mut HookContext) {
    answer_inspection_request(state, ctx, "work_order");
}

fn on_inventory_goods_received(state: QualityState, ctx: &mut HookContext) {
    answer_inspection_request(state, ctx, "incoming_goods");
}

fn on_procurement_quality_check_requested(state: QualityState, ctx: &mut HookContext) {
    answer_inspection_request(state, ctx, "supplier");
}

fn on_logistics_shipment_inspection_requested(state: QualityState, ctx: &mut HookContext) {
    answer_inspection_request(state, ctx, "shipment");
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct QualityModule {
    state: QualityState,
}

impl QualityModule {
    pub fn new() -> Self {
        Self {
            state: QualityState::default(),
        }
    }
}

impl Default for QualityModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for QualityModule {
    fn id(&self) -> &'static str {
        "quality"
    }

    fn name(&self) -> &'static str {
        "Quality Management"
    }

    fn description(&self) -> &'static str {
        "Quality management: data-driven quality points with tolerance criteria, inspections with measured results, failure alerts that can block the related record until resolved, CAPA records with verification of effectiveness, and transparent quality reports (pass rate, top failure causes, backlog, supplier quality score). All scopes, kinds, policies, causes, and scoring inputs are admin-configurable settings, never hard-coded."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["roles", "storage", "documents", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["roles", "storage", "documents", "notifications"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            QUALITY_BLOCKS_QUERY,
            "quality",
            Arc::new(move |ctx: &mut HookContext| {
                on_blocks_query(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            MANUFACTURING_INSPECTION_REQUESTED,
            "quality",
            Arc::new(move |ctx: &mut HookContext| {
                on_manufacturing_inspection_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            INVENTORY_GOODS_RECEIVED,
            "quality",
            Arc::new(move |ctx: &mut HookContext| {
                on_inventory_goods_received(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            PROCUREMENT_QUALITY_CHECK_REQUESTED,
            "quality",
            Arc::new(move |ctx: &mut HookContext| {
                on_procurement_quality_check_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            LOGISTICS_SHIPMENT_INSPECTION_REQUESTED,
            "quality",
            Arc::new(move |ctx: &mut HookContext| {
                on_logistics_shipment_inspection_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/quality")
                .app_data(state)
                .route("/points", web::get().to(list_points))
                .route("/points", web::post().to(create_point))
                .route("/points/{id}", web::get().to(get_point))
                .route("/points/{id}", web::patch().to(update_point))
                .route("/points/{id}", web::delete().to(delete_point))
                .route("/points/{id}/activate", web::post().to(activate_point))
                .route("/points/{id}/deactivate", web::post().to(deactivate_point))
                .route("/points/{id}/criteria", web::get().to(list_criteria))
                .route("/points/{id}/criteria", web::post().to(create_criterion))
                .route("/criteria/{id}", web::patch().to(update_criterion))
                .route("/criteria/{id}", web::delete().to(delete_criterion))
                .route("/inspections", web::get().to(list_inspections))
                .route("/inspections", web::post().to(create_inspection))
                .route("/inspections/{id}", web::get().to(get_inspection))
                .route("/inspections/{id}/submit", web::post().to(submit_inspection))
                .route("/inspections/{id}/cancel", web::post().to(cancel_inspection))
                .route("/alerts", web::get().to(list_alerts))
                .route("/alerts/{id}", web::get().to(get_alert))
                .route("/alerts/{id}/acknowledge", web::post().to(acknowledge_alert))
                .route("/alerts/{id}/resolve", web::post().to(resolve_alert))
                .route("/blocks", web::get().to(list_blocks))
                .route("/capas", web::get().to(list_capas))
                .route("/capas", web::post().to(create_capa))
                .route("/capas/{id}", web::get().to(get_capa))
                .route("/capas/{id}", web::patch().to(update_capa))
                .route("/capas/{id}/transition", web::post().to(transition_capa))
                .route("/reports/pass-rate", web::get().to(report_pass_rate))
                .route("/reports/failure-causes", web::get().to(report_failure_causes))
                .route("/reports/backlog", web::get().to(report_backlog))
                .route("/reports/supplier-score", web::get().to(report_supplier_score))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/quality/points", owner: "quality", description: "List quality points (paginated, filter by scope, check_kind, on_fail_policy, active, target_ref)." },
            RouteInfo { method: "POST", path: "/api/v1/quality/points", owner: "quality", description: "Create a quality point (manager; scope, check kind, fail policy, and severity validated against settings)." },
            RouteInfo { method: "GET", path: "/api/v1/quality/points/{id}", owner: "quality", description: "Get one quality point with its criteria." },
            RouteInfo { method: "PATCH", path: "/api/v1/quality/points/{id}", owner: "quality", description: "Update a quality point (manager)." },
            RouteInfo { method: "DELETE", path: "/api/v1/quality/points/{id}", owner: "quality", description: "Delete a quality point with no inspections (manager); otherwise deactivate it." },
            RouteInfo { method: "POST", path: "/api/v1/quality/points/{id}/activate", owner: "quality", description: "Activate a quality point (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/quality/points/{id}/deactivate", owner: "quality", description: "Deactivate a quality point (manager); no new inspections may be requested against it." },
            RouteInfo { method: "GET", path: "/api/v1/quality/points/{id}/criteria", owner: "quality", description: "List the criteria of a quality point (paginated, filter by kind)." },
            RouteInfo { method: "POST", path: "/api/v1/quality/points/{id}/criteria", owner: "quality", description: "Add a tolerance criterion to a quality point (manager; numeric bands, boolean or text expectations, per-criterion failure cause)." },
            RouteInfo { method: "PATCH", path: "/api/v1/quality/criteria/{id}", owner: "quality", description: "Update a criterion (manager; tolerance shape re-validated)." },
            RouteInfo { method: "DELETE", path: "/api/v1/quality/criteria/{id}", owner: "quality", description: "Delete a criterion not referenced by completed inspections (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/quality/inspections", owner: "quality", description: "List inspections (paginated, filter by point_id, target_ref, target_kind, status, result, inspector_id)." },
            RouteInfo { method: "POST", path: "/api/v1/quality/inspections", owner: "quality", description: "Request an inspection against an active quality point (manager or inspector)." },
            RouteInfo { method: "GET", path: "/api/v1/quality/inspections/{id}", owner: "quality", description: "Get one inspection with its point and alert, if any." },
            RouteInfo { method: "POST", path: "/api/v1/quality/inspections/{id}/submit", owner: "quality", description: "Submit measurements; every criterion is evaluated against its stored tolerance and the inspection completes as pass / fail / conditional. A failure raises an alert and blocks the record when the point policy is block." },
            RouteInfo { method: "POST", path: "/api/v1/quality/inspections/{id}/cancel", owner: "quality", description: "Cancel a requested inspection (manager or inspector)." },
            RouteInfo { method: "GET", path: "/api/v1/quality/alerts", owner: "quality", description: "List quality alerts (paginated, filter by status, severity, point_id, target_ref, blocked)." },
            RouteInfo { method: "GET", path: "/api/v1/quality/alerts/{id}", owner: "quality", description: "Get one alert with its inspection and CAPAs." },
            RouteInfo { method: "POST", path: "/api/v1/quality/alerts/{id}/acknowledge", owner: "quality", description: "Acknowledge an open alert (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/quality/alerts/{id}/resolve", owner: "quality", description: "Resolve an alert (manager); resolving a blocking alert unblocks its target." },
            RouteInfo { method: "GET", path: "/api/v1/quality/blocks", owner: "quality", description: "List currently blocked targets (open blocking alerts) so owning modules can refuse to proceed with them." },
            RouteInfo { method: "GET", path: "/api/v1/quality/capas", owner: "quality", description: "List CAPAs (paginated, filter by state, owner_id, inspection_id, overdue)." },
            RouteInfo { method: "POST", path: "/api/v1/quality/capas", owner: "quality", description: "Open a CAPA for a failed inspection (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/quality/capas/{id}", owner: "quality", description: "Get one CAPA with its inspection." },
            RouteInfo { method: "PATCH", path: "/api/v1/quality/capas/{id}", owner: "quality", description: "Update a CAPA that is not closed (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/quality/capas/{id}/transition", owner: "quality", description: "Move a CAPA through open -> in_progress -> verified -> closed (manager; verification needs notes when verification is required)." },
            RouteInfo { method: "GET", path: "/api/v1/quality/reports/pass-rate", owner: "quality", description: "Pass rate per product / point / period, with the calculation method in the response." },
            RouteInfo { method: "GET", path: "/api/v1/quality/reports/failure-causes", owner: "quality", description: "Top failure causes over failed inspections in a period, with counts and shares." },
            RouteInfo { method: "GET", path: "/api/v1/quality/reports/backlog", owner: "quality", description: "Inspection backlog: open alerts, blocking alerts, open and overdue CAPAs, requested inspections." },
            RouteInfo { method: "GET", path: "/api/v1/quality/reports/supplier-score", owner: "quality", description: "Supplier quality score from inspection history, with every input and the formula in the response." },
            RouteInfo { method: "GET", path: "/api/v1/quality/settings", owner: "quality", description: "Get the configurable quality settings." },
            RouteInfo { method: "PUT", path: "/api/v1/quality/settings", owner: "quality", description: "Update the configurable quality settings (manager)." },
        ]
    }
}

/// Register this module with a module registry. The coordinator
/// calls this from the binary's module list; `mod.rs` is owned by
/// the coordinator, so this module never edits it itself.
pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(QualityModule::new()));
}
