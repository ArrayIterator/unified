// Incident & On-Call module — incidents with a validated status
// machine and an append-only timeline, on-call rotations and
// schedules with overrides, escalation policies with paging, and
// postmortems whose action items become tasks.
//
// Routes live under `/api/v1/incidents`:
//
//   Incidents:
//     GET/POST / — list incidents / open an incident.
//     GET/PATCH /{id}.
//     POST /{id}/acknowledge — acknowledge (starts the clock).
//     POST /{id}/status — validated status transition.
//     POST /{id}/page — page the on-call responder.
//     POST /{id}/escalate — escalate to the next policy step.
//     GET  /{id}/timeline — append-only timeline entries.
//     POST /{id}/timeline — add a note entry.
//     POST /{id}/responders — add a responder.
//     DELETE /{id}/responders/{user_id} — remove a responder.
//   Postmortems:
//     GET  /{id}/postmortem.
//     POST /{id}/postmortem — create (resolved / closed only).
//     POST /{id}/postmortem/action-items — add an action item.
//     PATCH /{id}/postmortem/action-items/{item_id} — update one.
//   On-call rotations:
//     GET/POST /rotations — list rotations / create a rotation.
//     GET/PATCH/DELETE /rotations/{id}.
//     POST /rotations/{id}/members — add a rotation member.
//     DELETE /rotations/{id}/members/{user_id} — remove a member.
//     GET  /rotations/{id}/on-call — who is on call now.
//     GET/POST /rotations/{id}/schedules — list / create a schedule
//       entry (kind `regular` or `override`).
//     POST /rotations/{id}/overrides — create an override entry.
//   Escalation policies:
//     GET/POST /escalation-policies.
//     GET/PATCH/DELETE /escalation-policies/{id}.
//     POST /escalations/run — process due escalations (called by
//       the scheduler or an operator; pages the next responder).
//   Service Monitor linkage:
//     GET/POST /auto-open — list / set per-service auto-open rules.
//     POST /auto-open/{service_id} — set one service's rule.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory incident, timeline, rotation, schedule, escalation
// policy, page-attempt, and postmortem management; the validated
// status machine (`open` -> `acknowledged` -> `investigating` ->
// `mitigating` -> `resolved` -> `closed`, with `resolved` ->
// `investigating` reopen and `investigating` -> `acknowledged`
// step-back); an append-only timeline (entries are never updated
// or deleted); on-call rotation order with handover times and
// schedule entries where `override` entries take precedence over
// `regular` ones; escalation policies as ordered steps with a
// per-incident escalation state; paging that emits
// `notification.requested` with high priority and records the
// page attempt with acknowledgement tracking; postmortems linked
// to resolved or closed incidents. What is NOT built here: actual
// SMS / phone-call delivery (the notifications module owns
// delivery — this module only emits `notification.requested`);
// automatic escalation on a timer inside this module (escalation
// only moves when `POST /escalations/run` is called — the
// scheduler must call it, or an operator must; the run endpoint
// is the honest boundary, exactly like the events module's
// reminder scheduler note); and task creation itself — a
// postmortem action item emits `tasks.create.requested` with a
// full task payload for the tasks module, but no listener in the
// tasks module consumes that event yet, so today the action item
// is stored here with status `open` and the request event is the
// integration point (see the integrator note in the module
// report).
//
// Public-access gate: this module has no public routes. Every
// route below requires the caller headers and the `incidents.view`
// or `incidents.manage` capability.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `incidents.created`, `incidents.updated`,
//     `incidents.acknowledged`, `incidents.status_changed`,
//     `incidents.resolved`, `incidents.closed`
//   - `incidents.paged`, `incidents.page.acknowledged`,
//     `incidents.escalated`
//   - `incidents.postmortem.created`,
//     `incidents.postmortem.action_item.created`
//   - `incidents.timeline.added`
//   - `tasks.create.requested` (postmortem action items — see the
//     honesty note above; no tasks listener consumes it yet)
//   - `notification.requested` (paging, escalation, new incidents)
//     with the notifications module's payload shape: `{"kind",
//     "user_id", "title", "body", "module": "incidents", "link",
//     "priority", "channels"}`.
// Events this module listens to:
//   - `cluster.health.changed` (Service Monitor) — a `down`
//     health for a service with an enabled auto-open rule opens
//     an incident (deduplicated: no second open auto-incident for
//     the same service); a recovery appends a timeline note to
//     open auto-incidents for that service and resolves them when
//     the rule asks for it. Auto-open is off by default for every
//     service.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_TITLE_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_MESSAGE_LEN: usize = 5000;
const MAX_NAME_LEN: usize = 200;
const MAX_SUMMARY_LEN: usize = 10000;
const MAX_ROOT_CAUSE_LEN: usize = 10000;
const MAX_MEMBERS: usize = 100;
const MAX_STEPS: usize = 20;
const MAX_SERVICES: usize = 100;
const MAX_RESPONDERS: usize = 100;

/// Capability names checked through the `role.check` hook.
const PERM_INCIDENTS_VIEW: &str = "incidents.view";
const PERM_INCIDENTS_MANAGE: &str = "incidents.manage";

/// Hook event names this module listens to (the literal repeated
/// here so the module stays decoupled from the Service Monitor
/// module, exactly like the events module repeats the payments
/// constants). The Service Monitor emits `cluster.health.changed`
/// with `service_id`, `service_name`, `health` (`down` on outage),
/// `status`, `detail`, and `checked_at`.
const SERVICE_HEALTH_CHANGED: &str = "cluster.health.changed";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Incident severity: `critical`, `high`, `medium`, `low`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentSeverity {
    Critical,
    High,
    Medium,
    Low,
}

impl Default for IncidentSeverity {
    fn default() -> Self {
        Self::Medium
    }
}

impl IncidentSeverity {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "critical" => Some(Self::Critical),
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            _ => None,
        }
    }
}

/// Incident status. The machine is:
///
///   open -> acknowledged -> investigating -> mitigating ->
///   resolved -> closed
///
/// with two step-backs: `investigating` -> `acknowledged` and
/// `resolved` -> `investigating` (reopen). Every other transition
/// is rejected. `closed` is terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentStatus {
    Open,
    Acknowledged,
    Investigating,
    Mitigating,
    Resolved,
    Closed,
}

impl Default for IncidentStatus {
    fn default() -> Self {
        Self::Open
    }
}

impl IncidentStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Acknowledged => "acknowledged",
            Self::Investigating => "investigating",
            Self::Mitigating => "mitigating",
            Self::Resolved => "resolved",
            Self::Closed => "closed",
        }
    }

    /// Allowed next statuses from this status.
    fn allowed_next(&self) -> &'static [IncidentStatus] {
        match self {
            Self::Open => &[Self::Acknowledged],
            Self::Acknowledged => &[Self::Investigating],
            Self::Investigating => &[Self::Mitigating, Self::Acknowledged],
            Self::Mitigating => &[Self::Resolved, Self::Investigating],
            Self::Resolved => &[Self::Closed, Self::Investigating],
            Self::Closed => &[],
        }
    }
}

/// One incident. `affected_service_ids` links to Service Monitor
/// service ids (plain strings — the Service Monitor owns services).
/// `source` is `manual` or `auto` (opened from a Service Monitor
/// alert); `source_service_id` names the service for auto
/// incidents. `responders` are user ids; `current_responder` is
/// the user id currently paged / owning the incident.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Incident {
    pub id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub severity: IncidentSeverity,
    pub status: IncidentStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_service_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub responders: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_responder: Option<String>,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_service_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_policy_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_id: Option<Uuid>,
    pub created_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One append-only timeline entry. `kind` is `note`,
/// `status_change`, `responder_action`, `page`, `escalation`, or
/// `system`. Entries are never updated or deleted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentTimelineEntry {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub actor_id: String,
    pub kind: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
}

/// One on-call rotation: an ordered list of responder user ids.
/// Handover happens every `handover_interval_days` at
/// `handover_time` (HH:MM, UTC) in `timezone` (format-checked
/// only). `current_index` is the member currently on call when no
/// schedule entry covers now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnCallRotation {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<String>,
    pub handover_interval_days: i64,
    /// Handover time of day, "HH:MM" (UTC).
    pub handover_time: String,
    pub timezone: String,
    pub current_index: usize,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One on-call schedule entry: `member_id` is on call from
/// `starts_at` to `ends_at` for `rotation_id`. `kind` is `regular`
/// or `override`; an `override` entry wins over a `regular` entry
/// covering the same time. `overrides_entry_id` names the regular
/// entry this override replaces, when known.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnCallSchedule {
    pub id: Uuid,
    pub rotation_id: Uuid,
    pub member_id: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides_entry_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One step of an escalation policy: after `after_minutes`
/// unacknowledged, page `target` — `responder` (the next rotation
/// member), `manager` (the `manager_id`), or `user` (the
/// `user_id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationStep {
    pub after_minutes: i64,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manager_id: Option<String>,
}

/// One escalation policy: an ordered list of steps applied to
/// incidents of the listed severities (`applies_to_severities`
/// empty means every severity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationPolicy {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applies_to_severities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<EscalationStep>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Per-incident escalation state: which policy step the incident
/// is on and when the next escalation is due. `status` is
/// `active`, `completed` (all steps exhausted), or `stopped` (the
/// incident was acknowledged or resolved).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentEscalation {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub policy_id: Uuid,
    pub step_index: usize,
    pub next_escalation_at: DateTime<Utc>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One page attempt: paging a responder for an incident. `status`
