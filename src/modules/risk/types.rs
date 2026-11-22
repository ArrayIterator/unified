// Risk Mitigation module.
//
// A risk register with mitigation plans, compliance checklists,
// and threshold alerts. Every number the module shows is derived
// from data it stores, by a formula written down here:
//
//   - Score: `score = likelihood * impact`, both integers 1-5, so
//     the score is always an integer in 1..=25. It is computed
//     server-side on every create and update; a client-supplied
//     score is never accepted.
//   - Band, derived from the score with fixed thresholds:
//     1-4 Low, 5-9 Medium, 10-16 High, 17-25 Critical.
//   - Alert threshold: a risk alerts when `score >=
//     alert_threshold`. The threshold defaults to 15, inside the
//     High band, so the default alerts on High risks scoring 15 or
//     more and on all Critical risks. It can be set per risk in
//     1..=25. Crossing the threshold emits
//     `risk.threshold_crossed` and a `notification.requested` to
//     the risk owner, and records a `RiskAlert` (see
//     `risk_alerts`; the alerts list is the alerts history).
//   - Review dates: a risk carries `next_review_at`. The review
//     check (`POST /api/v1/risk/review-check` and the
//     `risk.review_check_requested` hook) alerts once per review
//     date when it passes without the risk being closed or
//     accepted, and records a `review_overdue` alert. The HTTP
//     endpoint is the only trigger wired today: the hook listener
//     is registered, but no emitter exists in the tree (no
//     scheduler job and no other module emits the event), so a
//     scheduled review check is pending scheduler work and is
//     out of scope for this module.
//
// Mitigation plans and tasks: a plan groups tasks. Each task is
// requested from the Projects module through the
// `risk.mitigation_task_requested` event:
//
//   {
//     "risk_id": "...", "plan_id": "...", "task_id": "...",
//     "project_id": "..." | null, "title": "...",
//     "description": "...", "assignee_id": "..." | null,
//     "due_date": "RFC3339" | null
//   }
//
// The Projects module is expected to answer with a result carrying
// `project_task_id`. CONVERGENCE POINT: the projects module is
// built in parallel and, at the time of writing, registers no
// handler for this event (it answers `project.task_from_ticket_
// requested`, `project.ticket_from_task_requested`, and
// `project.billable_hours_requested` only). Until the integrator
// wires the handler, a task stays in status `requested` with
// `awaiting_projects_handler: true` and `external_project_task_id:
// null`; the request itself is still emitted and recorded, and the
// task can be tracked and completed locally. Nothing here invents
// a project task id.
//
// KNOWN LIMITATION (blocked, needs a projects-module fix builder):
// the missing `risk.mitigation_task_requested` handler lives in
// the Projects module, which this module must not modify, so the
// task lifecycle above is the honest behaviour until that handler
// is wired. Do not present a task as backed by a project task
// while `awaiting_projects_handler` is true.
//
// Compliance checklists: a template holds ordered items (each
// `required` or optional); an instance is a per-company run of a
// template, optionally linked to a risk. Completing an item stores
// evidence notes, who completed it, and when. When every required
// item of an instance is complete, the instance completes and
// emits `risk.checklist_completed`.
//
// Events emitted by this module:
//   - `risk.risk_created`, `risk.risk_updated`,
//     `risk.risk_deleted`, `risk.risk_status_changed`
//   - `risk.threshold_crossed` — a risk reached or passed its
//     alert threshold (also sent as `notification.requested`).
//   - `risk.review_overdue` — a risk's review date passed
//     (also sent as `notification.requested`).
//   - `risk.mitigation_task_requested` — a mitigation task was
//     requested from the Projects module.
//   - `risk.checklist_completed` — a checklist instance completed.
//   The audit module records these once the integrator adds them
//   to its audited list (see docs/integration/INTEGRATION_NOTES_BATCH3_B4.md);
//   this module itself emits them on every change.
//
// Interconnection happens only through the hook bus — no module
// calls another module directly. Money is NUMERIC(19,4) in SQL
// (see the migration); this module stores no money, but the
// migration keeps the convention for any future cost fields.

