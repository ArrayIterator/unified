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
//     accepted, and records a `review_overdue` alert.
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
//   to its audited list (see INTEGRATION_NOTES_BATCH3_B4.md);
//   this module itself emits them on every change.
//
// Interconnection happens only through the hook bus — no module
// calls another module directly. Money is NUMERIC(19,4) in SQL
// (see the migration); this module stores no money, but the
// migration keeps the convention for any future cost fields.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

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

/// Band name for a score in 1..=25.
pub fn band_for(score: u8) -> &'static str {
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
pub fn score_for(likelihood: u8, impact: u8) -> u8 {
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

/// Actor identity from query parameters, following the projects
/// module's `viewer_of` convention.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ActorQuery {
    pub viewer_id: Option<String>,
    pub viewer_role: Option<String>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct RiskState {
    hooks: RwLock<Arc<HookBus>>,
    risks: DashMap<String, Risk>,
    plans: DashMap<String, MitigationPlan>,
    tasks: DashMap<String, MitigationTask>,
    templates: DashMap<String, ChecklistTemplate>,
    template_items: DashMap<String, ChecklistTemplateItem>,
    instances: DashMap<String, ChecklistInstance>,
    instance_items: DashMap<String, ChecklistInstanceItem>,
    alerts: DashMap<String, RiskAlert>,
}

impl RiskState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            risks: DashMap::new(),
            plans: DashMap::new(),
            tasks: DashMap::new(),
            templates: DashMap::new(),
            template_items: DashMap::new(),
            instances: DashMap::new(),
            instance_items: DashMap::new(),
            alerts: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    fn emit_with_results(&self, event: &str, data: Value) -> HookContext {
        self.bus().emit(event, data)
    }

    /// Ask the Roles module whether `role` holds `permission`
    /// (`role.check`). Fails closed: no answer means not allowed.
    fn check_capability(&self, role: &str, permission: &str) -> bool {
        let context = self.bus().emit(
            "role.check",
            json!({ "role": role, "permission": permission, "allowed": false }),
        );
        context
            .get("allowed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Send an alert through the Notifications module.
    fn notify(&self, user_id: &str, title: String, body: String) {
        if user_id.trim().is_empty() {
            return;
        }
        self.emit(
            "notification.requested",
            json!({
                "user_id": user_id.trim(),
                "title": title,
                "body": body,
                "kind": "risk",
            }),
        );
    }

    /// Record an alert and notify the owner, unless an identical
    /// alert (same risk, kind, and figures) already exists — the
    /// threshold and review checks run repeatedly and must not spam
    /// the owner with the same alert.
    fn record_alert(
        &self,
        risk: &Risk,
        kind: &str,
        message: String,
        figures: Value,
    ) -> Option<RiskAlert> {
        let duplicate = self.alerts.iter().any(|entry| {
            let alert = entry.value();
            alert.risk_id == risk.id && alert.kind == kind && alert.figures == figures
        });
        if duplicate {
            return None;
        }
        let alert = RiskAlert {
            id: new_id(),
            risk_id: risk.id.clone(),
            company_id: risk.company_id.clone(),
            kind: kind.to_string(),
            message: message.clone(),
            score: Some(risk.score),
            notified_user_id: Some(risk.owner_id.clone()),
            figures,
            created_at: Utc::now(),
        };
        self.alerts.insert(alert.id.clone(), alert.clone());
        alert
    }

    /// Check whether a risk crosses its alert threshold and, when it
    /// does, emit `risk.threshold_crossed`, notify the owner, and
    /// record the alert. Returns true when an alert was raised.
    /// The threshold rule is: alert when score >= alert_threshold.
    fn check_threshold(&self, risk: &Risk) -> bool {
        if risk.score < risk.alert_threshold {
            return false;
        }
        let figures = json!({
            "likelihood": risk.likelihood,
            "impact": risk.impact,
            "score": risk.score,
            "band": risk.band,
            "alert_threshold": risk.alert_threshold,
        });
        let message = format!(
            "Risk \"{}\" scored {} ({} band, likelihood {} x impact {}) and reached its alert threshold of {}.",
            risk.title, risk.score, risk.band, risk.likelihood, risk.impact, risk.alert_threshold
        );
        let raised = self
            .record_alert(risk, "threshold_crossed", message.clone(), figures)
            .is_some();
        if raised {
            self.emit(
                "risk.threshold_crossed",
                json!({
                    "risk_id": risk.id,
                    "company_id": risk.company_id,
                    "title": risk.title,
                    "likelihood": risk.likelihood,
                    "impact": risk.impact,
                    "score": risk.score,
                    "band": risk.band,
                    "alert_threshold": risk.alert_threshold,
                    "owner_id": risk.owner_id,
                }),
            );
            self.notify(
                &risk.owner_id,
                format!("Risk threshold crossed: {}", risk.title),
                message,
            );
        }
        raised
    }

    /// Ask the Projects module to create the task for a mitigation
    /// task, through `risk.mitigation_task_requested`. Returns the
    /// project task id when a handler answers with one; None means
    /// no handler answered (the projects module has not wired this
    /// event yet — see the module header convergence note).
    fn request_project_task(&self, task: &MitigationTask, project_id: Option<&str>) -> Option<String> {
        let context = self.emit_with_results(
            "risk.mitigation_task_requested",
            json!({
                "risk_id": task.risk_id,
                "plan_id": task.plan_id,
                "task_id": task.id,
                "project_id": project_id,
                "title": task.title,
                "description": task.description,
                "assignee_id": task.assignee_id,
                "due_date": task.due_date,
            }),
        );
        for result in &context.results {
            if let Some(task_id) = result.get("project_task_id").and_then(|v| v.as_str()) {
                if !task_id.trim().is_empty() {
                    return Some(task_id.to_string());
                }
            }
        }
        None
    }

    /// Scan risks whose review date has passed without the risk
    /// being closed or accepted. One `review_overdue` alert per
    /// risk per review date, plus a notification to the owner.
    /// Returns the number of alerts raised.
    fn run_review_check(&self, company_id: Option<&str>) -> u32 {
        let now = Utc::now();
        let mut raised: u32 = 0;
        let risks: Vec<Risk> = self
            .risks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|risk| match company_id {
                Some(company) => risk.company_id.as_deref() == Some(company),
                None => true,
            })
            .collect();
        for risk in risks {
            if matches!(risk.status.as_str(), "closed" | "accepted") {
                continue;
            }
            let Some(review_at) = risk.next_review_at else {
                continue;
            };
            if review_at >= now {
                continue;
            }
            let figures = json!({
                "review_date": review_at,
                "score": risk.score,
                "band": risk.band,
            });
            let days = (now - review_at).num_days();
            let message = format!(
                "Risk \"{}\" is {} day(s) past its review date ({}).",
                risk.title,
                days,
                review_at.to_rfc3339()
            );
            if self
                .record_alert(&risk, "review_overdue", message.clone(), figures)
                .is_some()
            {
                raised += 1;
                self.emit(
                    "risk.review_overdue",
                    json!({
                        "risk_id": risk.id,
                        "company_id": risk.company_id,
                        "title": risk.title,
                        "review_date": review_at,
                        "days_overdue": days,
                        "score": risk.score,
                        "band": risk.band,
                        "owner_id": risk.owner_id,
                    }),
                );
                self.notify(
                    &risk.owner_id,
                    format!("Risk review overdue: {}", risk.title),
                    message,
                );
            }
        }
        raised
    }

    fn get_risk(&self, id: &str) -> UnifiedResult<Risk> {
        self.risks
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Risk not found"))
    }

    fn get_plan(&self, id: &str) -> UnifiedResult<MitigationPlan> {
        self.plans
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Mitigation plan not found"))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
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

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

fn viewer_of(params: &PaginationParams) -> (Option<String>, Option<String>) {
    let viewer_id = params
        .filters
        .get("viewer_id")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let viewer_role = params
        .filters
        .get("viewer_role")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    (viewer_id, viewer_role)
}

fn require_capability(
    state: &RiskState,
    role: Option<&str>,
    permission: &str,
) -> UnifiedResult<()> {
    match role {
        Some(role) if state.check_capability(role, permission) => Ok(()),
        _ => Err(UnifiedError::forbidden(format!(
            "Role lacks required capability: {permission}"
        ))),
    }
}

fn actor_of(query: &ActorQuery) -> (Option<String>, Option<String>) {
    (
        query.viewer_id.clone().filter(|value| !value.trim().is_empty()),
        query.viewer_role.clone().filter(|value| !value.trim().is_empty()),
    )
}

fn valid_rating(value: u8) -> bool {
    (1..=5).contains(&value)
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

// ---------------------------------------------------------------------------
// Handlers: risks
// ---------------------------------------------------------------------------

async fn list_risks(
    state: web::Data<Arc<RiskState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (_viewer_id, viewer_role) = viewer_of(&params);
    if let Err(error) = require_capability(&state, viewer_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    let mut items: Vec<Risk> = state
        .risks
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|risk| {
            matches_search(
                &format!("{} {} {}", risk.title, risk.description, risk.category),
                &params,
            ) && params
                .filters
                .get("company_id")
                .is_none_or(|company| risk.company_id.as_deref() == Some(company.as_str()))
                && params
                    .filters
                    .get("category")
                    .is_none_or(|category| risk.category == *category)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| risk.status == *status)
                && params
                    .filters
                    .get("band")
                    .is_none_or(|band| risk.band == *band)
                && params
                    .filters
                    .get("owner_id")
                    .is_none_or(|owner| risk.owner_id == *owner)
                && params
                    .filters
                    .get("min_score")
                    .and_then(|value| value.parse::<u8>().ok())
                    .is_none_or(|min| risk.score >= min)
        })
        .collect();
    items.sort_by(|a, b| b.score.cmp(&a.score).then(b.updated_at.cmp(&a.updated_at)));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_risk(
    state: web::Data<Arc<RiskState>>,
    query: web::Query<ActorQuery>,
    body: web::Json<CreateRisk>,
) -> HttpResponse {
    let (actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:create") {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<Risk> = (|| {
        let input = body.into_inner();
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request("Risk title must not be empty"));
        }
        if !RISK_CATEGORIES.contains(&input.category.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "category must be one of: {}",
                RISK_CATEGORIES.join(", ")
            )));
        }
        if !valid_rating(input.likelihood) || !valid_rating(input.impact) {
            return Err(UnifiedError::bad_request(
                "likelihood and impact must each be between 1 and 5",
            ));
        }
        if input.owner_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("owner_id must not be empty"));
        }
        let status = input.status.unwrap_or_else(|| "identified".to_string());
        if !RISK_STATUSES.contains(&status.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "status must be one of: {}",
                RISK_STATUSES.join(", ")
            )));
        }
        let alert_threshold = input.alert_threshold.unwrap_or(DEFAULT_ALERT_THRESHOLD);
        if !(1..=25).contains(&alert_threshold) {
            return Err(UnifiedError::bad_request(
                "alert_threshold must be between 1 and 25",
            ));
        }
        let score = score_for(input.likelihood, input.impact);
        let risk = Risk {
            id: new_id(),
            company_id: input.company_id.filter(|value| !value.trim().is_empty()),
            title: input.title.trim().to_string(),
            description: input
                .description
                .map(|value| value.trim().to_string())
                .unwrap_or_default(),
            category: input.category,
            likelihood: input.likelihood,
            impact: input.impact,
            score,
            band: band_for(score).to_string(),
            owner_id: input.owner_id.trim().to_string(),
            status,
            next_review_at: input.next_review_at,
            alert_threshold,
            created_by: actor_id,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state.risks.insert(risk.id.clone(), risk.clone());
        state.emit(
            "risk.risk_created",
            serde_json::to_value(&risk).unwrap_or(Value::Null),
        );
        state.check_threshold(&risk);
        Ok(risk)
    })();
    respond_created(result)
}