/// is `sent`, `acknowledged`, or `expired`. Acknowledgement is
/// recorded on the attempt and on the incident.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageAttempt {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub paged_user_id: String,
    pub channel: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One postmortem action item. `status` is `open`, `in_progress`,
/// or `done`. `task_reference` is the id the tasks module returns
/// once it consumes the `tasks.create.requested` event (empty
/// until then — see the module header).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostmortemActionItem {
    pub id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_reference: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One postmortem, linked to a resolved or closed incident.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentPostmortem {
    pub id: Uuid,
    pub incident_id: Uuid,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_cause: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub action_items: Vec<PostmortemActionItem>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Per-service auto-open rule for Service Monitor alerts.
/// `enabled` defaults to `false` (off) — auto-open is opt-in per
/// service. `auto_resolve_on_recovery` resolves open auto
/// incidents for the service when it recovers (also off by
/// default).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoOpenRule {
    pub service_id: String,
    pub enabled: bool,
    pub severity: IncidentSeverity,
    #[serde(default)]
    pub auto_resolve_on_recovery: bool,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateIncidentRequest {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub severity: Option<IncidentSeverity>,
    #[serde(default)]
    pub affected_service_ids: Vec<String>,
    #[serde(default)]
    pub responders: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_policy_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateIncidentRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub severity: Option<IncidentSeverity>,
    pub affected_service_ids: Option<Vec<String>>,
    pub escalation_policy_id: Option<Option<Uuid>>,
    pub rotation_id: Option<Option<Uuid>>,
}

#[derive(Debug, Deserialize)]
pub struct ChangeStatusRequest {
    pub status: IncidentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddTimelineRequest {
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct AddResponderRequest {
    pub user_id: String,
}

#[derive(Debug, Deserialize)]
pub struct PageRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default = "default_page_channel")]
    pub channel: String,
}

fn default_page_channel() -> String {
    "notification".to_string()
}

#[derive(Debug, Deserialize)]
pub struct CreateRotationRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default = "default_handover_interval")]
    pub handover_interval_days: i64,
    #[serde(default = "default_handover_time")]
    pub handover_time: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
}

fn default_handover_interval() -> i64 {
    7
}

fn default_handover_time() -> String {
    "09:00".to_string()
}

fn default_timezone() -> String {
    "UTC".to_string()
}

#[derive(Debug, Deserialize)]
pub struct UpdateRotationRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub handover_interval_days: Option<i64>,
    pub handover_time: Option<String>,
    pub timezone: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddMemberRequest {
    pub user_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateScheduleRequest {
    pub member_id: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    #[serde(default = "default_schedule_kind")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides_entry_id: Option<Uuid>,
}

fn default_schedule_kind() -> String {
    "regular".to_string()
}

#[derive(Debug, Deserialize)]
pub struct CreateEscalationPolicyRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub applies_to_severities: Vec<String>,
    pub steps: Vec<EscalationStep>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEscalationPolicyRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub applies_to_severities: Option<Vec<String>>,
    pub steps: Option<Vec<EscalationStep>>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePostmortemRequest {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_cause: Option<String>,
    #[serde(default)]
    pub action_items: Vec<CreateActionItemRequest>,
}

#[derive(Debug, Deserialize)]
pub struct CreateActionItemRequest {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateActionItemRequest {
    pub title: Option<String>,
    pub assignee_id: Option<Option<String>>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SetAutoOpenRequest {
    pub enabled: bool,
    #[serde(default)]
    pub severity: Option<IncidentSeverity>,
    #[serde(default)]
    pub auto_resolve_on_recovery: bool,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for incidents. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration `2026-11-22-000053_incidents`;
/// until that migration is executed, incidents do not survive a
/// restart.
#[derive(Clone)]
pub struct IncidentsState {
    pub incidents: Arc<DashMap<Uuid, Incident>>,
    pub timeline: Arc<DashMap<Uuid, IncidentTimelineEntry>>,
    pub rotations: Arc<DashMap<Uuid, OnCallRotation>>,
    pub schedules: Arc<DashMap<Uuid, OnCallSchedule>>,
    pub escalation_policies: Arc<DashMap<Uuid, EscalationPolicy>>,
    pub escalations: Arc<DashMap<Uuid, IncidentEscalation>>,
    pub page_attempts: Arc<DashMap<Uuid, PageAttempt>>,
    pub postmortems: Arc<DashMap<Uuid, IncidentPostmortem>>,
    pub auto_open_rules: Arc<DashMap<String, AutoOpenRule>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for IncidentsState {
    fn default() -> Self {
        Self {
            incidents: Arc::new(DashMap::new()),
            timeline: Arc::new(DashMap::new()),
            rotations: Arc::new(DashMap::new()),
            schedules: Arc::new(DashMap::new()),
            escalation_policies: Arc::new(DashMap::new()),
            escalations: Arc::new(DashMap::new()),
            page_attempts: Arc::new(DashMap::new()),
            postmortems: Arc::new(DashMap::new()),
            auto_open_rules: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl IncidentsState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape. Paging and escalation use high
    /// priority; new-incident notices use normal priority.
    fn notify(
        &self,
        kind: &str,
        user_id: &str,
        title: &str,
        body: &str,
        link: Option<String>,
        priority: &str,
    ) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "incidents",
                "category": "incidents",
                "priority": priority,
                "link": link,
            }),
        );
    }

    /// Append one row to an incident's timeline. The timeline is
    /// append-only: this is the only writer, and no handler ever
    /// updates or removes a row.
    fn add_timeline(&self, incident_id: Uuid, actor_id: &str, kind: &str, message: String) {
        self.timeline.insert(
            Uuid::now_v7(),
            IncidentTimelineEntry {
                id: Uuid::now_v7(),
                incident_id,
                actor_id: actor_id.to_string(),
                kind: kind.to_string(),
                message,
                created_at: Utc::now(),
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the ai module.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
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
/// (same fail-closed shape as the ai module's capability check).
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

/// A handover time is "HH:MM" in 24-hour form.
fn valid_handover_time(value: &str) -> bool {
    let parts: Vec<&str> = value.split(':').collect();
    if parts.len() != 2 || parts[0].len() != 2 || parts[1].len() != 2 {
        return false;
    }
    let Ok(hour) = parts[0].parse::<u32>() else {
        return false;
    };
    let Ok(minute) = parts[1].parse::<u32>() else {
        return false;
    };
    hour < 24 && minute < 60
}

/// A timezone is an IANA name (`"Asia/Jakarta"`, `"UTC"`): letters,
/// digits, `_`, `-`, `+`, and `/` only, no empty segments. Shape
/// only; no conversion is done (all stored times are UTC).
fn valid_timezone(timezone: &str) -> bool {
    if timezone.is_empty() || timezone.len() > 64 {
        return false;
    }
    if timezone == "UTC" {
        return true;
    }
    timezone.contains('/')
        && timezone.split('/').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+')
        })
}

fn validate_user_ids(ids: &[String], field: &str, max: usize) -> UnifiedResult<Vec<String>> {
    if ids.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must contain at most {max} entries"
        )));
    }
    let mut out: Vec<String> = Vec::new();
    for raw in ids {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(UnifiedError::bad_request(format!(
                "{field} must not contain empty entries"
            )));
        }
        if !out.contains(&trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    Ok(out)
}

fn validate_escalation_steps(steps: &[EscalationStep]) -> UnifiedResult<()> {
    if steps.is_empty() || steps.len() > MAX_STEPS {
        return Err(UnifiedError::bad_request(format!(
            "steps must contain between 1 and {MAX_STEPS} entries"
        )));
    }
    let mut previous_minutes: Option<i64> = None;
    for step in steps {
        if step.after_minutes < 0 || step.after_minutes > 60 * 24 * 30 {
            return Err(UnifiedError::bad_request(
                "steps[].after_minutes must be between 0 and 43200 (30 days)",
            ));
        }
        if let Some(previous) = previous_minutes {
            if step.after_minutes <= previous {
                return Err(UnifiedError::bad_request(
                    "steps[].after_minutes must strictly increase across steps",
                ));
            }
        }
        previous_minutes = Some(step.after_minutes);
        match step.target.as_str() {
            "responder" => {}
            "manager" => {
                if step.manager_id.as_deref().map(str::trim).unwrap_or("").is_empty() {
                    return Err(UnifiedError::bad_request(
                        "steps[] with target \"manager\" requires manager_id",
                    ));
                }
            }
            "user" => {
                if step.user_id.as_deref().map(str::trim).unwrap_or("").is_empty() {
                    return Err(UnifiedError::bad_request(
                        "steps[] with target \"user\" requires user_id",
                    ));
                }
            }
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "steps[].target must be \"responder\", \"manager\", or \"user\", got \"{other}\""
                )))
            }
        }
    }
    Ok(())
}

fn load_incident(state: &IncidentsState, raw_id: &str) -> UnifiedResult<Incident> {
    let id = parse_uuid(raw_id, "incident")?;
    state
        .incidents
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Incident not found: {id}")))
}

fn load_rotation(state: &IncidentsState, raw_id: &str) -> UnifiedResult<OnCallRotation> {
    let id = parse_uuid(raw_id, "rotation")?;
    state
        .rotations
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Rotation not found: {id}")))
}

