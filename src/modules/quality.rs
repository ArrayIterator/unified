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
//     "target_kind", "result", "inspector_id", "blocked",
//     "alert_id"}` — emitted exactly once per submission, after
//     the alert / block outcome is decided, so the `blocked`
//     value listeners act on is final),
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
// inspections). Every record carries the tenant id resolved from
// the request, and every read is filtered to that tenant, so one
// tenant never sees another tenant's quality data. A submission
// is accepted only from the assigned inspector (or a supervisor),
// and a CAPA may only be verified by someone other than its
// owner. All input is validated (lengths, UUIDs, settings
// lists, numeric ranges); output travels through the standard
// JSON envelope, so no raw markup is ever rendered from user
// input. No raw SQL anywhere — the module holds no database
// handle at all; the migration is the persistence target only.

pub mod ai_capabilities;
mod constants;
mod evaluation;
mod handlers_alerts;
mod handlers_capas;
mod handlers_inspections;
mod handlers_points;
mod helpers;
mod hooks;
pub mod module;
mod reports;
pub mod state;
mod store;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::{QualityModule, register};
pub use self::state::QualityState;
pub use self::types::{Capa, CreatePointInput, QualityAlert, UpdatePointInput};
pub use self::types::{CreateCapaInput, MeasurementInput, SubmitInspectionInput, UpdateCapaInput};
pub use self::types::{CreateCriterionInput, CreateInspectionInput, UpdateCriterionInput};
pub use self::types::{Inspection, Measurement, QualityCriterion, QualityPoint, QualitySettings};
pub use self::types::{TransitionCapaInput, UpdateSettingsInput};
