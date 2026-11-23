// OKR / Goals — Public record, request, and view types.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_cycles::*;
use super::handlers_objectives::*;
use super::handlers_key_results::*;
use super::hooks::*;
use super::module::*;

/// Cycle lifecycle: `draft` -> `active` -> `closed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CycleStatus {
    Draft,
    Active,
    Closed,
}

impl CycleStatus {
    pub(crate) fn as_str(&self) -> &'static str {
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