fn load_policy(state: &IncidentsState, raw_id: &str) -> UnifiedResult<EscalationPolicy> {
    let id = parse_uuid(raw_id, "escalation policy")?;
    state
        .escalation_policies
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Escalation policy not found: {id}")))
}

fn timeline_of(state: &IncidentsState, incident_id: Uuid) -> Vec<IncidentTimelineEntry> {
    let mut items: Vec<IncidentTimelineEntry> = state
        .timeline
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| entry.incident_id == incident_id)
        .collect();
    items.sort_by_key(|entry| entry.created_at);
    items
}

/// Who is on call for `rotation` at `now`: an `override` schedule
/// entry covering now wins; otherwise a `regular` entry covering
/// now wins; otherwise the rotation's `current_index` member (the
/// handover cycle). Returns `None` when the rotation has no
/// members.
fn on_call_member(state: &IncidentsState, rotation: &OnCallRotation, now: DateTime<Utc>) -> Option<String> {
    let mut covering: Vec<OnCallSchedule> = state
        .schedules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            entry.rotation_id == rotation.id && entry.starts_at <= now && now < entry.ends_at
        })
        .collect();
    // Overrides first, then the latest-starting regular entry.
    covering.sort_by(|a, b| {
        let rank = |kind: &str| if kind == "override" { 0 } else { 1 };
        rank(&a.kind)
            .cmp(&rank(&b.kind))
            .then(b.starts_at.cmp(&a.starts_at))
    });
    if let Some(entry) = covering.first() {
        return Some(entry.member_id.clone());
    }
    rotation.members.get(rotation.current_index).cloned().or_else(|| {
        rotation.members.first().cloned()
    })
}

/// Advance a rotation to its next member (handover). The handover
/// time of day is honoured by the caller scheduling handovers; this
/// helper only moves the index and records the move.
fn advance_rotation(state: &IncidentsState, rotation: &mut OnCallRotation) {
    if rotation.members.is_empty() {
        return;
    }
    rotation.current_index = (rotation.current_index + 1) % rotation.members.len();
    rotation.updated_at = Utc::now();
    state.rotations.insert(rotation.id, rotation.clone());
}

/// Start escalation tracking for an incident: the first policy step
/// becomes due `after_minutes` after now. Only when the incident
/// has a policy and is still `open` or `acknowledged`.
fn start_escalation(state: &IncidentsState, incident: &Incident) {
    let Some(policy_id) = incident.escalation_policy_id else {
        return;
    };
    let Some(policy) = state
        .escalation_policies
        .get(&policy_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    if policy.steps.is_empty() {
        return;
    }
    let first = &policy.steps[0];
    let now = Utc::now();
    let escalation = IncidentEscalation {
        id: Uuid::now_v7(),
        incident_id: incident.id,
        policy_id,
        step_index: 0,
        next_escalation_at: now + chrono::Duration::minutes(first.after_minutes),
        status: "active".to_string(),
        created_at: now,
        updated_at: now,
    };
    state.escalations.insert(escalation.id, escalation);
}

fn stop_escalation(state: &IncidentsState, incident_id: Uuid) {
    for entry in state.escalations.iter_mut() {
        if entry.value().incident_id == incident_id && entry.value().status == "active" {
            entry.value_mut().status = "stopped".to_string();
            entry.value_mut().updated_at = Utc::now();
        }
    }
}

/// Page one user for an incident: record the attempt, write a
/// timeline entry, and emit a high-priority `notification.requested`.
/// Returns the recorded attempt.
fn page_user(
    state: &IncidentsState,
    incident: &Incident,
    user_id: &str,
    channel: &str,
) -> PageAttempt {
    let now = Utc::now();
    let attempt = PageAttempt {
        id: Uuid::now_v7(),
        incident_id: incident.id,
        paged_user_id: user_id.to_string(),
        channel: channel.to_string(),
        status: "sent".to_string(),
        acknowledged_at: None,
        created_at: now,
        updated_at: now,
    };
    state.page_attempts.insert(attempt.id, attempt.clone());
    state.add_timeline(
        incident.id,
        "system",
        "page",
        format!("Paged {user_id} via {channel} for incident \"{}\"", incident.title),
    );
    state.notify(
        "incidents.paged",
        user_id,
        &format!("Incident: {}", incident.title),
        &format!(
            "You have been paged for a {} severity incident: {}",
            incident.severity.as_str(),
            incident.title
        ),
        Some(format!("/api/v1/incidents/{}", incident.id)),
        "high",
    );
    state.emit(
        "incidents.paged",
        json!({
            "incident_id": incident.id.to_string(),
            "paged_user_id": user_id,
            "channel": channel,
            "page_attempt_id": attempt.id.to_string(),
            "severity": incident.severity.as_str(),
        }),
    );
    attempt
}

/// Resolve the escalation target of `step` for `incident` to a
/// concrete user id: the next rotation member, the step's manager,
/// or the step's user.
fn escalation_target_user(
    state: &IncidentsState,
    incident: &Incident,
    step: &EscalationStep,
) -> Option<String> {
    match step.target.as_str() {
        "manager" => step.manager_id.clone(),
        "user" => step.user_id.clone(),
        _ => {
            // "responder": the next member of the incident's
            // rotation after the current responder, else the
            // current on-call member, else the first responder.
            if let Some(rotation_id) = incident.rotation_id {
                if let Some(rotation) = state
                    .rotations
                    .get(&rotation_id)
                    .map(|entry| entry.value().clone())
                {
                    if let Some(current) = &incident.current_responder {
                        if let Some(position) =
                            rotation.members.iter().position(|member| member == current)
                        {
                            if !rotation.members.is_empty() {
                                let next = (position + 1) % rotation.members.len();
                                return Some(rotation.members[next].clone());
                            }
                        }
                    }
                    return on_call_member(state, &rotation, Utc::now());
                }
            }
            incident.responders.first().cloned()
        }
    }
}

// ---------------------------------------------------------------------------
// Incident handlers
// ---------------------------------------------------------------------------

async fn list_incidents(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let severity_filter = params.filters.get("severity").cloned();
        let responder_filter = params.filters.get("responder_id").cloned();
        let service_filter = params.filters.get("service_id").cloned();
        let source_filter = params.filters.get("source").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Incident> = state
            .incidents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|incident| {
                if let Some(status) = &status_filter {
                    if incident.status.as_str() != status {
                        return false;
                    }
                }
                if let Some(severity) = &severity_filter {
                    if incident.severity.as_str() != severity {
                        return false;
                    }
                }
                if let Some(responder) = &responder_filter {
                    if !incident.responders.iter().any(|id| id == responder)
                        && incident.current_responder.as_deref() != Some(responder.as_str())
                    {
                        return false;
                    }
                }
                if let Some(service) = &service_filter {
                    if !incident.affected_service_ids.iter().any(|id| id == service) {
                        return false;
                    }
                }
                if let Some(source) = &source_filter {
                    if &incident.source != source {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        incident.title,
                        incident.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|incident| incident.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_incident(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    body: web::Json<CreateIncidentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let affected = validate_user_ids(&input.affected_service_ids, "affected_service_ids", MAX_SERVICES)?;
        let responders = validate_user_ids(&input.responders, "responders", MAX_RESPONDERS)?;
        if let Some(policy_id) = input.escalation_policy_id {
            if state.escalation_policies.get(&policy_id).is_none() {
                return Err(UnifiedError::not_found(format!(
                    "Escalation policy not found: {policy_id}"
                )));
            }
        }
        if let Some(rotation_id) = input.rotation_id {
            if state.rotations.get(&rotation_id).is_none() {
                return Err(UnifiedError::not_found(format!("Rotation not found: {rotation_id}")));
            }
        }
        let now = Utc::now();
        let incident = Incident {
            id: Uuid::now_v7(),
            title,
            description,
            severity: input.severity.unwrap_or_default(),
            status: IncidentStatus::Open,
            affected_service_ids: affected,
            responders,
            current_responder: None,
            source: "manual".to_string(),
            source_service_id: None,
            escalation_policy_id: input.escalation_policy_id,
            rotation_id: input.rotation_id,
            created_by: caller.actor.clone(),
            acknowledged_by: None,
            acknowledged_at: None,
            resolved_at: None,
            closed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.incidents.insert(incident.id, incident.clone());
        state.add_timeline(
            incident.id,
            &caller.actor,
            "system",
            format!("Incident opened by {}", caller.actor),
        );
        start_escalation(&state, &incident);
        state.emit(
            "incidents.created",
            json!({
                "incident_id": incident.id.to_string(),
                "title": incident.title,
                "severity": incident.severity.as_str(),
                "status": incident.status.as_str(),
                "created_by": caller.actor,
                "affected_service_ids": incident.affected_service_ids,
            }),
        );
        // Notify every listed responder that the incident exists.
        for responder in &incident.responders {
            state.notify(
                "incidents.created",
                responder,
                &format!("Incident: {}", incident.title),
                &format!(
                    "A {} severity incident was opened: {}",
                    incident.severity.as_str(),
                    incident.title
                ),
                Some(format!("/api/v1/incidents/{}", incident.id)),
                "normal",
            );
        }
        Ok(incident)
    })();
    match result {
        Ok(incident) => created(incident),
        Err(error) => err(error, debug),
    }
}

async fn get_incident(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        load_incident(&state, &path.into_inner())
    })();
    match result {
        Ok(incident) => ok(incident),
        Err(error) => err(error, debug),
    }
}

async fn update_incident(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateIncidentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let incident = load_incident(&state, &path.into_inner())?;
        if incident.status == IncidentStatus::Closed {
            return Err(UnifiedError::conflict("A closed incident cannot be updated"));
        }
        let input = body.into_inner();
        let mut updated = incident.clone();
        if let Some(title) = input.title {
            updated.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = input.description {
            updated.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(severity) = input.severity {
            updated.severity = severity;
        }
        if let Some(services) = input.affected_service_ids {
            updated.affected_service_ids =
                validate_user_ids(&services, "affected_service_ids", MAX_SERVICES)?;
        }
        if let Some(policy) = input.escalation_policy_id {
            match policy {
                Some(policy_id) => {
                    if state.escalation_policies.get(&policy_id).is_none() {
                        return Err(UnifiedError::not_found(format!(
                            "Escalation policy not found: {policy_id}"
                        )));
                    }
                    updated.escalation_policy_id = Some(policy_id);
                }
                None => updated.escalation_policy_id = None,
            }
        }
        if let Some(rotation) = input.rotation_id {
            match rotation {
                Some(rotation_id) => {
                    if state.rotations.get(&rotation_id).is_none() {
                        return Err(UnifiedError::not_found(format!(
                            "Rotation not found: {rotation_id}"
                        )));
                    }
                    updated.rotation_id = Some(rotation_id);
                }
                None => updated.rotation_id = None,
            }
        }
        updated.updated_at = Utc::now();
        state.incidents.insert(updated.id, updated.clone());
        state.emit(
            "incidents.updated",
            json!({
                "incident_id": updated.id.to_string(),
                "updated_by": caller.actor,
            }),
        );
        Ok(updated)
    })();
    match result {
        Ok(incident) => ok(incident),
        Err(error) => err(error, debug),
    }
}

/// Apply a validated status transition, stamping the lifecycle
/// timestamps and writing a timeline entry. Shared by the
/// acknowledge endpoint, the status endpoint, and the Service
/// Monitor recovery listener.
fn apply_status_transition(
    state: &IncidentsState,
    incident: &mut Incident,
    next: IncidentStatus,
    actor: &str,
    note: Option<String>,
) -> UnifiedResult<()> {
    if !incident.status.allowed_next().contains(&next) {
        return Err(UnifiedError::conflict(format!(
            "Invalid status transition from \"{}\" to \"{}\"",
            incident.status.as_str(),
            next.as_str()
        )));
    }
    let previous = incident.status;
    let now = Utc::now();
    incident.status = next;
    incident.updated_at = now;
    match next {
        IncidentStatus::Acknowledged => {
            incident.acknowledged_by = Some(actor.to_string());
            incident.acknowledged_at = Some(now);
            stop_escalation(state, incident.id);
        }
        IncidentStatus::Resolved => {
            incident.resolved_at = Some(now);
            stop_escalation(state, incident.id);
        }
        IncidentStatus::Closed => {
            incident.closed_at = Some(now);
            stop_escalation(state, incident.id);
        }
        IncidentStatus::Investigating if previous == IncidentStatus::Resolved => {
            // Reopen: clear the resolution stamp.
            incident.resolved_at = None;
        }
        _ => {}
    }
    state.incidents.insert(incident.id, incident.clone());
    let mut message = format!(
        "Status changed from \"{}\" to \"{}\" by {}",
        previous.as_str(),
        next.as_str(),
        actor
    );
    if let Some(note) = note {
        message.push_str(&format!(" — {note}"));
    }
    state.add_timeline(incident.id, actor, "status_change", message);
    state.emit(
        "incidents.status_changed",
        json!({
            "incident_id": incident.id.to_string(),
            "previous_status": previous.as_str(),
            "status": next.as_str(),
            "actor_id": actor,
        }),
    );
    match next {
        IncidentStatus::Resolved => state.emit(
            "incidents.resolved",
            json!({
                "incident_id": incident.id.to_string(),
                "resolved_at": now.to_rfc3339(),
            }),
        ),
        IncidentStatus::Closed => state.emit(
            "incidents.closed",
            json!({
                "incident_id": incident.id.to_string(),
                "closed_at": now.to_rfc3339(),
            }),
        ),
        _ => {}
    }
    Ok(())
}

async fn acknowledge_incident(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let mut incident = load_incident(&state, &path.into_inner())?;
        // Acknowledgement also closes out any open page attempts
        // for the caller (or for the current responder).
        let now = Utc::now();
        for mut entry in state.page_attempts.iter_mut() {
            if entry.value().incident_id == incident.id
                && entry.value().status == "sent"
                && (entry.value().paged_user_id == caller.actor
                    || Some(entry.value().paged_user_id.clone()) == incident.current_responder)
            {
                entry.value_mut().status = "acknowledged".to_string();
                entry.value_mut().acknowledged_at = Some(now);
                entry.value_mut().updated_at = now;
            }
        }
        apply_status_transition(&state, &mut incident, IncidentStatus::Acknowledged, &caller.actor, None)?;
        state.emit(
            "incidents.acknowledged",
            json!({
                "incident_id": incident.id.to_string(),
                "acknowledged_by": caller.actor,
                "acknowledged_at": now.to_rfc3339(),
            }),
        );
        state.emit(
            "incidents.page.acknowledged",
            json!({
                "incident_id": incident.id.to_string(),
                "acknowledged_by": caller.actor,
            }),
        );
        Ok(incident)
    })();
    match result {
        Ok(incident) => ok(incident),
        Err(error) => err(error, debug),
    }
}