async fn get_risk(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<ActorQuery>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    json::respond(state.get_risk(path.as_str()), false)
}

async fn update_risk(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<ActorQuery>,
    body: web::Json<UpdateRisk>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:edit") {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<Risk> = (|| {
        let input = body.into_inner();
        if let Some(category) = &input.category {
            if !RISK_CATEGORIES.contains(&category.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "category must be one of: {}",
                    RISK_CATEGORIES.join(", ")
                )));
            }
        }
        if let Some(likelihood) = input.likelihood {
            if !valid_rating(likelihood) {
                return Err(UnifiedError::bad_request(
                    "likelihood must be between 1 and 5",
                ));
            }
        }
        if let Some(impact) = input.impact {
            if !valid_rating(impact) {
                return Err(UnifiedError::bad_request("impact must be between 1 and 5"));
            }
        }
        if let Some(status) = &input.status {
            if !RISK_STATUSES.contains(&status.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "status must be one of: {}",
                    RISK_STATUSES.join(", ")
                )));
            }
        }
        if let Some(threshold) = input.alert_threshold {
            if !(1..=25).contains(&threshold) {
                return Err(UnifiedError::bad_request(
                    "alert_threshold must be between 1 and 25",
                ));
            }
        }
        let mut risk = state
            .risks
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Risk not found"))?;
        let old_status = risk.status.clone();
        if let Some(title) = input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request("Risk title must not be empty"));
            }
            risk.title = title.trim().to_string();
        }
        if let Some(description) = input.description {
            risk.description = description.trim().to_string();
        }
        if let Some(category) = input.category {
            risk.category = category;
        }
        if let Some(likelihood) = input.likelihood {
            risk.likelihood = likelihood;
        }
        if let Some(impact) = input.impact {
            risk.impact = impact;
        }
        // Score and band are always recomputed server-side; a
        // client-supplied score is never accepted.
        risk.score = score_for(risk.likelihood, risk.impact);
        risk.band = band_for(risk.score).to_string();
        if let Some(owner_id) = input.owner_id {
            if owner_id.trim().is_empty() {
                return Err(UnifiedError::bad_request("owner_id must not be empty"));
            }
            risk.owner_id = owner_id.trim().to_string();
        }
        if let Some(status) = input.status {
            risk.status = status;
        }
        if let Some(next_review_at) = input.next_review_at {
            risk.next_review_at = Some(next_review_at);
        }
        if let Some(alert_threshold) = input.alert_threshold {
            risk.alert_threshold = alert_threshold;
        }
        risk.updated_at = Utc::now();
        let value = serde_json::to_value(&*risk).unwrap_or(Value::Null);
        let updated = risk.clone();
        let status_changed = old_status != updated.status;
        drop(risk);
        state.emit("risk.risk_updated", value.clone());
        if status_changed {
            state.emit(
                "risk.risk_status_changed",
                json!({
                    "risk_id": updated.id,
                    "company_id": updated.company_id,
                    "title": updated.title,
                    "old_status": old_status,
                    "new_status": updated.status,
                    "score": updated.score,
                    "band": updated.band,
                    "owner_id": updated.owner_id,
                }),
            );
        }
        state.check_threshold(&updated);
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_risk(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<ActorQuery>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:delete") {
        return json::error_response(&error, false);
    }
    let risk_id = path.as_str().to_string();
    let result: UnifiedResult<Value> = (|| {
        state
            .risks
            .remove(&risk_id)
            .map(|_| json!({ "deleted": true, "id": &risk_id }))
            .ok_or_else(|| UnifiedError::not_found("Risk not found"))
    })();
    if result.is_ok() {
        // Plans, tasks, and alerts belong to the risk; they go
        // with it. Checklist instances linked to the risk keep
        // their records but lose the link.
        let plan_ids: Vec<String> = state
            .plans
            .iter()
            .filter(|entry| entry.value().risk_id == risk_id)
            .map(|entry| entry.key().clone())
            .collect();
        for plan_id in &plan_ids {
            state.plans.remove(plan_id);
        }
        state
            .tasks
            .retain(|_, task| task.risk_id != risk_id);
        state.alerts.retain(|_, alert| alert.risk_id != risk_id);
        for mut entry in state.instances.iter_mut() {
            if entry.value().risk_id.as_deref() == Some(risk_id.as_str()) {
                entry.value_mut().risk_id = None;
            }
        }
        state.emit(
            "risk.risk_deleted",
            json!({ "risk_id": risk_id }),
        );
    }
    json::respond(result, false)
}

