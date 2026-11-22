// OKR / Goals module — objectives and key results at company,
// team, and personal levels, with cycles / quarters, check-ins,
// cycle-end scoring, computed status, alignment (objective to
// parent objective, key result to project / task), and progress
// roll-up computed from linked work.
//
// Routes live under `/api/v1/okrs`:
//
//   Cycles:
//     GET/POST /cycles — list cycles / create a cycle.
//     GET  /cycles/{id}.
//     POST /cycles/{id}/activate — make it the active cycle
//          (one active cycle at a time per company).
//     POST /cycles/{id}/close — close the cycle and score every
//          objective (0.0–1.0).
//   Objectives:
//     GET/POST /objectives — list / create an objective.
//     GET/PATCH/DELETE /objectives/{id}.
//     GET  /objectives/{id}/alignment — parent chain, children,
//          and linked projects / tasks.
//   Key results:
//     GET/POST /objectives/{id}/key-results — list / add a key result.
//     GET/PATCH/DELETE /key-results/{id}.
//     POST /key-results/{id}/check-ins — periodic progress note +
//          confidence (no value change; see the roll-up note).
//     GET  /key-results/{id}/check-ins — list check-ins.
//     POST /key-results/{id}/override — manual current-value
//          override; requires a reason and writes an audit record.
//     GET/POST /key-results/{id}/links — list / link a project or
//          task.
//     DELETE /key-results/{id}/links/{link_id} — unlink.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory cycles, objectives, key results, check-ins, and links;
// one active cycle per company enforced at write time; key-result
// progress computed as (current - start) / (target - start);
// roll-up from linked tasks refreshed by the `task.completed` and
// `project.task.completed` listeners (a completed linked task
// moves the key result's current value along the start-to-target
// range); manual overrides that require a reason string, are
// stored as an override check-in, and emit the audit-shaped
// `okrs.kr.override` event; status computed from progress versus
// elapsed cycle time; cycle-end scoring (mean key-result progress
// per objective, clamped to 0.0–1.0). What is NOT built here: no
// automatic discovery of a project's total task count — a link of
// type `project` is stored for alignment and shown in the
// alignment view, but the numeric roll-up counts linked *tasks*
// only (a project link with no linked tasks contributes no
// progress, and the alignment view says so); the audit module only
// records events listed in its `AUDITED_EVENTS`, so the integrator
// must add `okrs.kr.override` there for the override to land in
// the audit log (the override check-in row is stored regardless);
// and no notification is sent on cycle close (only
// `okrs.cycle.closed` is emitted).
//
// Public-access note: this module has NO public routes. Every route
// requires the `X-User-Id` / `X-Role` headers and a capability
// check (see the actor-identity note below). Do NOT treat any route
// here as public.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai and events modules. Capability
// checks use the `role.check` hook and fail closed (no answer means
// not allowed).
//
// Events emitted on the hook bus:
//   - `okrs.cycle.created`, `okrs.cycle.closed`
//   - `okrs.objective.created`, `okrs.objective.updated`,
//     `okrs.objective.deleted`
//   - `okrs.key_result.created`, `okrs.key_result.updated`,
//     `okrs.key_result.deleted`
//   - `okrs.kr.checkin`
//   - `okrs.kr.override` (audit-shaped payload: `actor_id`,
//     `resource`, `key_result_id`, `old_value`, `new_value`,
//     `reason` — the audit module records it once the integrator
//     adds the name to `AUDITED_EVENTS` in `audit.rs`).
//   - `okrs.link.created`, `okrs.link.deleted`
// Events this module listens to:
//   - `task.completed` — refreshes roll-ups of key results linked
//     to the completed task (payload: `task_id` / `id`, optional
//     `project_id`, `company_id`).
//   - `project.task.completed` — same, for project tasks (payload:
//     `task_id` / `id`, optional `project_id`).

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
const MAX_NAME_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_TITLE_LEN: usize = 300;
const MAX_UNIT_LEN: usize = 32;
const MAX_NOTE_LEN: usize = 5000;
const MAX_REASON_LEN: usize = 1000;
const MAX_COMPANY_LEN: usize = 128;

/// Capability names checked through the `role.check` hook.
const PERM_OKRS_VIEW: &str = "okrs.view";
const PERM_OKRS_MANAGE: &str = "okrs.manage";

/// Objective levels.
const ALLOWED_LEVELS: &[&str] = &["company", "team", "personal"];

/// Check-in confidence values.
const ALLOWED_CONFIDENCE: &[&str] = &["low", "medium", "high"];

/// Link target types.
const ALLOWED_LINK_TYPES: &[&str] = &["project", "task"];

/// Cycle lifecycle: `draft` -> `active` -> `closed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CycleStatus {
    Draft,
    Active,
    Closed,
}

impl CycleStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Closed => "closed",
        }
    }
}

/// One cycle / quarter. Exactly one cycle may be `active` per
/// company at a time (enforced at write time in the handlers and
/// by a partial unique index in the migration).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OkrCycle {
    pub id: Uuid,
    pub company_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: CycleStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One objective, at company, team, or personal level. A parent
/// objective links the alignment tree; `final_score` is set only
/// when the cycle closes (mean key-result progress, 0.0–1.0).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OkrObjective {
    pub id: Uuid,
    pub cycle_id: Uuid,
    pub company_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_objective_id: Option<Uuid>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub level: String,
    pub owner_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_score: Option<f64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One key result: a target value with a unit, a start value, and