async fn change_incident_status(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ChangeStatusRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let mut incident = load_incident(&state, &path.into_inner())?;
        let input = body.into_inner();
        let note = match &input.note {
            Some(raw) => Some(require_text(raw, "note", MAX_MESSAGE_LEN)?),
            None => None,
        };
        apply_status_transition(&state, &mut incident, input.status, &caller.actor, note)?;
        Ok(incident)
    })();
    match result {
        Ok(incident) => ok(incident),
        Err(error) => err(error, debug),
    }
}

async fn page_incident(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let mut incident = load_incident(&state, &path.into_inner())?;
        if matches!(incident.status, IncidentStatus::Resolved | IncidentStatus::Closed) {
            return Err(UnifiedError::conflict(format!(
                "A {} incident cannot be paged",
                incident.status.as_str()
            )));
        }
        let input = body.into_inner();
        let channel = require_text(&input.channel, "channel", 64)?;
        // Explicit user first; otherwise the current on-call member
        // of the incident's rotation; otherwise the first responder.
        let target = match input.user_id {
            Some(user_id) => {
                let trimmed = user_id.trim().to_string();
                if trimmed.is_empty() {
                    return Err(UnifiedError::bad_request("user_id must not be empty"));
                }
                trimmed
            }
            None => {
                let rotation_member = incident.rotation_id.and_then(|rotation_id| {
                    state
                        .rotations
                        .get(&rotation_id)
                        .map(|entry| entry.value().clone())
                        .and_then(|rotation| on_call_member(&state, &rotation, Utc::now()))
                });
                match rotation_member {
                    Some(member) => member,
                    None => incident
                        .responders
                        .first()
                        .cloned()
                        .ok_or_else(|| UnifiedError::conflict(
                            "No responder to page: the incident has no rotation and no responders",
                        ))?,
                }
            }
        };
        let attempt = page_user(&state, &incident, &target, &channel);
        incident.current_responder = Some(target.clone());
        incident.updated_at = Utc::now();
        state.incidents.insert(incident.id, incident.clone());
        Ok(json!({
            "paged": true,
            "incident_id": incident.id.to_string(),
            "paged_user_id": target,
            "page_attempt_id": attempt.id.to_string(),
            "channel": channel,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn escalate_incident(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let incident = load_incident(&state, &path.into_inner())?;
        escalate_one(&state, &incident, &caller.actor)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Run one escalation step for `incident`: move its escalation
/// state to the next step, page the step's target, and emit
/// `incidents.escalated`. Returns an error when the incident has
/// no active escalation (no policy, already acknowledged, or all
/// steps exhausted).
fn escalate_one(state: &IncidentsState, incident: &Incident, actor: &str) -> UnifiedResult<Value> {
    if matches!(
        incident.status,
        IncidentStatus::Resolved | IncidentStatus::Closed
    ) {
        return Err(UnifiedError::conflict(format!(
            "A {} incident cannot be escalated",
            incident.status.as_str()
        )));
    }
    let Some(policy_id) = incident.escalation_policy_id else {
        return Err(UnifiedError::conflict(
            "Incident has no escalation policy",
        ));
    };
    let policy = state
        .escalation_policies
        .get(&policy_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Escalation policy not found: {policy_id}")))?;
    let mut escalation = state
        .escalations
        .iter_mut()
        .map(|entry| entry.value().clone())
        .find(|entry| entry.incident_id == incident.id && entry.status == "active")
        .ok_or_else(|| {
            UnifiedError::conflict("Incident has no active escalation (already acknowledged or all steps exhausted)")
        })?;
    if escalation.step_index >= policy.steps.len() {
        return Err(UnifiedError::conflict("All escalation steps are exhausted"));
    }
    let step = &policy.steps[escalation.step_index];
    let target = escalation_target_user(state, incident, step).ok_or_else(|| {
        UnifiedError::conflict("Escalation step has no reachable target user")
    })?;
    let attempt = page_user(state, incident, &target, "notification");
    let now = Utc::now();
    let step_index = escalation.step_index;
    escalation.step_index += 1;
    if escalation.step_index >= policy.steps.len() {
        escalation.status = "completed".to_string();
    } else {
        let next = &policy.steps[escalation.step_index];
        escalation.next_escalation_at =
            now + chrono::Duration::minutes(next.after_minutes - step.after_minutes);
    }
    escalation.updated_at = now;
    state.escalations.insert(escalation.id, escalation.clone());
    let mut updated = incident.clone();
    updated.current_responder = Some(target.clone());
    updated.updated_at = now;
    state.incidents.insert(updated.id, updated);
    state.add_timeline(
        incident.id,
        actor,
        "escalation",
        format!(
            "Escalated to step {} (target {target}) by {actor}",
            step_index + 1
        ),
    );
    state.emit(
        "incidents.escalated",
        json!({
            "incident_id": incident.id.to_string(),
            "policy_id": policy.id.to_string(),
            "step": step_index + 1,
            "paged_user_id": target,
            "page_attempt_id": attempt.id.to_string(),
            "escalated_by": actor,
        }),
    );
    Ok(json!({
        "escalated": true,
        "incident_id": incident.id.to_string(),
        "step": step_index + 1,
        "paged_user_id": target,
        "page_attempt_id": attempt.id.to_string(),
        "escalation_status": escalation.status,
    }))
}

async fn list_timeline(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        let incident = load_incident(&state, &path.into_inner())?;
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let actor_filter = params.filters.get("actor_id").cloned();
        let mut items = timeline_of(&state, incident.id);
        if let Some(kind) = &kind_filter {
            items.retain(|entry| &entry.kind == kind);
        }
        if let Some(actor) = &actor_filter {
            items.retain(|entry| &entry.actor_id == actor);
        }
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn add_timeline_note(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddTimelineRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let incident = load_incident(&state, &path.into_inner())?;
        let input = body.into_inner();
        let message = require_text(&input.message, "message", MAX_MESSAGE_LEN)?;
        state.add_timeline(incident.id, &caller.actor, "note", message.clone());
        state.emit(
            "incidents.timeline.added",
            json!({
                "incident_id": incident.id.to_string(),
                "actor_id": caller.actor,
                "kind": "note",
            }),
        );
        Ok(json!({
            "added": true,
            "incident_id": incident.id.to_string(),
            "kind": "note",
            "message": message,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn add_responder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddResponderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let mut incident = load_incident(&state, &path.into_inner())?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", 255)?;
        if incident.responders.contains(&user_id) {
            return Err(UnifiedError::conflict(format!(
                "User \"{user_id}\" is already a responder"
            )));
        }
        if incident.responders.len() >= MAX_RESPONDERS {
            return Err(UnifiedError::conflict(format!(
                "An incident may have at most {MAX_RESPONDERS} responders"
            )));
        }
        incident.responders.push(user_id.clone());
        incident.updated_at = Utc::now();
        state.incidents.insert(incident.id, incident.clone());
        state.add_timeline(
            incident.id,
            &caller.actor,
            "responder_action",
            format!("Added responder {user_id}"),
        );
        Ok(incident)
    })();
    match result {
        Ok(incident) => ok(incident),
        Err(error) => err(error, debug),
    }
}

async fn remove_responder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let (incident_id, user_id) = path.into_inner();
        let mut incident = load_incident(&state, &incident_id)?;
        let user_id = user_id.trim().to_string();
        if !incident.responders.contains(&user_id) {
            return Err(UnifiedError::not_found(format!(
                "User \"{user_id}\" is not a responder of this incident"
            )));
        }
        incident.responders.retain(|id| id != &user_id);
        if incident.current_responder.as_deref() == Some(user_id.as_str()) {
            incident.current_responder = None;
        }
        incident.updated_at = Utc::now();
        state.incidents.insert(incident.id, incident.clone());
        state.add_timeline(
            incident.id,
            &caller.actor,
            "responder_action",
            format!("Removed responder {user_id}"),
        );
        Ok(incident)
    })();
    match result {
        Ok(incident) => ok(incident),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Rotation handlers
// ---------------------------------------------------------------------------

async fn list_rotations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        let params = query.into_inner();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<OnCallRotation> = state
            .rotations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rotation| {
                search.is_empty() || rotation.name.to_lowercase().contains(&search)
            })
            .collect();
        items.sort_by_key(|rotation| rotation.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_rotation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    body: web::Json<CreateRotationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OnCallRotation> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let members = validate_user_ids(&input.members, "members", MAX_MEMBERS)?;
        if input.handover_interval_days < 1 || input.handover_interval_days > 365 {
            return Err(UnifiedError::bad_request(
                "handover_interval_days must be between 1 and 365",
            ));
        }
        if !valid_handover_time(&input.handover_time) {
            return Err(UnifiedError::bad_request("handover_time must be \"HH:MM\" in 24-hour form"));
        }
        if !valid_timezone(&input.timezone) {
            return Err(UnifiedError::bad_request("timezone must be a valid IANA name"));
        }
        let now = Utc::now();
        let rotation = OnCallRotation {
            id: Uuid::now_v7(),
            name,
            description,
            members,
            handover_interval_days: input.handover_interval_days,
            handover_time: input.handover_time,
            timezone: input.timezone,
            current_index: 0,
            created_at: now,
            updated_at: now,
        };
        state.rotations.insert(rotation.id, rotation.clone());
        Ok(rotation)
    })();
    match result {
        Ok(rotation) => created(rotation),
        Err(error) => err(error, debug),
    }
}

async fn get_rotation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OnCallRotation> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        load_rotation(&state, &path.into_inner())
    })();
    match result {
        Ok(rotation) => ok(rotation),
        Err(error) => err(error, debug),
    }
}

async fn update_rotation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRotationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OnCallRotation> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let rotation = load_rotation(&state, &path.into_inner())?;
        let input = body.into_inner();
        let mut updated = rotation.clone();
        if let Some(name) = input.name {
            updated.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = input.description {
            updated.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(days) = input.handover_interval_days {
            if days < 1 || days > 365 {
                return Err(UnifiedError::bad_request(
                    "handover_interval_days must be between 1 and 365",
                ));
            }
            updated.handover_interval_days = days;
        }
        if let Some(time) = input.handover_time {
            if !valid_handover_time(&time) {
                return Err(UnifiedError::bad_request(
                    "handover_time must be \"HH:MM\" in 24-hour form",
                ));
            }
            updated.handover_time = time;
        }
        if let Some(timezone) = input.timezone {
            if !valid_timezone(&timezone) {
                return Err(UnifiedError::bad_request("timezone must be a valid IANA name"));
            }
            updated.timezone = timezone;
        }
        updated.updated_at = Utc::now();
        state.rotations.insert(updated.id, updated.clone());
        Ok(updated)
    })();
    match result {
        Ok(rotation) => ok(rotation),
        Err(error) => err(error, debug),
    }
}