/// The 5x5 risk matrix: for every likelihood (1-5) by impact
/// (1-5) cell, the score, the band, how many risks sit in it, and
/// their ids. Band thresholds are returned alongside so a client
/// never has to hard-code them.
async fn risk_matrix(
    state: web::Data<Arc<RiskState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (_viewer_id, viewer_role) = viewer_of(&params);
    if let Err(error) = require_capability(&state, viewer_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    let company_filter = params.filters.get("company_id").cloned();
    let mut cells: Vec<Value> = Vec::new();
    for likelihood in 1..=5u8 {
        for impact in 1..=5u8 {
            let score = score_for(likelihood, impact);
            let risk_ids: Vec<String> = state
                .risks
                .iter()
                .filter(|entry| {
                    let risk = entry.value();
                    risk.likelihood == likelihood
                        && risk.impact == impact
                        && company_filter
                            .as_ref()
                            .is_none_or(|company| risk.company_id.as_deref() == Some(company.as_str()))
                })
                .map(|entry| entry.key().clone())
                .collect();
            cells.push(json!({
                "likelihood": likelihood,
                "impact": impact,
                "score": score,
                "band": band_for(score),
                "count": risk_ids.len(),
                "risk_ids": risk_ids,
            }));
        }
    }
    json::success_response(
        json!({
            "company_id": company_filter,
            "cells": cells,
            "bands": [
                { "band": "low", "min_score": 1, "max_score": BAND_LOW_MAX },
                { "band": "medium", "min_score": BAND_LOW_MAX + 1, "max_score": BAND_MEDIUM_MAX },
                { "band": "high", "min_score": BAND_MEDIUM_MAX + 1, "max_score": BAND_HIGH_MAX },
                { "band": "critical", "min_score": BAND_HIGH_MAX + 1, "max_score": 25 },
            ],
            "default_alert_threshold": DEFAULT_ALERT_THRESHOLD,
        }),
        200,
    )
}

// ---------------------------------------------------------------------------
// Handlers: mitigation plans and tasks
// ---------------------------------------------------------------------------

async fn list_plans_for_risk(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (_viewer_id, viewer_role) = viewer_of(&params);
    if let Err(error) = require_capability(&state, viewer_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    let risk_id = path.as_str().to_string();
    if state.get_risk(&risk_id).is_err() {
        return json::error_response(&UnifiedError::not_found("Risk not found"), false);
    }
    let mut items: Vec<MitigationPlan> = state
        .plans
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|plan| {
            plan.risk_id == risk_id
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| plan.status == *status)
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_plan(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<ActorQuery>,
    body: web::Json<CreateMitigationPlan>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:create") {
        return json::error_response(&error, false);
    }
    let risk_id = path.as_str().to_string();
    let result: UnifiedResult<MitigationPlan> = (|| {
        if state.get_risk(&risk_id).is_err() {
            return Err(UnifiedError::not_found("Risk not found"));
        }
        let input = body.into_inner();
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request("Plan title must not be empty"));
        }
        let status = input.status.unwrap_or_else(|| "draft".to_string());
        if !PLAN_STATUSES.contains(&status.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "status must be one of: {}",
                PLAN_STATUSES.join(", ")
            )));
        }
        let plan = MitigationPlan {
            id: new_id(),
            risk_id,
            title: input.title.trim().to_string(),
            description: input
                .description
                .map(|value| value.trim().to_string())
                .unwrap_or_default(),
            target_date: input.target_date,
            status,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state.plans.insert(plan.id.clone(), plan.clone());
        state.emit(
            "risk.mitigation_plan_created",
            serde_json::to_value(&plan).unwrap_or(Value::Null),
        );
        Ok(plan)
    })();
    respond_created(result)
}