/// a current value. Progress is always computed as
/// (current - start) / (target - start); `current_value` is moved
/// by linked-task completion or by a reasoned manual override —
/// never by a plain hand-typed update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OkrKeyResult {
    pub id: Uuid,
    pub objective_id: Uuid,
    pub title: String,
    pub unit: String,
    pub target_value: f64,
    pub start_value: f64,
    pub current_value: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One check-in: a periodic progress note plus confidence. `kind`
/// is `note` for an ordinary check-in or `override` for a manual
/// value override (which also carries the reason and the value
/// snapshot). `value_snapshot` is the key result's current value
/// at check-in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OkrCheckin {
    pub id: Uuid,
    pub key_result_id: Uuid,
    pub author_id: String,
    pub note: String,
    pub confidence: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_snapshot: Option<f64>,
    pub created_at: DateTime<Utc>,
}

/// One link from a key result to a project or a task. Task links
/// drive the progress roll-up; project links are alignment only
/// (see the module header).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OkrLink {
    pub id: Uuid,
    pub key_result_id: Uuid,
    pub link_type: String,
    pub target_id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// Objective view with computed progress and status, as returned
/// by the list / get endpoints. `progress` is the mean key-result
/// progress (0.0–1.0, or null when the objective has no key
/// results); `expected_progress` is the share of the cycle elapsed;
/// `status` is `on_track`, `at_risk`, or `off_track` (or
/// `not_started` before the cycle starts, `closed` after it
/// closes, `no_key_results` when there is nothing to measure).
#[derive(Debug, Clone, Serialize)]
pub struct ObjectiveView {
    #[serde(flatten)]
    pub objective: OkrObjective,
    pub progress: Option<f64>,
    pub expected_progress: Option<f64>,
    pub status: String,
    pub key_result_count: usize,
}