async fn delete_rotation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let rotation = load_rotation(&state, &path.into_inner())?;
        if state
            .incidents
            .iter()
            .any(|entry| entry.value().rotation_id == Some(rotation.id))
        {
            return Err(UnifiedError::conflict(
                "Rotation is still used by incidents; detach it first",
            ));
        }
        for entry in state
            .schedules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.rotation_id == rotation.id)
        {
            state.schedules.remove(&entry.id);
        }
        state.rotations.remove(&rotation.id);
        Ok(json!({ "deleted": true, "rotation_id": rotation.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn add_rotation_member(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddMemberRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OnCallRotation> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let mut rotation = load_rotation(&state, &path.into_inner())?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", 255)?;
        if rotation.members.contains(&user_id) {
            return Err(UnifiedError::conflict(format!(
                "User \"{user_id}\" is already a rotation member"
            )));
        }
        if rotation.members.len() >= MAX_MEMBERS {
            return Err(UnifiedError::conflict(format!(
                "A rotation may have at most {MAX_MEMBERS} members"
            )));
        }
        rotation.members.push(user_id);
        rotation.updated_at = Utc::now();
        state.rotations.insert(rotation.id, rotation.clone());
        Ok(rotation)
    })();
    match result {
        Ok(rotation) => ok(rotation),
        Err(error) => err(error, debug),
    }
}

async fn remove_rotation_member(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OnCallRotation> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let (rotation_id, user_id) = path.into_inner();
        let mut rotation = load_rotation(&state, &rotation_id)?;
        let user_id = user_id.trim().to_string();
        if !rotation.members.contains(&user_id) {
            return Err(UnifiedError::not_found(format!(
                "User \"{user_id}\" is not a member of this rotation"
            )));
        }
        rotation.members.retain(|id| id != &user_id);
        if rotation.current_index >= rotation.members.len() {
            rotation.current_index = 0;
        }
        rotation.updated_at = Utc::now();
        state.rotations.insert(rotation.id, rotation.clone());
        Ok(rotation)
    })();
    match result {
        Ok(rotation) => ok(rotation),
        Err(error) => err(error, debug),
    }
}

async fn handover_rotation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OnCallRotation> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let mut rotation = load_rotation(&state, &path.into_inner())?;
        if rotation.members.is_empty() {
            return Err(UnifiedError::conflict(
                "Rotation has no members to hand over to",
            ));
        }
        let previous = rotation
            .members
            .get(rotation.current_index)
            .cloned();
        advance_rotation(&state, &mut rotation);
        let current = rotation
            .members
            .get(rotation.current_index)
            .cloned();
        state.emit(
            "incidents.rotation.handover",
            json!({
                "rotation_id": rotation.id.to_string(),
                "previous_user_id": previous,
                "current_user_id": current,
                "handed_over_by": caller.actor,
            }),
        );
        Ok(rotation)
    })();
    match result {
        Ok(rotation) => ok(rotation),
        Err(error) => err(error, debug),
    }
}