// risk — types.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::store::*;
use super::common::*;
use super::handlers_risks::*;
use super::handlers_plans::*;
use super::handlers_templates::*;
use super::handlers_instances::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Score bands (the single source of truth, also served by /matrix)
// ---------------------------------------------------------------------------

/// Score band thresholds: Low 1-4, Medium 5-9, High 10-16,
/// Critical 17-25. The boundaries are inclusive and cover 1..=25
/// with no gaps.
pub const BAND_LOW_MAX: u8 = 4;

pub const BAND_MEDIUM_MAX: u8 = 9;

pub const BAND_HIGH_MAX: u8 = 16;

/// Default alert threshold: 15, inside the High band, so High
/// risks scoring 15+ and all Critical risks alert by default.
pub const DEFAULT_ALERT_THRESHOLD: u8 = 15;

/// Band name for a score in 1..=25. Callers validate the inputs
/// (likelihood and impact are each 1..=5, so the score is always
/// 1..=25); out-of-domain scores are a programming error and are
/// caught by the debug assertion below.
pub fn band_for(score: u8) -> &'static str {
    debug_assert!(
        (1..=25).contains(&score),
        "band_for: score must be in 1..=25, got {score}"
    );
    if score <= BAND_LOW_MAX {
        "low"
    } else if score <= BAND_MEDIUM_MAX {
        "medium"
    } else if score <= BAND_HIGH_MAX {
        "high"
    } else {
        "critical"
    }
}

/// Score for a likelihood/impact pair. Both inputs are validated to
/// 1..=5 before this is called, so the result is always 1..=25.
/// Out-of-domain inputs are a programming error (debug-asserted).
pub fn score_for(likelihood: u8, impact: u8) -> u8 {
    debug_assert!(
        (1..=5).contains(&likelihood) && (1..=5).contains(&impact),
        "score_for: likelihood and impact must each be in 1..=5, got {likelihood} x {impact}"
    );
    likelihood * impact
}

pub const RISK_CATEGORIES: &[&str] = &[
    "operational",
    "financial",
    "security",
    "compliance",
    "strategic",
    "reputational",
    "technical",
];

pub const RISK_STATUSES: &[&str] = &[
    "identified",
    "assessed",
    "mitigating",
    "monitored",
    "closed",
    "accepted",
];

pub const PLAN_STATUSES: &[&str] = &["draft", "active", "completed", "cancelled"];

pub const TASK_STATUSES: &[&str] = &[
    "requested",
    "in_progress",
    "done",
    "cancelled",
];

/// Allowed status transitions. A move to the same status is a
/// no-op and always allowed; every other move not listed here is
/// rejected with 422 by the update handlers.
///
///   - Risk: `identified -> assessed -> mitigating -> monitored ->
///     closed | accepted`, with `closed | accepted -> identified`
///     only as an explicit reopen.
///   - Plan: `draft -> active -> completed`, `draft | active ->
///     cancelled`. Completed and cancelled plans are terminal.
///   - Task: `requested -> in_progress -> done`, `requested |
///     in_progress -> cancelled`. Done and cancelled tasks are
///     terminal.
pub(crate) fn valid_risk_transition(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    matches!(
        (from, to),
        ("identified", "assessed")
            | ("assessed", "mitigating")
            | ("mitigating", "monitored")
            | ("monitored", "closed")
            | ("monitored", "accepted")
            | ("closed", "identified")
            | ("accepted", "identified")
    )
}

pub(crate) fn valid_plan_transition(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    matches!(
        (from, to),
        ("draft", "active")
            | ("draft", "cancelled")
            | ("active", "completed")
            | ("active", "cancelled")
    )
}