/// Key-result view with computed progress.
#[derive(Debug, Clone, Serialize)]
pub struct KeyResultView {
    #[serde(flatten)]
    pub key_result: OkrKeyResult,
    pub progress: f64,
    pub linked_task_count: usize,
    pub completed_task_count: usize,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateCycleRequest {
    pub name: String,
    pub description: Option<String>,
    pub company_id: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateObjectiveRequest {
    pub cycle_id: String,
    pub company_id: String,
    pub parent_objective_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub level: String,
    pub owner_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateObjectiveRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub parent_objective_id: Option<Option<String>>,
    pub level: Option<String>,
    pub owner_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateKeyResultRequest {
    pub title: String,
    pub unit: String,
    pub target_value: f64,
    pub start_value: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateKeyResultRequest {
    pub title: Option<String>,
    pub unit: Option<String>,
    pub target_value: Option<f64>,
    pub start_value: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCheckinRequest {
    pub note: String,
    pub confidence: String,
}

#[derive(Debug, Deserialize)]
pub struct OverrideRequest {
    pub value: f64,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateLinkRequest {
    pub link_type: String,
    pub target_id: String,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for OKRs. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); handlers tolerate a missing bus
/// by skipping event emission. Durable rows for these types are
/// defined by migration `2026-11-22-000043_okrs`; until that
/// migration is executed, cycles, objectives, and key results do
/// not survive a restart. `completed_tasks` records the ids of
/// tasks seen completed through the task-completion listeners, so
/// a roll-up can be recomputed even when the completion event
/// arrived before the link was created.
#[derive(Clone)]
pub struct OkrState {
    pub cycles: Arc<DashMap<Uuid, OkrCycle>>,
    pub objectives: Arc<DashMap<Uuid, OkrObjective>>,
    pub key_results: Arc<DashMap<Uuid, OkrKeyResult>>,
    pub checkins: Arc<DashMap<Uuid, OkrCheckin>>,
    pub links: Arc<DashMap<Uuid, OkrLink>>,
    pub completed_tasks: Arc<DashMap<Uuid, DateTime<Utc>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for OkrState {
    fn default() -> Self {
        Self {
            cycles: Arc::new(DashMap::new()),
            objectives: Arc::new(DashMap::new()),
            key_results: Arc::new(DashMap::new()),
            checkins: Arc::new(DashMap::new()),
            links: Arc::new(DashMap::new()),
            completed_tasks: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl OkrState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the ai and events modules.
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
/// (same fail-closed shape as the ai and events modules).
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

/// A number is usable as a key-result value when it is finite.
fn valid_number(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite number"
        )));
    }
    Ok(value)
}

/// Progress of one key result: (current - start) / (target -
/// start), clamped to 0.0–1.0. A zero target range scores 1.0 when
/// the current value reached the target, else 0.0.
fn key_result_progress(key_result: &OkrKeyResult) -> f64 {
    let span = key_result.target_value - key_result.start_value;
    if span == 0.0 {
        if key_result.current_value >= key_result.target_value {
            1.0
        } else {
            0.0
        }
    } else {
        ((key_result.current_value - key_result.start_value) / span).clamp(0.0, 1.0)
    }
}

fn load_cycle(state: &OkrState, raw_id: &str) -> UnifiedResult<OkrCycle> {
    let id = parse_uuid(raw_id, "cycle")?;
    state
        .cycles
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Cycle not found: {id}")))
}

fn load_objective(state: &OkrState, raw_id: &str) -> UnifiedResult<OkrObjective> {
    let id = parse_uuid(raw_id, "objective")?;
    state
        .objectives
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Objective not found: {id}")))
}

fn load_key_result(state: &OkrState, raw_id: &str) -> UnifiedResult<OkrKeyResult> {
    let id = parse_uuid(raw_id, "key result")?;
    state
        .key_results
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Key result not found: {id}")))
}

fn load_link(state: &OkrState, raw_id: &str) -> UnifiedResult<OkrLink> {
    let id = parse_uuid(raw_id, "link")?;
    state
        .links
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Link not found: {id}")))
}

fn key_results_of(state: &OkrState, objective_id: Uuid) -> Vec<OkrKeyResult> {
    let mut out: Vec<OkrKeyResult> = state
        .key_results
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|key_result| key_result.objective_id == objective_id)
        .collect();
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    out
}

fn links_of(state: &OkrState, key_result_id: Uuid) -> Vec<OkrLink> {
    let mut out: Vec<OkrLink> = state
        .links
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|link| link.key_result_id == key_result_id)
        .collect();
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    out
}

/// Mean key-result progress of an objective, or `None` when it has
/// no key results.
fn objective_progress(state: &OkrState, objective_id: Uuid) -> Option<f64> {
    let results = key_results_of(state, objective_id);
    if results.is_empty() {
        return None;
    }
    let sum: f64 = results.iter().map(key_result_progress).sum();
    Some((sum / results.len() as f64).clamp(0.0, 1.0))
}

/// Share of the cycle elapsed at `now`, 0.0 before it starts and
/// 1.0 after it ends. `None` when the cycle has zero length.
fn cycle_elapsed(cycle: &OkrCycle, now: DateTime<Utc>) -> Option<f64> {
    let total = (cycle.ends_at - cycle.starts_at).num_seconds();
    if total <= 0 {
        return None;
    }
    let elapsed = (now - cycle.starts_at).num_seconds();
    Some((elapsed as f64 / total as f64).clamp(0.0, 1.0))
}

/// Computed status from progress versus elapsed cycle time:
/// progress within 0.10 of the expected share is on track, within
/// 0.25 is at risk, further behind is off track. Before the cycle
/// starts the status is `not_started`; a closed cycle reports
/// `closed`; an objective with no key results reports
/// `no_key_results`.
fn objective_status(
    state: &OkrState,
    objective: &OkrObjective,
    now: DateTime<Utc>,
) -> (Option<f64>, Option<f64>, String) {
    let progress = objective_progress(state, objective.id);
    let Some(cycle) = state.cycles.get(&objective.cycle_id).map(|entry| entry.value().clone())
    else {
        return (progress, None, "no_cycle".to_string());
    };
    if cycle.status == CycleStatus::Closed {
        return (progress, Some(1.0), "closed".to_string());
    }
    if now < cycle.starts_at {
        return (progress, Some(0.0), "not_started".to_string());
    }
    let Some(expected) = cycle_elapsed(&cycle, now) else {
        return (progress, None, "no_key_results".to_string());
    };
    let Some(progress) = progress else {
        return (None, Some(expected), "no_key_results".to_string());
    };
    let gap = expected - progress;
    let status = if gap <= 0.10 {
        "on_track"
    } else if gap <= 0.25 {
        "at_risk"
    } else {
        "off_track"
    };
    (Some(progress), Some(expected), status.to_string())
}

fn objective_view(state: &OkrState, objective: &OkrObjective) -> ObjectiveView {
    let now = Utc::now();
    let (progress, expected_progress, status) = objective_status(state, objective, now);
    ObjectiveView {
        objective: objective.clone(),
        progress,
        expected_progress,
        status,
        key_result_count: key_results_of(state, objective.id).len(),
    }
}

fn key_result_view(state: &OkrState, key_result: &OkrKeyResult) -> KeyResultView {
    let links = links_of(state, key_result.id);
    let task_links: Vec<&OkrLink> = links
        .iter()
        .filter(|link| link.link_type == "task")
        .collect();
    let completed = task_links
        .iter()
        .filter(|link| state.completed_tasks.contains_key(&link.target_id))
        .count();
    KeyResultView {
        key_result: key_result.clone(),
        progress: key_result_progress(key_result),
        linked_task_count: task_links.len(),
        completed_task_count: completed,
    }
}

/// Recompute one key result's current value from its linked tasks:
/// current = start + (completed / total) * (target - start). A key
/// result with no task links is left untouched. Returns true when
/// the stored value changed.
fn refresh_rollup(state: &OkrState, key_result_id: Uuid) -> bool {
    let Some(key_result) = state
        .key_results
        .get(&key_result_id)
        .map(|entry| entry.value().clone())
    else {
        return false;
    };
    let task_links: Vec<OkrLink> = links_of(state, key_result_id)
        .into_iter()
        .filter(|link| link.link_type == "task")
        .collect();
    if task_links.is_empty() {
        return false;
    }
    let completed = task_links
        .iter()
        .filter(|link| state.completed_tasks.contains_key(&link.target_id))
        .count();
    let fraction = completed as f64 / task_links.len() as f64;
    let next = key_result.start_value
        + fraction * (key_result.target_value - key_result.start_value);
    if (next - key_result.current_value).abs() < f64::EPSILON {
        return false;
    }
    if let Some(mut stored) = state.key_results.get_mut(&key_result_id) {
        stored.current_value = next;
        stored.updated_at = Utc::now();
    }
    true
}

/// Refresh the roll-up of every key result linked to the completed
/// task id. Unknown task ids are still recorded in
/// `completed_tasks` so a link created later picks the completion
/// up on its next refresh.
fn on_task_completed(state: OkrState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let task_id = data
        .get("task_id")
        .and_then(Value::as_str)
        .or_else(|| data.get("id").and_then(Value::as_str))
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(task_id) = task_id else {
        return;
    };
    state.completed_tasks.insert(task_id, Utc::now());
    let affected: Vec<Uuid> = state
        .links
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|link| link.link_type == "task" && link.target_id == task_id)
        .map(|link| link.key_result_id)
        .collect();
    for key_result_id in affected {
        refresh_rollup(&state, key_result_id);
    }
}

/// Emit an audit-shaped event on the hook bus: the audit module
/// reads `actor_id`, `resource`, and `id` from the payload. The
/// audit module only records events listed in its `AUDITED_EVENTS`,
/// so the integrator must add `okrs.kr.override` there.
fn emit_audit(state: &OkrState, event: &str, actor: &str, resource: &str, extra: Value) {
    let mut payload = match extra {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    payload.insert("actor_id".to_string(), Value::String(actor.to_string()));
    payload.insert("resource".to_string(), Value::String(resource.to_string()));
    state.emit(event, Value::Object(payload));
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct OkrsModule {
    state: OkrState,
}

impl OkrsModule {
    pub fn new() -> Self {
        Self {
            state: OkrState::default(),
        }
    }
}

impl Default for OkrsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for OkrsModule {
    fn id(&self) -> &'static str {
        "okrs"
    }

    fn name(&self) -> &'static str {
        "OKR / Goals"
    }

    fn description(&self) -> &'static str {
        "Objectives and key results at company, team, and personal levels, with cycles / quarters, check-ins, cycle-end scoring, computed status, alignment to parent objectives and linked projects / tasks, and progress roll-up computed from linked task completion."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "projects", "tasks"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "projects", "tasks"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
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
            "task.completed",
            "okrs",
            Arc::new(move |ctx: &mut HookContext| on_task_completed(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "project.task.completed",
            "okrs",
            Arc::new(move |ctx: &mut HookContext| on_task_completed(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/okrs")
                .app_data(state)
                .route("/cycles", web::get().to(list_cycles))
                .route("/cycles", web::post().to(create_cycle))
                .route("/cycles/{id}", web::get().to(get_cycle))
                .route("/cycles/{id}/activate", web::post().to(activate_cycle))
                .route("/cycles/{id}/close", web::post().to(close_cycle))
                .route("/objectives", web::get().to(list_objectives))
                .route("/objectives", web::post().to(create_objective))
                .route("/objectives/{id}", web::get().to(get_objective))
                .route("/objectives/{id}", web::patch().to(update_objective))
                .route("/objectives/{id}", web::delete().to(delete_objective))
                .route(
                    "/objectives/{id}/alignment",
                    web::get().to(get_alignment),
                )
                .route(
                    "/objectives/{id}/key-results",
                    web::get().to(list_key_results),
                )
                .route(
                    "/objectives/{id}/key-results",
                    web::post().to(create_key_result),
                )
                .route("/key-results/{id}", web::get().to(get_key_result))
                .route("/key-results/{id}", web::patch().to(update_key_result))
                .route("/key-results/{id}", web::delete().to(delete_key_result))
                .route(
                    "/key-results/{id}/check-ins",
                    web::get().to(list_checkins),
                )
                .route(
                    "/key-results/{id}/check-ins",
                    web::post().to(create_checkin),
                )
                .route(
                    "/key-results/{id}/override",
                    web::post().to(override_key_result),
                )
                .route("/key-results/{id}/links", web::get().to(list_links))
                .route("/key-results/{id}/links", web::post().to(create_link))
                .route(
                    "/key-results/{id}/links/{link_id}",
                    web::delete().to(delete_link),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/okrs/cycles", owner: "okrs", description: "List cycles (paginated, filter by company_id, status)." },
            RouteInfo { method: "POST", path: "/api/v1/okrs/cycles", owner: "okrs", description: "Create a cycle / quarter (draft; ends_at must be after starts_at)." },
            RouteInfo { method: "GET", path: "/api/v1/okrs/cycles/{id}", owner: "okrs", description: "Get one cycle." },
            RouteInfo { method: "POST", path: "/api/v1/okrs/cycles/{id}/activate", owner: "okrs", description: "Activate a cycle (fails when another cycle of the company is already active)." },
            RouteInfo { method: "POST", path: "/api/v1/okrs/cycles/{id}/close", owner: "okrs", description: "Close a cycle and score every objective (mean key-result progress, 0.0-1.0; emits okrs.cycle.closed)." },
            RouteInfo { method: "GET", path: "/api/v1/okrs/objectives", owner: "okrs", description: "List objectives with computed progress and status (paginated, filter by cycle_id, company_id, level, owner_id, parent_objective_id)." },
            RouteInfo { method: "POST", path: "/api/v1/okrs/objectives", owner: "okrs", description: "Create an objective (company / team / personal level, optional parent objective)." },
            RouteInfo { method: "GET", path: "/api/v1/okrs/objectives/{id}", owner: "okrs", description: "Get one objective with computed progress and status." },
            RouteInfo { method: "PATCH", path: "/api/v1/okrs/objectives/{id}", owner: "okrs", description: "Update an objective (not after its cycle closed)." },
            RouteInfo { method: "DELETE", path: "/api/v1/okrs/objectives/{id}", owner: "okrs", description: "Delete an objective with its key results, check-ins, and links." },
            RouteInfo { method: "GET", path: "/api/v1/okrs/objectives/{id}/alignment", owner: "okrs", description: "Alignment view: parent chain, child objectives, and linked projects / tasks." },
            RouteInfo { method: "GET", path: "/api/v1/okrs/objectives/{id}/key-results", owner: "okrs", description: "List key results of an objective (with computed progress)." },
            RouteInfo { method: "POST", path: "/api/v1/okrs/objectives/{id}/key-results", owner: "okrs", description: "Add a key result (target value + unit, start value)." },
            RouteInfo { method: "GET", path: "/api/v1/okrs/key-results/{id}", owner: "okrs", description: "Get one key result with computed progress." },
            RouteInfo { method: "PATCH", path: "/api/v1/okrs/key-results/{id}", owner: "okrs", description: "Update a key result title / unit / target / start (current value is never hand-typed here)." },
            RouteInfo { method: "DELETE", path: "/api/v1/okrs/key-results/{id}", owner: "okrs", description: "Delete a key result with its check-ins and links." },
            RouteInfo { method: "GET", path: "/api/v1/okrs/key-results/{id}/check-ins", owner: "okrs", description: "List check-ins of a key result." },
            RouteInfo { method: "POST", path: "/api/v1/okrs/key-results/{id}/check-ins", owner: "okrs", description: "Add a check-in (progress note + confidence; records a value snapshot, never sets the value)." },
            RouteInfo { method: "POST", path: "/api/v1/okrs/key-results/{id}/override", owner: "okrs", description: "Manual current-value override (reason required; stored as an override check-in and emitted as the audit-shaped okrs.kr.override event)." },
            RouteInfo { method: "GET", path: "/api/v1/okrs/key-results/{id}/links", owner: "okrs", description: "List project / task links of a key result." },
            RouteInfo { method: "POST", path: "/api/v1/okrs/key-results/{id}/links", owner: "okrs", description: "Link a project or task to a key result (task links drive the roll-up)." },
            RouteInfo { method: "DELETE", path: "/api/v1/okrs/key-results/{id}/links/{link_id}", owner: "okrs", description: "Remove a project / task link (roll-up is recomputed)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(OkrsModule::new()));
}

// ---------------------------------------------------------------------------
// Cycle handlers
// ---------------------------------------------------------------------------

async fn list_cycles(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_VIEW)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<OkrCycle> = state
            .cycles
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|cycle| {
                if let Some(company) = &company_filter {
                    if &cycle.company_id != company {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if cycle.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        cycle.name,
                        cycle.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "starts_at", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.cmp(&b.name),
                "starts_at" => a.starts_at.cmp(&b.starts_at),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_cycle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    body: web::Json<CreateCycleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OkrCycle> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let name = require_text(&body.name, "name", MAX_NAME_LEN)?;
        let company_id = require_text(&body.company_id, "company_id", MAX_COMPANY_LEN)?;
        if body.ends_at <= body.starts_at {
            return Err(UnifiedError::bad_request(
                "ends_at must be after starts_at",
            ));
        }
        let description = match &body.description {
            Some(text) if !text.trim().is_empty() => Some(require_text(
                text,
                "description",
                MAX_DESCRIPTION_LEN,
            )?),
            _ => None,
        };
        let now = Utc::now();
        let cycle = OkrCycle {
            id: Uuid::new_v4(),
            company_id: company_id.clone(),
            name,
            description,
            starts_at: body.starts_at,
            ends_at: body.ends_at,
            status: CycleStatus::Draft,
            created_at: now,
            updated_at: now,
        };
        state.cycles.insert(cycle.id, cycle.clone());
        state.emit(
            "okrs.cycle.created",
            json!({
                "cycle_id": cycle.id.to_string(),
                "company_id": company_id,
                "actor_id": caller.actor,
            }),
        );
        Ok(cycle)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_cycle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OkrCycle> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_VIEW)?;
        load_cycle(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn activate_cycle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OkrCycle> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let mut cycle = load_cycle(&state, &path.into_inner())?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict("A closed cycle cannot be activated"));
        }
        if cycle.status == CycleStatus::Active {
            return Ok(cycle);
        }
        let clash = state.cycles.iter().any(|entry| {
            let other = entry.value();
            other.company_id == cycle.company_id
                && other.id != cycle.id
                && other.status == CycleStatus::Active
        });
        if clash {
            return Err(UnifiedError::conflict(format!(
                "Company \"{}\" already has an active cycle; close it before activating another",
                cycle.company_id
            )));
        }
        cycle.status = CycleStatus::Active;
        cycle.updated_at = Utc::now();
        state.cycles.insert(cycle.id, cycle.clone());
        Ok(cycle)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn close_cycle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let mut cycle = load_cycle(&state, &path.into_inner())?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict("Cycle is already closed"));
        }
        cycle.status = CycleStatus::Closed;
        cycle.updated_at = Utc::now();
        state.cycles.insert(cycle.id, cycle.clone());
        let mut scored: Vec<Value> = Vec::new();
        let objectives: Vec<OkrObjective> = state
            .objectives
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|objective| objective.cycle_id == cycle.id)
            .collect();
        for mut objective in objectives {
            let score = objective_progress(&state, objective.id).unwrap_or(0.0);
            objective.final_score = Some(score);
            objective.updated_at = Utc::now();
            state.objectives.insert(objective.id, objective.clone());
            scored.push(json!({
                "objective_id": objective.id.to_string(),
                "title": objective.title,
                "score": score,
            }));
        }
        state.emit(
            "okrs.cycle.closed",
            json!({
                "cycle_id": cycle.id.to_string(),
                "company_id": cycle.company_id,
                "actor_id": caller.actor,
                "objectives_scored": scored.len(),
            }),
        );
        Ok(json!({
            "cycle_id": cycle.id.to_string(),
            "status": "closed",
            "objectives_scored": scored.len(),
            "scores": scored,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Objective handlers
// ---------------------------------------------------------------------------

async fn list_objectives(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_VIEW)?;
        let params = query.into_inner();
        let cycle_filter = params.filters.get("cycle_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let level_filter = params.filters.get("level").cloned();
        let owner_filter = params.filters.get("owner_id").cloned();
        let parent_filter = params.filters.get("parent_objective_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<ObjectiveView> = state
            .objectives
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|objective| {
                if let Some(cycle_id) = &cycle_filter {
                    if objective.cycle_id.to_string() != *cycle_id {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    if &objective.company_id != company {
                        return false;
                    }
                }
                if let Some(level) = &level_filter {
                    if &objective.level != level {
                        return false;
                    }
                }
                if let Some(owner) = &owner_filter {
                    if &objective.owner_id != owner {
                        return false;
                    }
                }
                if let Some(parent) = &parent_filter {
                    if objective
                        .parent_objective_id
                        .map(|id| id.to_string())
                        .as_deref()
                        != Some(parent.as_str())
                    {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        objective.title,
                        objective.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|objective| objective_view(&state, &objective))
            .collect();
        let sort = params.allowed_sort(&["title", "level", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a.objective.title.cmp(&b.objective.title),
                "level" => a.objective.level.cmp(&b.objective.level),
                _ => a.objective.created_at.cmp(&b.objective.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_objective(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    body: web::Json<CreateObjectiveRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ObjectiveView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let cycle = load_cycle(&state, &body.cycle_id)?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict(
                "Cannot add an objective to a closed cycle",
            ));
        }
        let company_id = require_text(&body.company_id, "company_id", MAX_COMPANY_LEN)?;
        if cycle.company_id != company_id {
            return Err(UnifiedError::bad_request(
                "company_id does not match the cycle's company_id",
            ));
        }
        let title = require_text(&body.title, "title", MAX_TITLE_LEN)?;
        let level = body.level.trim().to_lowercase();
        if !ALLOWED_LEVELS.contains(&level.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "level must be one of: {}",
                ALLOWED_LEVELS.join(", ")
            )));
        }
        let owner_id = match &body.owner_id {
            Some(owner) => require_text(owner, "owner_id", MAX_NAME_LEN)?,
            None => caller.actor.clone(),
        };
        let parent_objective_id = match &body.parent_objective_id {
            Some(raw) => {
                let parent = load_objective(&state, raw)?;
                if parent.cycle_id != cycle.id {
                    return Err(UnifiedError::bad_request(
                        "parent objective must belong to the same cycle",
                    ));
                }
                Some(parent.id)
            }
            None => None,
        };
        let description = match &body.description {
            Some(text) if !text.trim().is_empty() => Some(require_text(
                text,
                "description",
                MAX_DESCRIPTION_LEN,
            )?),
            _ => None,
        };
        let now = Utc::now();
        let objective = OkrObjective {
            id: Uuid::new_v4(),
            cycle_id: cycle.id,
            company_id: company_id.clone(),
            parent_objective_id,
            title,
            description,
            level,
            owner_id: owner_id.clone(),
            final_score: None,
            created_at: now,
            updated_at: now,
        };
        state.objectives.insert(objective.id, objective.clone());
        state.emit(
            "okrs.objective.created",
            json!({
                "objective_id": objective.id.to_string(),
                "cycle_id": cycle.id.to_string(),
                "company_id": company_id,
                "level": objective.level,
                "actor_id": caller.actor,
            }),
        );
        Ok(objective_view(&state, &objective))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_objective(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ObjectiveView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_VIEW)?;
        let objective = load_objective(&state, &path.into_inner())?;
        Ok(objective_view(&state, &objective))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_objective(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateObjectiveRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ObjectiveView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let mut objective = load_objective(&state, &path.into_inner())?;
        let cycle = load_cycle(&state, &objective.cycle_id.to_string())?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict(
                "Cannot update an objective of a closed cycle",
            ));
        }
        if let Some(title) = &body.title {
            objective.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &body.description {
            objective.description = match description {
                Some(text) if !text.trim().is_empty() => Some(require_text(
                    text,
                    "description",
                    MAX_DESCRIPTION_LEN,
                )?),
                _ => None,
            };
        }
        if let Some(level) = &body.level {
            let level = level.trim().to_lowercase();
            if !ALLOWED_LEVELS.contains(&level.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "level must be one of: {}",
                    ALLOWED_LEVELS.join(", ")
                )));
            }
            objective.level = level;
        }
        if let Some(owner_id) = &body.owner_id {
            objective.owner_id = require_text(owner_id, "owner_id", MAX_NAME_LEN)?;
        }
        if let Some(parent) = &body.parent_objective_id {
            objective.parent_objective_id = match parent {
                Some(raw) => {
                    let parent_objective = load_objective(&state, raw)?;
                    if parent_objective.cycle_id != objective.cycle_id {
                        return Err(UnifiedError::bad_request(
                            "parent objective must belong to the same cycle",
                        ));
                    }
                    if parent_objective.id == objective.id {
                        return Err(UnifiedError::bad_request(
                            "an objective cannot be its own parent",
                        ));
                    }
                    Some(parent_objective.id)
                }
                None => None,
            };
        }
        objective.updated_at = Utc::now();
        state.objectives.insert(objective.id, objective.clone());
        state.emit(
            "okrs.objective.updated",
            json!({
                "objective_id": objective.id.to_string(),
                "cycle_id": objective.cycle_id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(objective_view(&state, &objective))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_objective(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let objective = load_objective(&state, &path.into_inner())?;
        // Children lose their parent instead of being deleted: the
        // alignment tree keeps the rest of the objectives intact.
        for mut child in state
            .objectives
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|child| child.parent_objective_id == Some(objective.id))
        {
            child.parent_objective_id = None;
            child.updated_at = Utc::now();
            state.objectives.insert(child.id, child);
        }
        for key_result in key_results_of(&state, objective.id) {
            delete_key_result_inner(&state, key_result.id);
        }
        state.objectives.remove(&objective.id);
        state.emit(
            "okrs.objective.deleted",
            json!({
                "objective_id": objective.id.to_string(),
                "cycle_id": objective.cycle_id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(json!({ "deleted": objective.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_alignment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_VIEW)?;
        let objective = load_objective(&state, &path.into_inner())?;
        // Parent chain, root first, guarding against a cycle in
        // the parent pointers.
        let mut parents: Vec<ObjectiveView> = Vec::new();
        let mut seen: Vec<Uuid> = vec![objective.id];
        let mut current = objective.parent_objective_id;
        while let Some(parent_id) = current {
            if seen.contains(&parent_id) {
                break;
            }
            seen.push(parent_id);
            let Some(parent) = state
                .objectives
                .get(&parent_id)
                .map(|entry| entry.value().clone())
            else {
                break;
            };
            parents.push(objective_view(&state, &parent));
            current = parent.parent_objective_id;
        }
        parents.reverse();
        let children: Vec<ObjectiveView> = state
            .objectives
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|child| child.parent_objective_id == Some(objective.id))
            .map(|child| objective_view(&state, &child))
            .collect();
        let mut linked_projects: Vec<String> = Vec::new();
        let mut linked_tasks: Vec<String> = Vec::new();
        for key_result in key_results_of(&state, objective.id) {
            for link in links_of(&state, key_result.id) {
                match link.link_type.as_str() {
                    "project" => {
                        let id = link.target_id.to_string();
                        if !linked_projects.contains(&id) {
                            linked_projects.push(id);
                        }
                    }
                    "task" => {
                        let id = link.target_id.to_string();
                        if !linked_tasks.contains(&id) {
                            linked_tasks.push(id);
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(json!({
            "objective": objective_view(&state, &objective),
            "parents": parents,
            "children": children,
            "linked_projects": linked_projects,
            "linked_tasks": linked_tasks,
            "note": "Project links are alignment only: the numeric roll-up counts linked tasks (see the module header).",
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Key-result handlers
// ---------------------------------------------------------------------------

/// Remove a key result with its check-ins and links (shared by the
/// objective delete and the key-result delete).
fn delete_key_result_inner(state: &OkrState, key_result_id: Uuid) {
    state
        .checkins
        .retain(|_, checkin| checkin.key_result_id != key_result_id);
    state
        .links
        .retain(|_, link| link.key_result_id != key_result_id);
    state.key_results.remove(&key_result_id);
}

async fn list_key_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_VIEW)?;
        let objective = load_objective(&state, &path.into_inner())?;
        let params = query.into_inner();
        let items: Vec<KeyResultView> = key_results_of(&state, objective.id)
            .iter()
            .map(|key_result| key_result_view(&state, key_result))
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_key_result(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateKeyResultRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<KeyResultView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let objective = load_objective(&state, &path.into_inner())?;
        let cycle = load_cycle(&state, &objective.cycle_id.to_string())?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict(
                "Cannot add a key result to an objective of a closed cycle",
            ));
        }
        let title = require_text(&body.title, "title", MAX_TITLE_LEN)?;
        let unit = require_text(&body.unit, "unit", MAX_UNIT_LEN)?;
        let target_value = valid_number(body.target_value, "target_value")?;
        let start_value = valid_number(body.start_value.unwrap_or(0.0), "start_value")?;
        if target_value == start_value {
            return Err(UnifiedError::bad_request(
                "target_value must differ from start_value",
            ));
        }
        let now = Utc::now();
        let key_result = OkrKeyResult {
            id: Uuid::new_v4(),
            objective_id: objective.id,
            title,
            unit,
            target_value,
            start_value,
            current_value: start_value,
            created_at: now,
            updated_at: now,
        };
        state.key_results.insert(key_result.id, key_result.clone());
        state.emit(
            "okrs.key_result.created",
            json!({
                "key_result_id": key_result.id.to_string(),
                "objective_id": objective.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(key_result_view(&state, &key_result))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_key_result(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<KeyResultView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_VIEW)?;
        let key_result = load_key_result(&state, &path.into_inner())?;
        Ok(key_result_view(&state, &key_result))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_key_result(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateKeyResultRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<KeyResultView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let mut key_result = load_key_result(&state, &path.into_inner())?;
        let objective = load_objective(&state, &key_result.objective_id.to_string())?;
        let cycle = load_cycle(&state, &objective.cycle_id.to_string())?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict(
                "Cannot update a key result of a closed cycle",
            ));
        }
        if let Some(title) = &body.title {
            key_result.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(unit) = &body.unit {
            key_result.unit = require_text(unit, "unit", MAX_UNIT_LEN)?;
        }
        if let Some(target_value) = body.target_value {
            key_result.target_value = valid_number(target_value, "target_value")?;
        }
        if let Some(start_value) = body.start_value {
            key_result.start_value = valid_number(start_value, "start_value")?;
        }
        if key_result.target_value == key_result.start_value {
            return Err(UnifiedError::bad_request(
                "target_value must differ from start_value",
            ));
        }
        // The current value is never hand-typed here: it moves only
        // through linked-task roll-up or the override endpoint.
        key_result.updated_at = Utc::now();
        state.key_results.insert(key_result.id, key_result.clone());
        state.emit(
            "okrs.key_result.updated",
            json!({
                "key_result_id": key_result.id.to_string(),
                "objective_id": key_result.objective_id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(key_result_view(&state, &key_result))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_key_result(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let key_result = load_key_result(&state, &path.into_inner())?;
        delete_key_result_inner(&state, key_result.id);
        state.emit(
            "okrs.key_result.deleted",
            json!({
                "key_result_id": key_result.id.to_string(),
                "objective_id": key_result.objective_id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(json!({ "deleted": key_result.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Check-in + override handlers
// ---------------------------------------------------------------------------

async fn list_checkins(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_VIEW)?;
        let key_result = load_key_result(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<OkrCheckin> = state
            .checkins
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|checkin| checkin.key_result_id == key_result.id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_checkin(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateCheckinRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OkrCheckin> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let key_result = load_key_result(&state, &path.into_inner())?;
        let objective = load_objective(&state, &key_result.objective_id.to_string())?;
        let cycle = load_cycle(&state, &objective.cycle_id.to_string())?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict(
                "Cannot check in on a key result of a closed cycle",
            ));
        }
        let note = require_text(&body.note, "note", MAX_NOTE_LEN)?;
        let confidence = body.confidence.trim().to_lowercase();
        if !ALLOWED_CONFIDENCE.contains(&confidence.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "confidence must be one of: {}",
                ALLOWED_CONFIDENCE.join(", ")
            )));
        }
        let checkin = OkrCheckin {
            id: Uuid::new_v4(),
            key_result_id: key_result.id,
            author_id: caller.actor.clone(),
            note,
            confidence: confidence.clone(),
            kind: "note".to_string(),
            reason: None,
            value_snapshot: Some(key_result.current_value),
            created_at: Utc::now(),
        };
        state.checkins.insert(checkin.id, checkin.clone());
        state.emit(
            "okrs.kr.checkin",
            json!({
                "key_result_id": key_result.id.to_string(),
                "objective_id": key_result.objective_id.to_string(),
                "checkin_id": checkin.id.to_string(),
                "confidence": confidence,
                "value_snapshot": key_result.current_value,
                "actor_id": caller.actor,
            }),
        );
        Ok(checkin)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn override_key_result(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<OverrideRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<KeyResultView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let mut key_result = load_key_result(&state, &path.into_inner())?;
        let objective = load_objective(&state, &key_result.objective_id.to_string())?;
        let cycle = load_cycle(&state, &objective.cycle_id.to_string())?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict(
                "Cannot override a key result of a closed cycle",
            ));
        }
        let value = valid_number(body.value, "value")?;
        let reason = require_text(&body.reason, "reason", MAX_REASON_LEN)?;
        let old_value = key_result.current_value;
        key_result.current_value = value;
        key_result.updated_at = Utc::now();
        state.key_results.insert(key_result.id, key_result.clone());
        // The override is stored as a check-in row so the manual
        // change always has a reason on record.
        let checkin = OkrCheckin {
            id: Uuid::new_v4(),
            key_result_id: key_result.id,
            author_id: caller.actor.clone(),
            note: format!("Manual override: {old_value} -> {value} ({reason})"),
            confidence: "medium".to_string(),
            kind: "override".to_string(),
            reason: Some(reason.clone()),
            value_snapshot: Some(value),
            created_at: Utc::now(),
        };
        state.checkins.insert(checkin.id, checkin.clone());
        emit_audit(
            &state,
            "okrs.kr.override",
            &caller.actor,
            &format!("key_result:{}", key_result.id),
            json!({
                "id": key_result.id.to_string(),
                "key_result_id": key_result.id.to_string(),
                "objective_id": key_result.objective_id.to_string(),
                "old_value": old_value,
                "new_value": value,
                "reason": reason,
            }),
        );
        Ok(key_result_view(&state, &key_result))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Link handlers
// ---------------------------------------------------------------------------

async fn list_links(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_VIEW)?;
        let key_result = load_key_result(&state, &path.into_inner())?;
        let params = query.into_inner();
        let items = links_of(&state, key_result.id);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateLinkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<OkrLink> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let key_result = load_key_result(&state, &path.into_inner())?;
        let objective = load_objective(&state, &key_result.objective_id.to_string())?;
        let cycle = load_cycle(&state, &objective.cycle_id.to_string())?;
        if cycle.status == CycleStatus::Closed {
            return Err(UnifiedError::conflict(
                "Cannot link work to a key result of a closed cycle",
            ));
        }
        let link_type = body.link_type.trim().to_lowercase();
        if !ALLOWED_LINK_TYPES.contains(&link_type.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "link_type must be one of: {}",
                ALLOWED_LINK_TYPES.join(", ")
            )));
        }
        let target_id = parse_uuid(&body.target_id, "target")?;
        let duplicate = state.links.iter().any(|entry| {
            let link = entry.value();
            link.key_result_id == key_result.id
                && link.link_type == link_type
                && link.target_id == target_id
        });
        if duplicate {
            return Err(UnifiedError::conflict(format!(
                "Key result is already linked to {link_type} {target_id}"
            )));
        }
        let link = OkrLink {
            id: Uuid::new_v4(),
            key_result_id: key_result.id,
            link_type: link_type.clone(),
            target_id,
            created_at: Utc::now(),
        };
        state.links.insert(link.id, link.clone());
        // A link created after its task already completed picks the
        // completion up immediately.
        refresh_rollup(&state, key_result.id);
        state.emit(
            "okrs.link.created",
            json!({
                "link_id": link.id.to_string(),
                "key_result_id": key_result.id.to_string(),
                "link_type": link_type,
                "target_id": target_id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(link)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<OkrState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_OKRS_MANAGE)?;
        let (key_result_id, link_id) = path.into_inner();
        let key_result = load_key_result(&state, &key_result_id)?;
        let link = load_link(&state, &link_id)?;
        if link.key_result_id != key_result.id {
            return Err(UnifiedError::not_found(format!("Link not found: {link_id}")));
        }
        state.links.remove(&link.id);
        refresh_rollup(&state, key_result.id);
        state.emit(
            "okrs.link.deleted",
            json!({
                "link_id": link.id.to_string(),
                "key_result_id": key_result.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(json!({ "deleted": link.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