async fn get_on_call(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        let rotation = load_rotation(&state, &path.into_inner())?;
        let now = Utc::now();
        let member = on_call_member(&state, &rotation, now);
        Ok(json!({
            "rotation_id": rotation.id.to_string(),
            "on_call_user_id": member,
            "checked_at": now.to_rfc3339(),
            "handover_time": rotation.handover_time,
            "handover_interval_days": rotation.handover_interval_days,
            "timezone": rotation.timezone,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_schedules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        let rotation = load_rotation(&state, &path.into_inner())?;
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let member_filter = params.filters.get("member_id").cloned();
        let mut items: Vec<OnCallSchedule> = state
            .schedules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                if entry.rotation_id != rotation.id {
                    return false;
                }
                if let Some(kind) = &kind_filter {
                    if &entry.kind != kind {
                        return false;
                    }
                }
                if let Some(member) = &member_filter {
                    if &entry.member_id != member {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|entry| entry.starts_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn create_schedule_entry(
    state: &IncidentsState,
    rotation: &OnCallRotation,
    input: CreateScheduleRequest,
) -> UnifiedResult<OnCallSchedule> {
    let member_id = require_text(&input.member_id, "member_id", 255)?;
    if !rotation.members.contains(&member_id) {
        return Err(UnifiedError::bad_request(format!(
            "User \"{member_id}\" is not a member of this rotation"
        )));
    }
    if input.ends_at <= input.starts_at {
        return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
    }
    match input.kind.as_str() {
        "regular" | "override" => {}
        other => {
            return Err(UnifiedError::bad_request(format!(
                "kind must be \"regular\" or \"override\", got \"{other}\""
            )))
        }
    }
    if let Some(overrides_id) = input.overrides_entry_id {
        let target = state
            .schedules
            .get(&overrides_id)
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.rotation_id == rotation.id)
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Schedule entry not found: {overrides_id}"))
            })?;
        if input.kind != "override" {
            return Err(UnifiedError::bad_request(
                "overrides_entry_id requires kind \"override\"",
            ));
        }
        if target.kind != "regular" {
            return Err(UnifiedError::bad_request(
                "overrides_entry_id must name a regular schedule entry",
            ));
        }
    }
    let now = Utc::now();
    let entry = OnCallSchedule {
        id: Uuid::now_v7(),
        rotation_id: rotation.id,
        member_id,
        starts_at: input.starts_at,
        ends_at: input.ends_at,
        kind: input.kind,
        overrides_entry_id: input.overrides_entry_id,
        created_at: now,
        updated_at: now,
    };
    state.schedules.insert(entry.id, entry.clone());
    Ok(entry)
}

async fn create_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateScheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OnCallSchedule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let rotation = load_rotation(&state, &path.into_inner())?;
        create_schedule_entry(&state, &rotation, body.into_inner())
    })();
    match result {
        Ok(entry) => created(entry),
        Err(error) => err(error, debug),
    }
}

async fn create_override(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateScheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OnCallSchedule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let rotation = load_rotation(&state, &path.into_inner())?;
        let mut input = body.into_inner();
        input.kind = "override".to_string();
        create_schedule_entry(&state, &rotation, input)
    })();
    match result {
        Ok(entry) => created(entry),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Escalation-policy handlers
// ---------------------------------------------------------------------------

async fn list_policies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        let params = query.into_inner();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<EscalationPolicy> = state
            .escalation_policies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|policy| search.is_empty() || policy.name.to_lowercase().contains(&search))
            .collect();
        items.sort_by_key(|policy| policy.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn validate_severities(values: &[String]) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for raw in values {
        let trimmed = raw.trim().to_lowercase();
        if IncidentSeverity::from_str(&trimmed).is_none() {
            return Err(UnifiedError::bad_request(format!(
                "applies_to_severities must contain only critical, high, medium, low — got \"{raw}\""
            )));
        }
        if !out.contains(&trimmed) {
            out.push(trimmed);
        }
    }
    Ok(out)
}

async fn create_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    body: web::Json<CreateEscalationPolicyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EscalationPolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let severities = validate_severities(&input.applies_to_severities)?;
        validate_escalation_steps(&input.steps)?;
        let now = Utc::now();
        let policy = EscalationPolicy {
            id: Uuid::now_v7(),
            name,
            description,
            applies_to_severities: severities,
            steps: input.steps,
            created_at: now,
            updated_at: now,
        };
        state.escalation_policies.insert(policy.id, policy.clone());
        Ok(policy)
    })();
    match result {
        Ok(policy) => created(policy),
        Err(error) => err(error, debug),
    }
}

async fn get_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EscalationPolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        load_policy(&state, &path.into_inner())
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