async fn get_plan(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<ActorQuery>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    json::respond(state.get_plan(path.as_str()), false)
}

async fn update_plan(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<ActorQuery>,
    body: web::Json<UpdateMitigationPlan>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:edit") {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<MitigationPlan> = (|| {
        let input = body.into_inner();
        if let Some(status) = &input.status {
            if !PLAN_STATUSES.contains(&status.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "status must be one of: {}",
                    PLAN_STATUSES.join(", ")
                )));
            }
        }
        let mut plan = state
            .plans
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Mitigation plan not found"))?;
        if let Some(title) = input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request("Plan title must not be empty"));
            }
            plan.title = title.trim().to_string();
        }
        if let Some(description) = input.description {
            plan.description = description.trim().to_string();
        }
        if let Some(target_date) = input.target_date {
            plan.target_date = Some(target_date);
        }
        if let Some(status) = input.status {
            plan.status = status;
        }
        plan.updated_at = Utc::now();
        let updated = plan.clone();
        drop(plan);
        state.emit(
            "risk.mitigation_plan_updated",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_plan(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<ActorQuery>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:delete") {
        return json::error_response(&error, false);
    }
    let plan_id = path.as_str().to_string();
    let result: UnifiedResult<Value> = (|| {
        state
            .plans
            .remove(&plan_id)
            .map(|_| json!({ "deleted": true, "id": &plan_id }))
            .ok_or_else(|| UnifiedError::not_found("Mitigation plan not found"))
    })();
    if result.is_ok() {
        state.tasks.retain(|_, task| task.plan_id != plan_id);
        state.emit(
            "risk.mitigation_plan_deleted",
            json!({ "plan_id": plan_id }),
        );
    }
    json::respond(result, false)
}