pub(crate) fn valid_task_transition(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    matches!(
        (from, to),
        ("requested", "in_progress")
            | ("requested", "cancelled")
            | ("in_progress", "done")
            | ("in_progress", "cancelled")
    )
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Risk {
    pub id: String,
    pub company_id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub category: String,
    pub likelihood: u8,
    pub impact: u8,
    /// Server-computed: likelihood * impact (1..=25).
    pub score: u8,
    /// Derived from the score: low / medium / high / critical.
    pub band: String,
    pub owner_id: String,
    pub status: String,
    pub next_review_at: Option<DateTime<Utc>>,
    pub alert_threshold: u8,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRisk {
    pub company_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub category: String,
    pub likelihood: u8,
    pub impact: u8,
    pub owner_id: String,
    pub status: Option<String>,
    pub next_review_at: Option<DateTime<Utc>>,
    pub alert_threshold: Option<u8>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateRisk {
    pub title: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub likelihood: Option<u8>,
    pub impact: Option<u8>,
    pub owner_id: Option<String>,
    pub status: Option<String>,
    pub next_review_at: Option<DateTime<Utc>>,
    pub alert_threshold: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MitigationPlan {
    pub id: String,
    pub risk_id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub target_date: Option<DateTime<Utc>>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateMitigationPlan {
    pub title: String,
    pub description: Option<String>,
    pub target_date: Option<DateTime<Utc>>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateMitigationPlan {
    pub title: Option<String>,
    pub description: Option<String>,
    pub target_date: Option<DateTime<Utc>>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MitigationTask {
    pub id: String,
    pub plan_id: String,
    pub risk_id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub assignee_id: Option<String>,
    pub status: String,
    pub due_date: Option<DateTime<Utc>>,
    /// Id of the task in the Projects module, once it answers
    /// `risk.mitigation_task_requested`. Null until then.
    pub external_project_task_id: Option<String>,
    /// True while no Projects handler has answered the request.
    pub awaiting_projects_handler: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateMitigationTask {
    pub title: String,
    pub description: Option<String>,
    pub assignee_id: Option<String>,
    pub due_date: Option<DateTime<Utc>>,
    /// Projects-module project to create the task in. Optional:
    /// without it the request still goes out and the Projects
    /// module decides (or declines) from the payload.
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateMitigationTask {
    pub title: Option<String>,
    pub description: Option<String>,
    pub assignee_id: Option<String>,
    pub status: Option<String>,
    pub due_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecklistTemplate {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub category: String,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateChecklistTemplate {
    pub title: String,
    pub description: Option<String>,
    pub category: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateChecklistTemplate {
    pub title: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecklistTemplateItem {
    pub id: String,
    pub template_id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub required: bool,
    pub sort_order: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateChecklistTemplateItem {
    pub title: String,
    pub description: Option<String>,
    pub required: Option<bool>,
    pub sort_order: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecklistInstance {
    pub id: String,
    pub template_id: String,
    pub company_id: Option<String>,
    pub risk_id: Option<String>,
    pub title: String,
    pub status: String,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateChecklistInstance {
    pub template_id: String,
    pub company_id: Option<String>,
    pub risk_id: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecklistInstanceItem {
    pub id: String,
    pub instance_id: String,
    pub template_item_id: String,
    pub title: String,
    pub required: bool,
    pub sort_order: u32,
    pub completed: bool,
    #[serde(default)]
    pub evidence: String,
    pub completed_by: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompleteChecklistItem {
    pub evidence: Option<String>,
}

/// One recorded alert: the alerts history. `figures` carries the
/// score and review date the alert was raised for, so an alert is
/// never shown without the data behind it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskAlert {
    pub id: String,
    pub risk_id: String,
    pub company_id: Option<String>,
    pub kind: String,
    pub message: String,
    pub score: Option<u8>,
    pub notified_user_id: Option<String>,
    pub figures: Value,
    pub created_at: DateTime<Utc>,
}

/// Legacy query shape for actor identity (`?viewer_id=` /
/// `?viewer_role=`). It is retained only so existing query strings
/// still deserialize; it is no longer read for identity. Actor
/// identity now comes from the `X-User-Id` and `X-Role` request
/// headers (see `actor_of` / `viewer_of` in `common.rs`): query
/// parameters are logged, cached, and bookmarked, so they must
/// never carry role identity.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ActorQuery {
    pub viewer_id: Option<String>,
    pub viewer_role: Option<String>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct RiskState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) risks: DashMap<String, Risk>,
    pub(crate) plans: DashMap<String, MitigationPlan>,
    pub(crate) tasks: DashMap<String, MitigationTask>,
    pub(crate) templates: DashMap<String, ChecklistTemplate>,
    pub(crate) template_items: DashMap<String, ChecklistTemplateItem>,
    pub(crate) instances: DashMap<String, ChecklistInstance>,
    pub(crate) instance_items: DashMap<String, ChecklistInstanceItem>,
    pub(crate) alerts: DashMap<String, RiskAlert>,
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_cover_every_score_from_one_to_twenty_five() {
        // Scores below 1 and above 25 cannot occur: callers validate
        // likelihood/impact to 1..=5 and `band_for` debug-asserts
        // the 1..=25 precondition, so no out-of-domain assertion
        // belongs here.
        assert_eq!(band_for(1), "low");
        assert_eq!(band_for(4), "low");
        assert_eq!(band_for(5), "medium");
        assert_eq!(band_for(9), "medium");
        assert_eq!(band_for(10), "high");
        assert_eq!(band_for(16), "high");
        assert_eq!(band_for(17), "critical");
        assert_eq!(band_for(25), "critical");
    }

    #[test]
    fn scores_multiply_likelihood_by_impact() {
        assert_eq!(score_for(1, 1), 1);
        assert_eq!(score_for(5, 5), 25);
        assert_eq!(score_for(3, 4), 12);
        assert_eq!(score_for(1, 5), 5);
    }

    #[test]
    fn risk_transitions_follow_the_lifecycle() {
        assert!(valid_risk_transition("identified", "assessed"));
        assert!(valid_risk_transition("assessed", "mitigating"));
        assert!(valid_risk_transition("mitigating", "monitored"));
        assert!(valid_risk_transition("monitored", "closed"));
        assert!(valid_risk_transition("monitored", "accepted"));
        assert!(valid_risk_transition("closed", "identified"));
        assert!(valid_risk_transition("accepted", "identified"));
        assert!(valid_risk_transition("identified", "identified"));
        assert!(!valid_risk_transition("identified", "mitigating"));
        assert!(!valid_risk_transition("identified", "monitored"));
        assert!(!valid_risk_transition("closed", "assessed"));
        assert!(!valid_risk_transition("closed", "monitored"));
        assert!(!valid_risk_transition("monitored", "identified"));
        assert!(!valid_risk_transition("assessed", "closed"));
    }

    #[test]
    fn plan_transitions_end_in_terminal_states() {
        assert!(valid_plan_transition("draft", "active"));
        assert!(valid_plan_transition("draft", "cancelled"));
        assert!(valid_plan_transition("active", "completed"));
        assert!(valid_plan_transition("active", "cancelled"));
        assert!(valid_plan_transition("completed", "completed"));
        assert!(!valid_plan_transition("completed", "draft"));
        assert!(!valid_plan_transition("completed", "active"));
        assert!(!valid_plan_transition("cancelled", "active"));
        assert!(!valid_plan_transition("active", "draft"));
    }

    #[test]
    fn task_transitions_end_in_terminal_states() {
        assert!(valid_task_transition("requested", "in_progress"));
        assert!(valid_task_transition("requested", "cancelled"));
        assert!(valid_task_transition("in_progress", "done"));
        assert!(valid_task_transition("in_progress", "cancelled"));
        assert!(valid_task_transition("done", "done"));
        assert!(!valid_task_transition("done", "requested"));
        assert!(!valid_task_transition("done", "in_progress"));
        assert!(!valid_task_transition("cancelled", "in_progress"));
        assert!(!valid_task_transition("requested", "done"));
    }
}