async fn update_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateEscalationPolicyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EscalationPolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let policy = load_policy(&state, &path.into_inner())?;
        let input = body.into_inner();
        let mut updated = policy.clone();
        if let Some(name) = input.name {
            updated.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = input.description {
            updated.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(severities) = input.applies_to_severities {
            updated.applies_to_severities = validate_severities(&severities)?;
        }
        if let Some(steps) = input.steps {
            validate_escalation_steps(&steps)?;
            updated.steps = steps;
        }
        updated.updated_at = Utc::now();
        state.escalation_policies.insert(updated.id, updated.clone());
        Ok(updated)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

async fn delete_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let policy = load_policy(&state, &path.into_inner())?;
        if state
            .incidents
            .iter()
            .any(|entry| entry.value().escalation_policy_id == Some(policy.id))
        {
            return Err(UnifiedError::conflict(
                "Escalation policy is still used by incidents; detach it first",
            ));
        }
        state.escalation_policies.remove(&policy.id);
        Ok(json!({ "deleted": true, "policy_id": policy.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Process due escalations: every active escalation whose
/// `next_escalation_at` has passed is advanced one step (page the
/// next target, emit `incidents.escalated`). This is the honest
/// boundary for time-based escalation: the module does not run a
/// background timer — the scheduler (or an operator) calls this
/// endpoint, exactly like the events module's reminder scheduler
/// note. Returns the count of escalations processed.
async fn run_escalations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let now = Utc::now();
        let due: Vec<IncidentEscalation> = state
            .escalations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.status == "active" && entry.next_escalation_at <= now)
            .collect();
        let mut processed = 0usize;
        let mut skipped = 0usize;
        for escalation in due {
            let Some(incident) = state
                .incidents
                .get(&escalation.incident_id)
                .map(|entry| entry.value().clone())
            else {
                skipped += 1;
                continue;
            };
            if matches!(
                incident.status,
                IncidentStatus::Resolved | IncidentStatus::Closed | IncidentStatus::Acknowledged
            ) {
                // Stale escalation: the incident moved on without
                // stopping it (acknowledge/resolve stop it, but a
                // direct status change through the listener path may
                // not have). Stop it now instead of paging.
                stop_escalation(&state, incident.id);
                skipped += 1;
                continue;
            }
            match escalate_one(&state, &incident, "escalation-runner") {
                Ok(_) => processed += 1,
                Err(_) => skipped += 1,
            }
        }
        Ok(json!({
            "processed": processed,
            "skipped": skipped,
            "checked_at": now.to_rfc3339(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Postmortem handlers
// ---------------------------------------------------------------------------

fn load_postmortem(state: &IncidentsState, incident_id: Uuid) -> UnifiedResult<IncidentPostmortem> {
    state
        .postmortems
        .iter()
        .map(|entry| entry.value().clone())
        .find(|postmortem| postmortem.incident_id == incident_id)
        .ok_or_else(|| {
            UnifiedError::not_found(format!("Postmortem not found for incident {incident_id}"))
        })
}

fn action_item_from(
    input: CreateActionItemRequest,
    incident: &Incident,
    state: &IncidentsState,
) -> UnifiedResult<PostmortemActionItem> {
    let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
    let assignee = match &input.assignee_id {
        Some(raw) => Some(require_text(raw, "assignee_id", 255)?),
        None => None,
    };
    let now = Utc::now();
    let item = PostmortemActionItem {
        id: Uuid::now_v7(),
        title: title.clone(),
        assignee_id: assignee.clone(),
        status: "open".to_string(),
        task_reference: None,
        created_at: now,
        updated_at: now,
    };
    // Action items become tasks via events: emit a task-create
    // request with the full task payload for the tasks module.
    // Honest boundary (see the module header): no listener in the
    // tasks module consumes `tasks.create.requested` yet, so the
    // item is stored here with status `open` and `task_reference`
    // stays empty until the tasks side picks the request up.
    state.emit(
        "tasks.create.requested",
        json!({
            "title": title,
            "description": format!(
                "Postmortem action item for incident \"{}\" ({})",
                incident.title, incident.id
            ),
            "assignee_id": assignee,
            "source_module": "incidents",
            "source_incident_id": incident.id.to_string(),
            "action_item_id": item.id.to_string(),
        }),
    );
    state.emit(
        "incidents.postmortem.action_item.created",
        json!({
            "incident_id": incident.id.to_string(),
            "action_item_id": item.id.to_string(),
            "title": title,
            "assignee_id": assignee,
        }),
    );
    Ok(item)
}

async fn get_postmortem(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<IncidentPostmortem> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        let incident = load_incident(&state, &path.into_inner())?;
        load_postmortem(&state, incident.id)
    })();
    match result {
        Ok(postmortem) => ok(postmortem),
        Err(error) => err(error, debug),
    }
}

async fn create_postmortem(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreatePostmortemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<IncidentPostmortem> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let incident = load_incident(&state, &path.into_inner())?;
        if !matches!(
            incident.status,
            IncidentStatus::Resolved | IncidentStatus::Closed
        ) {
            return Err(UnifiedError::conflict(format!(
                "A postmortem can only be created for a resolved or closed incident (status is \"{}\")",
                incident.status.as_str()
            )));
        }
        if load_postmortem(&state, incident.id).is_ok() {
            return Err(UnifiedError::conflict(
                "A postmortem already exists for this incident",
            ));
        }
        let input = body.into_inner();
        let summary = require_text(&input.summary, "summary", MAX_SUMMARY_LEN)?;
        let timeline_summary = match &input.timeline_summary {
            Some(raw) => Some(require_text(raw, "timeline_summary", MAX_SUMMARY_LEN)?),
            None => None,
        };
        let root_cause = match &input.root_cause {
            Some(raw) => Some(require_text(raw, "root_cause", MAX_ROOT_CAUSE_LEN)?),
            None => None,
        };
        let mut items: Vec<PostmortemActionItem> = Vec::new();
        for item_input in input.action_items {
            items.push(action_item_from(item_input, &incident, &state)?);
        }
        let now = Utc::now();
        let postmortem = IncidentPostmortem {
            id: Uuid::now_v7(),
            incident_id: incident.id,
            summary,
            timeline_summary,
            root_cause,
            action_items: items,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.postmortems.insert(postmortem.id, postmortem.clone());
        state.add_timeline(
            incident.id,
            &caller.actor,
            "system",
            format!("Postmortem created by {}", caller.actor),
        );
        state.emit(
            "incidents.postmortem.created",
            json!({
                "incident_id": incident.id.to_string(),
                "postmortem_id": postmortem.id.to_string(),
                "created_by": caller.actor,
                "action_items": postmortem.action_items.len(),
            }),
        );
        Ok(postmortem)
    })();
    match result {
        Ok(postmortem) => created(postmortem),
        Err(error) => err(error, debug),
    }
}

async fn add_action_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateActionItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<IncidentPostmortem> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let incident = load_incident(&state, &path.into_inner())?;
        let mut postmortem = load_postmortem(&state, incident.id)?;
        let item = action_item_from(body.into_inner(), &incident, &state)?;
        postmortem.action_items.push(item);
        postmortem.updated_at = Utc::now();
        state.postmortems.insert(postmortem.id, postmortem.clone());
        Ok(postmortem)
    })();
    match result {
        Ok(postmortem) => ok(postmortem),
        Err(error) => err(error, debug),
    }
}

async fn update_action_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateActionItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<IncidentPostmortem> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let (incident_id, item_id) = path.into_inner();
        let incident = load_incident(&state, &incident_id)?;
        let mut postmortem = load_postmortem(&state, incident.id)?;
        let item_uuid = parse_uuid(&item_id, "action item")?;
        let position = postmortem
            .action_items
            .iter()
            .position(|item| item.id == item_uuid)
            .ok_or_else(|| UnifiedError::not_found(format!("Action item not found: {item_id}")))?;
        let input = body.into_inner();
        if let Some(title) = input.title {
            postmortem.action_items[position].title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(assignee) = input.assignee_id {
            postmortem.action_items[position].assignee_id = match assignee {
                Some(raw) => Some(require_text(&raw, "assignee_id", 255)?),
                None => None,
            };
        }
        if let Some(status) = input.status {
            match status.as_str() {
                "open" | "in_progress" | "done" => {
                    postmortem.action_items[position].status = status;
                }
                other => {
                    return Err(UnifiedError::bad_request(format!(
                        "status must be \"open\", \"in_progress\", or \"done\", got \"{other}\""
                    )))
                }
            }
        }
        postmortem.action_items[position].updated_at = Utc::now();
        postmortem.updated_at = Utc::now();
        state.postmortems.insert(postmortem.id, postmortem.clone());
        Ok(postmortem)
    })();
    match result {
        Ok(postmortem) => ok(postmortem),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Auto-open rules + Service Monitor listener
// ---------------------------------------------------------------------------

async fn list_auto_open(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_VIEW)?;
        let params = query.into_inner();
        let enabled_filter = params.filters.get("enabled").cloned();
        let mut items: Vec<AutoOpenRule> = state
            .auto_open_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| {
                if let Some(enabled) = &enabled_filter {
                    let expected = match enabled.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if rule.enabled != expected {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|rule| rule.service_id.clone());
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn set_auto_open(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<IncidentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SetAutoOpenRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AutoOpenRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INCIDENTS_MANAGE)?;
        let service_id = require_text(&path.into_inner(), "service_id", 255)?;
        let input = body.into_inner();
        let rule = AutoOpenRule {
            service_id: service_id.clone(),
            enabled: input.enabled,
            severity: input.severity.unwrap_or_default(),
            auto_resolve_on_recovery: input.auto_resolve_on_recovery,
            updated_at: Utc::now(),
        };
        state.auto_open_rules.insert(service_id, rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(rule) => ok(rule),
        Err(error) => err(error, debug),
    }
}

/// Service Monitor listener: `cluster.health.changed`. A `down`
/// health for a service with an enabled auto-open rule opens an
/// incident (deduplicated: no second open auto-incident for the
/// same service). A recovery appends a timeline note to open
/// auto-incidents for that service and resolves them when the rule
/// asks for it. Auto-open is off by default for every service.
fn on_service_health_changed(state: Arc<IncidentsState>, ctx: &mut HookContext) {
    let Some(service_id) = ctx.get("service_id").and_then(|value| value.as_str()) else {
        return;
    };
    let service_id = service_id.to_string();
    let health = ctx
        .get("health")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let service_name = ctx
        .get("service_name")
        .and_then(|value| value.as_str())
        .unwrap_or(&service_id)
        .to_string();
    if health == "down" {
        let Some(rule) = state
            .auto_open_rules
            .get(&service_id)
            .map(|entry| entry.value().clone())
        else {
            return;
        };
        if !rule.enabled {
            return;
        }
        // Deduplicate: an open auto-incident for this service
        // already exists.
        let already_open = state.incidents.iter().any(|entry| {
            let incident = entry.value();
            incident.source == "auto"
                && incident.source_service_id.as_deref() == Some(service_id.as_str())
                && !matches!(
                    incident.status,
                    IncidentStatus::Resolved | IncidentStatus::Closed
                )
        });
        if already_open {
            return;
        }
        let now = Utc::now();
        let incident = Incident {
            id: Uuid::now_v7(),
            title: format!("Service down: {service_name}"),
            description: ctx
                .get("detail")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            severity: rule.severity,
            status: IncidentStatus::Open,
            affected_service_ids: vec![service_id.clone()],
            responders: Vec::new(),
            current_responder: None,
            source: "auto".to_string(),
            source_service_id: Some(service_id.clone()),
            escalation_policy_id: None,
            rotation_id: None,
            created_by: "service-monitor".to_string(),
            acknowledged_by: None,
            acknowledged_at: None,
            resolved_at: None,
            closed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.incidents.insert(incident.id, incident.clone());
        state.add_timeline(
            incident.id,
            "service-monitor",
            "system",
            format!("Auto-opened from Service Monitor alert for service {service_name}"),
        );
        state.emit(
            "incidents.created",
            json!({
                "incident_id": incident.id.to_string(),
                "title": incident.title,
                "severity": incident.severity.as_str(),
                "status": "open",
                "source": "auto",
                "source_service_id": service_id,
            }),
        );
        return;
    }
    // Recovery (any health other than "down"): note it on open
    // auto-incidents for the service, and resolve them when the
    // rule asks for it.
    let open_auto: Vec<Incident> = state
        .incidents
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|incident| {
            incident.source == "auto"
                && incident.source_service_id.as_deref() == Some(service_id.as_str())
                && !matches!(
                    incident.status,
                    IncidentStatus::Resolved | IncidentStatus::Closed
                )
        })
        .collect();
    for mut incident in open_auto {
        state.add_timeline(
            incident.id,
            "service-monitor",
            "system",
            format!("Service {service_name} recovered (health: {health})"),
        );
        let rule = state
            .auto_open_rules
            .get(&service_id)
            .map(|entry| entry.value().clone());
        if rule.map(|rule| rule.auto_resolve_on_recovery).unwrap_or(false) {
            // Resolve through the machine: open -> acknowledged ->
            // investigating -> mitigating -> resolved. Acknowledge
            // and investigate first so the transition is valid.
            let _ = apply_status_transition(
                &state,
                &mut incident,
                IncidentStatus::Acknowledged,
                "service-monitor",
                Some("Service recovered".to_string()),
            );
            let _ = apply_status_transition(
                &state,
                &mut incident,
                IncidentStatus::Investigating,
                "service-monitor",
                None,
            );
            let _ = apply_status_transition(
                &state,
                &mut incident,
                IncidentStatus::Mitigating,
                "service-monitor",
                None,
            );
            let _ = apply_status_transition(
                &state,
                &mut incident,
                IncidentStatus::Resolved,
                "service-monitor",
                Some(format!("Service {service_name} recovered")),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct IncidentsModule {
    state: IncidentsState,
}

impl IncidentsModule {
    pub fn new() -> Self {
        Self {
            state: IncidentsState::default(),
        }
    }
}

impl Default for IncidentsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for IncidentsModule {
    fn id(&self) -> &'static str {
        "incidents"
    }

    fn name(&self) -> &'static str {
        "Incident & On-Call"
    }

    fn description(&self) -> &'static str {
        "Incidents with a validated status machine and append-only timeline, on-call rotations and schedules with overrides, escalation policies with paging, postmortems whose action items become tasks, and auto-open from Service Monitor alerts (opt-in per service)."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "service_monitor", "tasks"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "service_monitor", "tasks"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("infrastructure")
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
        let state = Arc::new(self.state.clone());
        bus.register(
            SERVICE_HEALTH_CHANGED,
            "incidents",
            Arc::new(move |ctx: &mut HookContext| {
                on_service_health_changed(Arc::clone(&state), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/incidents")
                .app_data(state)
                .route("", web::get().to(list_incidents))
                .route("", web::post().to(create_incident))
                .route("/rotations", web::get().to(list_rotations))
                .route("/rotations", web::post().to(create_rotation))
                .route("/rotations/{id}", web::get().to(get_rotation))
                .route("/rotations/{id}", web::patch().to(update_rotation))
                .route("/rotations/{id}", web::delete().to(delete_rotation))
                .route("/rotations/{id}/members", web::post().to(add_rotation_member))
                .route(
                    "/rotations/{id}/members/{user_id}",
                    web::delete().to(remove_rotation_member),
                )
                .route("/rotations/{id}/on-call", web::get().to(get_on_call))
                .route("/rotations/{id}/handover", web::post().to(handover_rotation))
                .route("/rotations/{id}/schedules", web::get().to(list_schedules))
                .route("/rotations/{id}/schedules", web::post().to(create_schedule))
                .route("/rotations/{id}/overrides", web::post().to(create_override))
                .route("/escalation-policies", web::get().to(list_policies))
                .route("/escalation-policies", web::post().to(create_policy))
                .route("/escalation-policies/{id}", web::get().to(get_policy))
                .route("/escalation-policies/{id}", web::patch().to(update_policy))
                .route("/escalation-policies/{id}", web::delete().to(delete_policy))
                .route("/escalations/run", web::post().to(run_escalations))
                .route("/auto-open", web::get().to(list_auto_open))
                .route("/auto-open/{service_id}", web::post().to(set_auto_open))
                .route("/{id}", web::get().to(get_incident))
                .route("/{id}", web::patch().to(update_incident))
                .route("/{id}/acknowledge", web::post().to(acknowledge_incident))
                .route("/{id}/status", web::post().to(change_incident_status))
                .route("/{id}/page", web::post().to(page_incident))
                .route("/{id}/escalate", web::post().to(escalate_incident))
                .route("/{id}/timeline", web::get().to(list_timeline))
                .route("/{id}/timeline", web::post().to(add_timeline_note))
                .route("/{id}/responders", web::post().to(add_responder))
                .route("/{id}/responders/{user_id}", web::delete().to(remove_responder))
                .route("/{id}/postmortem", web::get().to(get_postmortem))
                .route("/{id}/postmortem", web::post().to(create_postmortem))
                .route("/{id}/postmortem/action-items", web::post().to(add_action_item))
                .route(
                    "/{id}/postmortem/action-items/{item_id}",
                    web::patch().to(update_action_item),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/incidents", owner: "incidents", description: "List incidents (paginated, filter by status, severity, responder_id, service_id, source)." },
            RouteInfo { method: "POST", path: "/api/v1/incidents", owner: "incidents", description: "Open an incident (manual source; responders are notified)." },
            RouteInfo { method: "GET", path: "/api/v1/incidents/{id}", owner: "incidents", description: "Get one incident." },
            RouteInfo { method: "PATCH", path: "/api/v1/incidents/{id}", owner: "incidents", description: "Update an incident (not when closed)." },
            RouteInfo { method: "POST", path: "/api/v1/incidents/{id}/acknowledge", owner: "incidents", description: "Acknowledge an incident (open -> acknowledged; stops escalation; closes open page attempts)." },
            RouteInfo { method: "POST", path: "/api/v1/incidents/{id}/status", owner: "incidents", description: "Validated status transition (open -> acknowledged -> investigating -> mitigating -> resolved -> closed, with reopen)." },
            RouteInfo { method: "POST", path: "/api/v1/incidents/{id}/page", owner: "incidents", description: "Page the on-call responder (high-priority notification.requested; attempt recorded)." },
            RouteInfo { method: "POST", path: "/api/v1/incidents/{id}/escalate", owner: "incidents", description: "Escalate to the next policy step (pages the step target)." },
            RouteInfo { method: "GET", path: "/api/v1/incidents/{id}/timeline", owner: "incidents", description: "List the append-only timeline (filter by kind, actor_id)." },
            RouteInfo { method: "POST", path: "/api/v1/incidents/{id}/timeline", owner: "incidents", description: "Add a note entry to the timeline." },
            RouteInfo { method: "POST", path: "/api/v1/incidents/{id}/responders", owner: "incidents", description: "Add a responder to an incident." },
            RouteInfo { method: "DELETE", path: "/api/v1/incidents/{id}/responders/{user_id}", owner: "incidents", description: "Remove a responder from an incident." },
            RouteInfo { method: "GET", path: "/api/v1/incidents/{id}/postmortem", owner: "incidents", description: "Get the postmortem of an incident." },
            RouteInfo { method: "POST", path: "/api/v1/incidents/{id}/postmortem", owner: "incidents", description: "Create a postmortem (resolved or closed incidents only)." },
            RouteInfo { method: "POST", path: "/api/v1/incidents/{id}/postmortem/action-items", owner: "incidents", description: "Add a postmortem action item (emits tasks.create.requested for the tasks module)." },
            RouteInfo { method: "PATCH", path: "/api/v1/incidents/{id}/postmortem/action-items/{item_id}", owner: "incidents", description: "Update a postmortem action item (title, assignee, status)." },
            RouteInfo { method: "GET", path: "/api/v1/incidents/rotations", owner: "incidents", description: "List on-call rotations." },
            RouteInfo { method: "POST", path: "/api/v1/incidents/rotations", owner: "incidents", description: "Create an on-call rotation (ordered members, handover interval and time)." },
            RouteInfo { method: "GET", path: "/api/v1/incidents/rotations/{id}", owner: "incidents", description: "Get one rotation." },
            RouteInfo { method: "PATCH", path: "/api/v1/incidents/rotations/{id}", owner: "incidents", description: "Update a rotation (name, handover interval/time, timezone)." },
            RouteInfo { method: "DELETE", path: "/api/v1/incidents/rotations/{id}", owner: "incidents", description: "Delete a rotation not used by incidents." },
            RouteInfo { method: "POST", path: "/api/v1/incidents/rotations/{id}/members", owner: "incidents", description: "Add a member to a rotation (appended at the end of the order)." },
            RouteInfo { method: "DELETE", path: "/api/v1/incidents/rotations/{id}/members/{user_id}", owner: "incidents", description: "Remove a member from a rotation." },
            RouteInfo { method: "GET", path: "/api/v1/incidents/rotations/{id}/on-call", owner: "incidents", description: "Who is on call now (override entries win over regular ones)." },
            RouteInfo { method: "POST", path: "/api/v1/incidents/rotations/{id}/handover", owner: "incidents", description: "Hand over to the next rotation member (advances current_index)." },
            RouteInfo { method: "GET", path: "/api/v1/incidents/rotations/{id}/schedules", owner: "incidents", description: "List schedule entries of a rotation (filter by kind, member_id)." },
            RouteInfo { method: "POST", path: "/api/v1/incidents/rotations/{id}/schedules", owner: "incidents", description: "Create a schedule entry (kind regular or override)." },
            RouteInfo { method: "POST", path: "/api/v1/incidents/rotations/{id}/overrides", owner: "incidents", description: "Create an override schedule entry." },
            RouteInfo { method: "GET", path: "/api/v1/incidents/escalation-policies", owner: "incidents", description: "List escalation policies." },
            RouteInfo { method: "POST", path: "/api/v1/incidents/escalation-policies", owner: "incidents", description: "Create an escalation policy (ordered steps with strictly increasing after_minutes)." },
            RouteInfo { method: "GET", path: "/api/v1/incidents/escalation-policies/{id}", owner: "incidents", description: "Get one escalation policy." },
            RouteInfo { method: "PATCH", path: "/api/v1/incidents/escalation-policies/{id}", owner: "incidents", description: "Update an escalation policy." },
            RouteInfo { method: "DELETE", path: "/api/v1/incidents/escalation-policies/{id}", owner: "incidents", description: "Delete an escalation policy not used by incidents." },
            RouteInfo { method: "POST", path: "/api/v1/incidents/escalations/run", owner: "incidents", description: "Process due escalations (called by the scheduler or an operator; pages the next responder)." },
            RouteInfo { method: "GET", path: "/api/v1/incidents/auto-open", owner: "incidents", description: "List per-service auto-open rules (off by default)." },
            RouteInfo { method: "POST", path: "/api/v1/incidents/auto-open/{service_id}", owner: "incidents", description: "Set one service's auto-open rule (enabled, severity, auto-resolve on recovery)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(IncidentsModule::new()));
}