async fn list_tasks_for_plan(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (_viewer_id, viewer_role) = viewer_of(&params);
    if let Err(error) = require_capability(&state, viewer_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    let plan_id = path.as_str().to_string();
    if state.get_plan(&plan_id).is_err() {
        return json::error_response(&UnifiedError::not_found("Mitigation plan not found"), false);
    }
    let mut items: Vec<MitigationTask> = state
        .tasks
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|task| {
            task.plan_id == plan_id
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| task.status == *status)
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_task(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<ActorQuery>,
    body: web::Json<CreateMitigationTask>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:create") {
        return json::error_response(&error, false);
    }
    let plan_id = path.as_str().to_string();
    let result: UnifiedResult<MitigationTask> = (|| {
        let plan = state.get_plan(&plan_id)?;
        let input = body.into_inner();
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request("Task title must not be empty"));
        }
        let task = MitigationTask {
            id: new_id(),
            plan_id: plan.id.clone(),
            risk_id: plan.risk_id.clone(),
            title: input.title.trim().to_string(),
            description: input
                .description
                .map(|value| value.trim().to_string())
                .unwrap_or_default(),
            assignee_id: input
                .assignee_id
                .filter(|value| !value.trim().is_empty()),
            status: "requested".to_string(),
            due_date: input.due_date,
            external_project_task_id: None,
            awaiting_projects_handler: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        // Ask the Projects module to create the backing task. When
        // it answers with a project task id, the task is linked;
        // when it does not (no handler wired yet), the task stays
        // `requested` with `awaiting_projects_handler: true`.
        let project_task_id = state.request_project_task(&task, input.project_id.as_deref());
        let mut task = task;
        if let Some(project_task_id) = project_task_id {
            task.external_project_task_id = Some(project_task_id);
            task.awaiting_projects_handler = false;
        }
        state.tasks.insert(task.id.clone(), task.clone());
        Ok(task)
    })();
    respond_created(result)
}

async fn update_task(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<ActorQuery>,
    body: web::Json<UpdateMitigationTask>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:edit") {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<MitigationTask> = (|| {
        let input = body.into_inner();
        if let Some(status) = &input.status {
            if !TASK_STATUSES.contains(&status.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "status must be one of: {}",
                    TASK_STATUSES.join(", ")
                )));
            }
        }
        let mut task = state
            .tasks
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Mitigation task not found"))?;
        if let Some(title) = input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request("Task title must not be empty"));
            }
            task.title = title.trim().to_string();
        }
        if let Some(description) = input.description {
            task.description = description.trim().to_string();
        }
        if let Some(assignee_id) = input.assignee_id {
            task.assignee_id = if assignee_id.trim().is_empty() {
                None
            } else {
                Some(assignee_id.trim().to_string())
            };
        }
        if let Some(status) = input.status {
            task.status = status;
        }
        if let Some(due_date) = input.due_date {
            task.due_date = Some(due_date);
        }
        task.updated_at = Utc::now();
        let updated = task.clone();
        drop(task);
        state.emit(
            "risk.mitigation_task_updated",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_task(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<ActorQuery>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:delete") {
        return json::error_response(&error, false);
    }
    let task_id = path.as_str().to_string();
    let result: UnifiedResult<Value> = (|| {
        state
            .tasks
            .remove(&task_id)
            .map(|_| json!({ "deleted": true, "id": &task_id }))
            .ok_or_else(|| UnifiedError::not_found("Mitigation task not found"))
    })();
    if result.is_ok() {
        state.emit(
            "risk.mitigation_task_deleted",
            json!({ "task_id": task_id }),
        );
    }
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Handlers: review check and alerts history
// ---------------------------------------------------------------------------

async fn review_check(
    state: web::Data<Arc<RiskState>>,
    query: web::Query<ActorQuery>,
    body: Option<web::Json<Value>>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:edit") {
        return json::error_response(&error, false);
    }
    let company_id = body
        .and_then(|body| {
            body.get("company_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .filter(|value| !value.trim().is_empty());
    let raised = state.run_review_check(company_id.as_deref());
    json::success_response(
        json!({
            "company_id": company_id,
            "alerts_raised": raised,
        }),
        200,
    )
}

async fn list_alerts(
    state: web::Data<Arc<RiskState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (_viewer_id, viewer_role) = viewer_of(&params);
    if let Err(error) = require_capability(&state, viewer_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    let mut items: Vec<RiskAlert> = state
        .alerts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|alert| {
            params
                .filters
                .get("risk_id")
                .is_none_or(|risk| alert.risk_id == *risk)
                && params
                    .filters
                    .get("company_id")
                    .is_none_or(|company| alert.company_id.as_deref() == Some(company.as_str()))
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| alert.kind == *kind)
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

// ---------------------------------------------------------------------------
// Handlers: compliance checklists
// ---------------------------------------------------------------------------

async fn list_templates(
    state: web::Data<Arc<RiskState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (_viewer_id, viewer_role) = viewer_of(&params);
    if let Err(error) = require_capability(&state, viewer_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    let mut items: Vec<ChecklistTemplate> = state
        .templates
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|template| {
            matches_search(
                &format!("{} {} {}", template.title, template.description, template.category),
                &params,
            ) && params
                .filters
                .get("category")
                .is_none_or(|category| template.category == *category)
        })
        .collect();
    items.sort_by(|a, b| a.title.cmp(&b.title));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_template(
    state: web::Data<Arc<RiskState>>,
    query: web::Query<ActorQuery>,
    body: web::Json<CreateChecklistTemplate>,
) -> HttpResponse {
    let (actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:create") {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<ChecklistTemplate> = (|| {
        let input = body.into_inner();
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request("Template title must not be empty"));
        }
        if input.category.trim().is_empty() {
            return Err(UnifiedError::bad_request("Template category must not be empty"));
        }
        let template = ChecklistTemplate {
            id: new_id(),
            title: input.title.trim().to_string(),
            description: input
                .description
                .map(|value| value.trim().to_string())
                .unwrap_or_default(),
            category: input.category.trim().to_string(),
            created_by: actor_id,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state.templates.insert(template.id.clone(), template.clone());
        Ok(template)
    })();
    respond_created(result)
}

async fn get_template(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<ActorQuery>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<ChecklistTemplate> = state
        .templates
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Checklist template not found"));
    json::respond(result, false)
}

async fn update_template(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<ActorQuery>,
    body: web::Json<UpdateChecklistTemplate>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:edit") {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<ChecklistTemplate> = (|| {
        let input = body.into_inner();
        let mut template = state
            .templates
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Checklist template not found"))?;
        if let Some(title) = input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request("Template title must not be empty"));
            }
            template.title = title.trim().to_string();
        }
        if let Some(description) = input.description {
            template.description = description.trim().to_string();
        }
        if let Some(category) = input.category {
            if category.trim().is_empty() {
                return Err(UnifiedError::bad_request("Template category must not be empty"));
            }
            template.category = category.trim().to_string();
        }
        template.updated_at = Utc::now();
        Ok(template.clone())
    })();
    json::respond(result, false)
}

async fn delete_template(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<ActorQuery>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:delete") {
        return json::error_response(&error, false);
    }
    let template_id = path.as_str().to_string();
    if state
        .instances
        .iter()
        .any(|entry| entry.value().template_id == template_id)
    {
        return json::error_response(
            &UnifiedError::conflict(
                "Template still has checklist instances; delete or complete them first",
            ),
            false,
        );
    }
    let result: UnifiedResult<Value> = (|| {
        state
            .templates
            .remove(&template_id)
            .map(|_| json!({ "deleted": true, "id": &template_id }))
            .ok_or_else(|| UnifiedError::not_found("Checklist template not found"))
    })();
    if result.is_ok() {
        state
            .template_items
            .retain(|_, item| item.template_id != template_id);
    }
    json::respond(result, false)
}

async fn list_template_items(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (_viewer_id, viewer_role) = viewer_of(&params);
    if let Err(error) = require_capability(&state, viewer_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    let template_id = path.as_str().to_string();
    if !state.templates.contains_key(template_id.as_str()) {
        return json::error_response(
            &UnifiedError::not_found("Checklist template not found"),
            false,
        );
    }
    let mut items: Vec<ChecklistTemplateItem> = state
        .template_items
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| item.template_id == template_id)
        .collect();
    items.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.created_at.cmp(&b.created_at)));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn add_template_item(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<ActorQuery>,
    body: web::Json<CreateChecklistTemplateItem>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:create") {
        return json::error_response(&error, false);
    }
    let template_id = path.as_str().to_string();
    let result: UnifiedResult<ChecklistTemplateItem> = (|| {
        if !state.templates.contains_key(template_id.as_str()) {
            return Err(UnifiedError::not_found("Checklist template not found"));
        }
        let input = body.into_inner();
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request("Item title must not be empty"));
        }
        let item = ChecklistTemplateItem {
            id: new_id(),
            template_id,
            title: input.title.trim().to_string(),
            description: input
                .description
                .map(|value| value.trim().to_string())
                .unwrap_or_default(),
            required: input.required.unwrap_or(true),
            sort_order: input.sort_order.unwrap_or(0),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state
            .template_items
            .insert(item.id.clone(), item.clone());
        Ok(item)
    })();
    respond_created(result)
}

async fn delete_template_item(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<(String, String)>,
    query: web::Query<ActorQuery>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:delete") {
        return json::error_response(&error, false);
    }
    let (template_id, item_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        match state.template_items.get(item_id.as_str()) {
            Some(item) if item.template_id == template_id => {
                state.template_items.remove(item_id.as_str());
                Ok(json!({ "deleted": true, "id": item_id }))
            }
            _ => Err(UnifiedError::not_found("Checklist template item not found")),
        }
    })();
    json::respond(result, false)
}

async fn create_instance(
    state: web::Data<Arc<RiskState>>,
    query: web::Query<ActorQuery>,
    body: web::Json<CreateChecklistInstance>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:create") {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        let template = state
            .templates
            .get(input.template_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist template not found"))?;
        if let Some(risk_id) = &input.risk_id {
            if state.get_risk(risk_id).is_err() {
                return Err(UnifiedError::not_found("Risk not found"));
            }
        }
        let mut items: Vec<ChecklistTemplateItem> = state
            .template_items
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| item.template_id == template.id)
            .collect();
        items.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.created_at.cmp(&b.created_at)));
        if items.is_empty() {
            return Err(UnifiedError::conflict(
                "Template has no items; add items before creating an instance",
            ));
        }
        let instance = ChecklistInstance {
            id: new_id(),
            template_id: template.id.clone(),
            company_id: input.company_id.filter(|value| !value.trim().is_empty()),
            risk_id: input.risk_id.filter(|value| !value.trim().is_empty()),
            title: input
                .title
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| template.title.clone()),
            status: "in_progress".to_string(),
            completed_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let mut instance_items: Vec<ChecklistInstanceItem> = Vec::new();
        for item in &items {
            let instance_item = ChecklistInstanceItem {
                id: new_id(),
                instance_id: instance.id.clone(),
                template_item_id: item.id.clone(),
                title: item.title.clone(),
                required: item.required,
                sort_order: item.sort_order,
                completed: false,
                evidence: String::new(),
                completed_by: None,
                completed_at: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };
            state
                .instance_items
                .insert(instance_item.id.clone(), instance_item.clone());
            instance_items.push(instance_item);
        }
        state
            .instances
            .insert(instance.id.clone(), instance.clone());
        Ok(json!({ "instance": instance, "items": instance_items }))
    })();
    respond_created(result)
}

async fn list_instances(
    state: web::Data<Arc<RiskState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (_viewer_id, viewer_role) = viewer_of(&params);
    if let Err(error) = require_capability(&state, viewer_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    let mut items: Vec<ChecklistInstance> = state
        .instances
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|instance| {
            params
                .filters
                .get("template_id")
                .is_none_or(|template| instance.template_id == *template)
                && params
                    .filters
                    .get("company_id")
                    .is_none_or(|company| instance.company_id.as_deref() == Some(company.as_str()))
                && params
                    .filters
                    .get("risk_id")
                    .is_none_or(|risk| instance.risk_id.as_deref() == Some(risk.as_str()))
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| instance.status == *status)
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn get_instance(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<String>,
    query: web::Query<ActorQuery>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<Value> = (|| {
        let instance = state
            .instances
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist instance not found"))?;
        let mut items: Vec<ChecklistInstanceItem> = state
            .instance_items
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| item.instance_id == instance.id)
            .collect();
        items.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.created_at.cmp(&b.created_at)));
        Ok(json!({ "instance": instance, "items": items }))
    })();
    json::respond(result, false)
}

/// Complete one checklist instance item with evidence. When every
/// required item of the instance is complete, the instance
/// completes and emits `risk.checklist_completed`.
async fn complete_instance_item(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<(String, String)>,
    query: web::Query<ActorQuery>,
    body: web::Json<CompleteChecklistItem>,
) -> HttpResponse {
    let (actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:edit") {
        return json::error_response(&error, false);
    }
    let (instance_id, item_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let mut item = state
            .instance_items
            .get_mut(item_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Checklist item not found"))?;
        if item.instance_id != instance_id {
            return Err(UnifiedError::not_found("Checklist item not found"));
        }
        item.completed = true;
        item.evidence = body
            .evidence
            .clone()
            .map(|value| value.trim().to_string())
            .unwrap_or_default();
        item.completed_by = actor_id.clone();
        item.completed_at = Some(Utc::now());
        item.updated_at = Utc::now();
        let updated_item = item.clone();
        drop(item);
        let all_required_done = state
            .instance_items
            .iter()
            .filter(|entry| entry.value().instance_id == instance_id)
            .all(|entry| !entry.value().required || entry.value().completed);
        let mut completed_instance: Option<ChecklistInstance> = None;
        if all_required_done {
            if let Some(mut instance) = state.instances.get_mut(instance_id.as_str()) {
                if instance.status != "completed" {
                    instance.status = "completed".to_string();
                    instance.completed_at = Some(Utc::now());
                    instance.updated_at = Utc::now();
                    completed_instance = Some(instance.clone());
                }
            }
        }
        if let Some(instance) = completed_instance {
            state.emit(
                "risk.checklist_completed",
                json!({
                    "instance_id": instance.id,
                    "template_id": instance.template_id,
                    "company_id": instance.company_id,
                    "risk_id": instance.risk_id,
                    "title": instance.title,
                    "completed_by": actor_id,
                }),
            );
        }
        Ok(json!({ "item": updated_item }))
    })();
    json::respond(result, false)
}

async fn reopen_instance_item(
    state: web::Data<Arc<RiskState>>,
    path: web::Path<(String, String)>,
    query: web::Query<ActorQuery>,
) -> HttpResponse {
    let (_actor_id, actor_role) = actor_of(&query.into_inner());
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:edit") {
        return json::error_response(&error, false);
    }
    let (instance_id, item_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let mut item = state
            .instance_items
            .get_mut(item_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Checklist item not found"))?;
        if item.instance_id != instance_id {
            return Err(UnifiedError::not_found("Checklist item not found"));
        }
        item.completed = false;
        item.evidence = String::new();
        item.completed_by = None;
        item.completed_at = None;
        item.updated_at = Utc::now();
        let updated_item = item.clone();
        drop(item);
        if let Some(mut instance) = state.instances.get_mut(instance_id.as_str()) {
            if instance.status == "completed" {
                instance.status = "in_progress".to_string();
                instance.completed_at = None;
                instance.updated_at = Utc::now();
            }
        }
        Ok(json!({ "item": updated_item }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// Answer `risk.review_check_requested` by running the review-date
/// scan and reporting how many alerts were raised.
fn on_review_check_requested(state: Arc<RiskState>, ctx: &mut HookContext) {
    let company_id = ctx
        .data
        .get("company_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|value| !value.is_empty());
    let raised = state.run_review_check(company_id.as_deref());
    ctx.add_result(json!({
        "company_id": company_id,
        "alerts_raised": raised,
    }));
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct RiskModule {
    state: Arc<RiskState>,
}

impl RiskModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RiskState::new()),
        }
    }
}

impl Default for RiskModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for RiskModule {
    fn id(&self) -> &'static str {
        "risk"
    }

    fn name(&self) -> &'static str {
        "Risk Mitigation"
    }

    fn description(&self) -> &'static str {
        "Risk register with server-computed likelihood x impact scores and bands, mitigation plans with tasks requested from the Projects module, threshold and review-date alerts, and compliance checklists."
    }

    fn requires(&self) -> &[&'static str] {
        &["notifications", "audit", "projects"]
    }

    fn available_as_product(&self) -> bool {
        false
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_review_check_requested(state.clone(), ctx));
        bus.register("risk.review_check_requested", "risk", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/risk")
                // Literal segments first: they must win over `/{id}`.
                .route("/matrix", web::get().to(risk_matrix))
                .route("/review-check", web::post().to(review_check))
                .route("/alerts", web::get().to(list_alerts))
                .route("/risks", web::get().to(list_risks))
                .route("/risks", web::post().to(create_risk))
                .route("/risks/{id}", web::get().to(get_risk))
                .route("/risks/{id}", web::put().to(update_risk))
                .route("/risks/{id}", web::delete().to(delete_risk))
                .route("/risks/{id}/plans", web::get().to(list_plans_for_risk))
                .route("/risks/{id}/plans", web::post().to(create_plan))
                .route("/plans/{id}", web::get().to(get_plan))
                .route("/plans/{id}", web::put().to(update_plan))
                .route("/plans/{id}", web::delete().to(delete_plan))
                .route("/plans/{id}/tasks", web::get().to(list_tasks_for_plan))
                .route("/plans/{id}/tasks", web::post().to(create_task))
                .route("/tasks/{id}", web::put().to(update_task))
                .route("/tasks/{id}", web::delete().to(delete_task))
                .route("/checklist-templates", web::get().to(list_templates))
                .route("/checklist-templates", web::post().to(create_template))
                .route("/checklist-templates/{id}", web::get().to(get_template))
                .route("/checklist-templates/{id}", web::put().to(update_template))
                .route("/checklist-templates/{id}", web::delete().to(delete_template))
                .route(
                    "/checklist-templates/{id}/items",
                    web::get().to(list_template_items),
                )
                .route(
                    "/checklist-templates/{id}/items",
                    web::post().to(add_template_item),
                )
                .route(
                    "/checklist-templates/{template_id}/items/{item_id}",
                    web::delete().to(delete_template_item),
                )
                .route("/checklist-instances", web::get().to(list_instances))
                .route("/checklist-instances", web::post().to(create_instance))
                .route("/checklist-instances/{id}", web::get().to(get_instance))
                .route(
                    "/checklist-instances/{id}/items/{item_id}/complete",
                    web::post().to(complete_instance_item),
                )
                .route(
                    "/checklist-instances/{id}/items/{item_id}/reopen",
                    web::post().to(reopen_instance_item),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/risk/risks", owner: "risk", description: "List risks (paginated; filter by company_id, category, status, band, owner_id, min_score)." },
            RouteInfo { method: "POST", path: "/api/v1/risk/risks", owner: "risk", description: "Create a risk (score and band are computed server-side)." },
            RouteInfo { method: "GET", path: "/api/v1/risk/risks/{id}", owner: "risk", description: "Get one risk." },
            RouteInfo { method: "PUT", path: "/api/v1/risk/risks/{id}", owner: "risk", description: "Update a risk (score and band are recomputed server-side)." },
            RouteInfo { method: "DELETE", path: "/api/v1/risk/risks/{id}", owner: "risk", description: "Delete a risk with its plans, tasks, and alerts." },
            RouteInfo { method: "GET", path: "/api/v1/risk/matrix", owner: "risk", description: "5x5 likelihood x impact matrix with counts and band thresholds." },
            RouteInfo { method: "GET", path: "/api/v1/risk/risks/{id}/plans", owner: "risk", description: "List a risk's mitigation plans." },
            RouteInfo { method: "POST", path: "/api/v1/risk/risks/{id}/plans", owner: "risk", description: "Create a mitigation plan for a risk." },
            RouteInfo { method: "GET", path: "/api/v1/risk/plans/{id}", owner: "risk", description: "Get one mitigation plan." },
            RouteInfo { method: "PUT", path: "/api/v1/risk/plans/{id}", owner: "risk", description: "Update a mitigation plan." },
            RouteInfo { method: "DELETE", path: "/api/v1/risk/plans/{id}", owner: "risk", description: "Delete a mitigation plan with its tasks." },
            RouteInfo { method: "GET", path: "/api/v1/risk/plans/{id}/tasks", owner: "risk", description: "List a plan's mitigation tasks." },
            RouteInfo { method: "POST", path: "/api/v1/risk/plans/{id}/tasks", owner: "risk", description: "Create a mitigation task and request its backing task from the Projects module." },
            RouteInfo { method: "PUT", path: "/api/v1/risk/tasks/{id}", owner: "risk", description: "Update a mitigation task." },
            RouteInfo { method: "DELETE", path: "/api/v1/risk/tasks/{id}", owner: "risk", description: "Delete a mitigation task." },
            RouteInfo { method: "POST", path: "/api/v1/risk/review-check", owner: "risk", description: "Run the review-date scan and raise review_overdue alerts." },
            RouteInfo { method: "GET", path: "/api/v1/risk/alerts", owner: "risk", description: "List risk alerts (threshold crossings and overdue reviews)." },
            RouteInfo { method: "GET", path: "/api/v1/risk/checklist-templates", owner: "risk", description: "List compliance checklist templates." },
            RouteInfo { method: "POST", path: "/api/v1/risk/checklist-templates", owner: "risk", description: "Create a compliance checklist template." },
            RouteInfo { method: "GET", path: "/api/v1/risk/checklist-templates/{id}", owner: "risk", description: "Get one checklist template." },
            RouteInfo { method: "PUT", path: "/api/v1/risk/checklist-templates/{id}", owner: "risk", description: "Update a checklist template." },
            RouteInfo { method: "DELETE", path: "/api/v1/risk/checklist-templates/{id}", owner: "risk", description: "Delete a checklist template (refused while instances exist)." },
            RouteInfo { method: "GET", path: "/api/v1/risk/checklist-templates/{id}/items", owner: "risk", description: "List a template's checklist items." },
            RouteInfo { method: "POST", path: "/api/v1/risk/checklist-templates/{id}/items", owner: "risk", description: "Add an item to a checklist template." },
            RouteInfo { method: "DELETE", path: "/api/v1/risk/checklist-templates/{template_id}/items/{item_id}", owner: "risk", description: "Delete a checklist template item." },
            RouteInfo { method: "GET", path: "/api/v1/risk/checklist-instances", owner: "risk", description: "List checklist instances." },
            RouteInfo { method: "POST", path: "/api/v1/risk/checklist-instances", owner: "risk", description: "Create a checklist instance from a template." },
            RouteInfo { method: "GET", path: "/api/v1/risk/checklist-instances/{id}", owner: "risk", description: "Get a checklist instance with its items." },
            RouteInfo { method: "POST", path: "/api/v1/risk/checklist-instances/{id}/items/{item_id}/complete", owner: "risk", description: "Complete a checklist item with evidence notes." },
            RouteInfo { method: "POST", path: "/api/v1/risk/checklist-instances/{id}/items/{item_id}/reopen", owner: "risk", description: "Reopen a completed checklist item." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(RiskModule::new()));
}
