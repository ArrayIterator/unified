// Surveys / NPS / CSAT module — survey builder with versions,
// distribution triggers, tokenized responses, NPS + CSAT scoring
// with trends, and low-score follow-ups through the tasks module.
//
// Routes live under `/api/v1/surveys`:
//
//   Surveys:
//     GET/POST / — list surveys / create a survey (draft, version 1).
//     GET/PATCH/DELETE /{id} — get one survey; PATCH on a draft
//       edits in place, PATCH on a published survey creates a new
//       version (draft) and returns it; DELETE removes a draft only.
//     POST /{id}/publish — publish a draft version (the previously
//       published version of the same family is archived).
//     POST /{id}/archive.
//   Questions:
//     GET/POST /{id}/questions — list / add a question (draft
//       surveys only).
//     GET/PATCH/DELETE /{id}/questions/{question_id} (draft only).
//   Results and responses:
//     GET  /{id}/results — NPS + CSAT scores, per-question stats,
//       breakdowns by agent / department / product, and trends
//       (?bucket=daily|weekly, ?from, ?to, ?agent_id,
//       ?department_id, ?product_id).
//     GET  /{id}/responses — paginated, filter by status
//       (submitted / pending), score band, agent_id,
//       department_id, product_id.
//     GET  /responses/{response_id} — one response with answers.
//   Distributions:
//     GET/POST /{id}/distributions — list / create a distribution
//       (trigger: manual, ticket_closed, purchase, scheduled).
//     GET  /distributions/{id}.
//     POST /distributions/{id}/send — send the invitations now
//       (manual recipient list, or re-send pending ones).
//     DELETE /distributions/{id} — delete a pending distribution.
//   Public response (no auth; see the public-access note below):
//     GET  /public/{token} — the survey + questions for one
//       response token.
//     POST /public/{token} — submit the answers for one token.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory survey versions, questions, distributions, responses,
// and answers; versioning (editing a published survey creates a
// new draft version in the same family; publishing archives the
// previous published version; every response references the exact
// version row it answered); tokenized response links (32 random
// bytes, base64url, one response per recipient per survey version,
// token expiry); NPS computed on read as %promoters minus
// %detractors and CSAT as % satisfied (score 4–5 of 5), with
// daily / weekly trend buckets and breakdowns by agent /
// department / product; trigger listeners for `ticket.updated`
// (a ticket moving to `resolved` / `closed` invites its user to
// the surveys with an active `ticket_closed` distribution) and
// `store.order.completed` / `order.paid` (a completed order
// invites its buyer to the surveys with an active `purchase`
// distribution); scheduled distributions queued through
// `scheduler.enqueue`; low-score follow-up as a
// `task.create_requested` event for the tasks module plus a
// `notification.requested` to the responsible user when one is
// configured. What is NOT built here: actual delivery of survey
// invitations (the module creates the tokenized response rows and
// emits `notification.requested` per recipient — the notifications
// module owns delivery, exactly like the events module's reminder
// path); a handler answering `task.create_requested` (no such
// listener exists in `tasks.rs` today, so a follow-up stays
// `requested` with no task id until the tasks module — or an
// integrator — answers that event; the follow-up link is still
// stored on the response and `surveys.followup.created` is still
// emitted); and a public survey directory (public routes answer
// only for a valid, unexpired token).
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`). There is no reusable per-request
// "is this endpoint public?" helper to call from a handler — the
// check lives in the API Manager middleware / `resolve_public`
// path — so the public routes below rely on that gate: an
// administrator must create the matching `ApiAccessRule` rows for
// `GET /api/v1/surveys/public/{token}` and
// `POST /api/v1/surveys/public/{token}`. The routes are registered
// in `route_info()` with `route_version_support()` defaulting to
// `All`, so the API Manager's per-version registry picks them up.
// Do NOT treat the absence of a capability check on a public route
// as an open door: without the rule the API Manager denies the
// call. A response token is unguessable (32 random bytes) and
// expires, so a leaked token alone cannot enumerate surveys.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not
// allowed).
//
// Events emitted on the hook bus:
//   - `surveys.created`, `surveys.updated`, `surveys.published`,
//     `surveys.archived`, `surveys.version_created`,
//     `surveys.deleted`
//   - `surveys.question.created`, `surveys.question.updated`,
//     `surveys.question.deleted`
//   - `surveys.distribution.created`, `surveys.distribution.sent`
//   - `surveys.response.created` (invitation issued),
//     `surveys.response.received`
//   - `surveys.followup.created` (low-score follow-up requested)
//   - `task.create_requested` (follow-up task request for the
//     tasks module: `{"source": "surveys", "title", "body",
//     "assignee_id", "department_id", "survey_id",
//     "response_id"}`; the answering handler contributes a
//     `task_id` / `id` result, like `ticket.create_requested`)
//   - `notification.requested` (survey invitations and low-score
//     follow-up notices, in the notifications module's payload
//     shape)
//   - `scheduler.enqueue` with `{"kind": "surveys.send",
//     "payload": {"survey_id", "distribution_id", ...}}` when a
//     scheduled distribution is created.
// Events this module listens to:
//   - `ticket.updated` — a ticket moving to `resolved` or `closed`
//     invites its user to every survey with an active
//     `ticket_closed` distribution (department filter respected).
//   - `store.order.completed`, `order.paid` — a completed order
//     invites its buyer to every survey with an active `purchase`
//     distribution (product filter respected when the order
//     payload names products).
//   - `surveys.send_requested` — the event the scheduler emits
//     when a `surveys.send` job fires, following its naming rule
//     (`backup.run` -> `backup.run_requested`); sends the pending
//     invitations of the named distribution.
//
// Scheduler note: `surveys.send` is NOT in the scheduler's
// `KNOWN_KINDS` list yet, so today the scheduler answers
// `{"queued": false, "error": "Unknown job kind: ..."}` and the
// distribution records `scheduler_status: "declined"` instead of
// pretending it was scheduled. The distribution is still stored
// and its invitations can be sent manually through
// `POST /distributions/{id}/send`; the follow-up is to add
// `"surveys.send"` to `KNOWN_KINDS` in `scheduler.rs` and arm the
// dispatch so the job emits `surveys.send_requested` with the
// distribution payload (see the integrator snippet in the module
// report).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Datelike, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_TITLE_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_QUESTION_LEN: usize = 1000;
const MAX_TEXT_LEN: usize = 5000;
const MAX_ID_LEN: usize = 255;
const MAX_EMAIL_LEN: usize = 254;
const MAX_OPTIONS: usize = 20;
const MAX_OPTION_LEN: usize = 200;
const MAX_RECIPIENTS: usize = 1000;
/// Default token lifetime for a response invitation (days).
const DEFAULT_TOKEN_DAYS: i64 = 30;
/// NPS detractor threshold: NPS answers 0..=6 are detractors,
/// 7–8 passives, 9–10 promoters.
const NPS_DETRACTOR_MAX: i32 = 6;
const NPS_PROMOTER_MIN: i32 = 9;
/// CSAT satisfied threshold: scores 4–5 of 5 count as satisfied;
/// 1–2 count as dissatisfied and trigger a follow-up.
const CSAT_SATISFIED_MIN: i32 = 4;
const CSAT_FOLLOWUP_MAX: i32 = 2;

/// Capability names checked through the `role.check` hook.
const PERM_SURVEYS_VIEW: &str = "surveys.view";
const PERM_SURVEYS_MANAGE: &str = "surveys.manage";

/// Hook event names this module listens to.
const TICKET_UPDATED: &str = "ticket.updated";
const STORE_ORDER_COMPLETED: &str = "store.order.completed";
const ORDER_PAID: &str = "order.paid";
const SEND_REQUESTED: &str = "surveys.send_requested";

/// Scheduler job kind used for scheduled distributions.
const SCHEDULER_KIND_SEND: &str = "surveys.send";

/// Question types. `nps` answers are 0–10, `csat` and `rating`
/// answers are 1–5, `text` answers are free text, and
/// `multiple_choice` answers must be one of `options`.
const QUESTION_TYPES: [&str; 5] = ["nps", "csat", "rating", "text", "multiple_choice"];
/// Distribution triggers.
const TRIGGERS: [&str; 4] = ["manual", "ticket_closed", "purchase", "scheduled"];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One survey version. A survey family shares `family_id` across
/// versions: version 1 has `family_id == id`; editing a published
/// survey creates a new row with `version + 1`,
/// `supersedes = Some(old id)`, and status `draft`. Publishing a
/// version archives the previously published version of the same
/// family. Responses always reference the exact version row they
/// answered (`survey_id`), never the family.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Survey {
    pub id: Uuid,
    pub family_id: Uuid,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<Uuid>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One question of one survey version. `options` applies to
/// `multiple_choice` only. `min_value` / `max_value` record the
/// score bounds the question was created with (NPS 0–10, CSAT and
/// rating 1–5); answers outside the bounds are rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurveyQuestion {
    pub id: Uuid,
    pub survey_id: Uuid,
    pub position: u32,
    pub question_type: String,
    pub text: String,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_value: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_value: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One distribution of one survey version: how invitations reach
/// recipients. `trigger` is `manual` (explicit recipient list),
/// `ticket_closed` (ticket close events), `purchase` (completed
/// orders), or `scheduled` (scheduler job at `scheduled_at`).
/// `department_id` / `product_id` narrow the trigger filters.
/// `responsible_user_id` receives low-score follow-up notices for
/// responses from this distribution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurveyDistribution {
    pub id: Uuid,
    pub survey_id: Uuid,
    pub trigger: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responsible_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<DateTime<Utc>>,
    /// Token lifetime for invitations from this distribution, in
    /// days (default 30).
    pub expires_in_days: i64,
    /// `pending`, `sent`, `completed`, `cancelled`.
    pub status: String,
    /// `not_scheduled`, `queued`, or `declined` (see the scheduler
    /// note in the module header; only meaningful for `scheduled`
    /// distributions).
    pub scheduler_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_job_id: Option<String>,
    pub sent_count: i64,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One response invitation / submitted response. One row per
/// recipient per survey version: the module refuses a second
/// invitation for the same recipient on the same version.
/// `token` is the unguessable response link token (32 random
/// bytes, base64url); `submitted_at` is `None` until the recipient
/// answers. `follow_up_status` is `none`, `requested`, or
/// `created`; `follow_up_task_id` holds the task id when the tasks
/// module answered `task.create_requested`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurveyResponse {
    pub id: Uuid,
    pub survey_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribution_id: Option<Uuid>,
    pub recipient_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    pub token: String,
    pub token_expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_at: Option<DateTime<Utc>>,
    pub follow_up_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_task_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One answer inside a response. Exactly one of `score`, `text`,
/// or `choice` is set, matching the question type: `score` for
/// `nps` / `csat` / `rating`, `text` for `text`, `choice` for
/// `multiple_choice`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurveyAnswer {
    pub id: Uuid,
    pub response_id: Uuid,
    pub question_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateSurveyRequest {
    pub title: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSurveyRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateQuestionRequest {
    pub question_type: String,
    pub text: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub options: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateQuestionRequest {
    pub text: Option<String>,
    pub required: Option<bool>,
    pub options: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDistributionRequest {
    pub trigger: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responsible_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<DateTime<Utc>>,
    pub expires_in_days: Option<i64>,
    /// Recipients for a `manual` distribution: `{recipient_id,
    /// recipient_email?, agent_id?, department_id?, product_id?}`.
    #[serde(default)]
    pub recipients: Vec<DistributionRecipient>,
}

#[derive(Debug, Deserialize)]
pub struct DistributionRecipient {
    pub recipient_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SendDistributionRequest {
    /// Extra recipients to invite on a manual send (merged with
    /// the distribution's pending recipients). Ignored for
    /// trigger distributions.
    #[serde(default)]
    pub recipients: Vec<DistributionRecipient>,
}

#[derive(Debug, Deserialize)]
pub struct SubmitResponseRequest {
    pub answers: Vec<SubmitAnswer>,
}

#[derive(Debug, Deserialize)]
pub struct SubmitAnswer {
    pub question_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for surveys. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); handlers tolerate a missing bus
/// by skipping event emission. Durable rows for these types are
/// defined by migration `2026-11-22-000055_surveys`; until that
/// migration is executed, surveys do not survive a restart.
#[derive(Clone)]
pub struct SurveysState {
    pub surveys: Arc<DashMap<Uuid, Survey>>,
    pub questions: Arc<DashMap<Uuid, SurveyQuestion>>,
    pub distributions: Arc<DashMap<Uuid, SurveyDistribution>>,
    pub responses: Arc<DashMap<Uuid, SurveyResponse>>,
    pub answers: Arc<DashMap<Uuid, SurveyAnswer>>,
    /// Recipients supplied at distribution creation, waiting for
    /// the send (manual distributions send them on
    /// `POST /distributions/{id}/send`; scheduled distributions
    /// send them when the `surveys.send` job fires). Keyed by
    /// distribution id; cleared once the invitations are issued.
    pub pending_recipients: Arc<DashMap<Uuid, Vec<DistributionRecipient>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for SurveysState {
    fn default() -> Self {
        Self {
            surveys: Arc::new(DashMap::new()),
            questions: Arc::new(DashMap::new()),
            distributions: Arc::new(DashMap::new()),
            responses: Arc::new(DashMap::new()),
            answers: Arc::new(DashMap::new()),
            pending_recipients: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl SurveysState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit an event and return the final hook context, so callers
    /// can read handler results (for example the task created by
    /// the tasks module in answer to `task.create_requested`).
    /// With no bus, returns an empty context for the event.
    fn emit_ctx(&self, event: &str, data: Value) -> HookContext {
        match self.bus() {
            Some(bus) => bus.emit(event, data),
            None => HookContext::new(event, data),
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape.
    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "surveys",
                "category": "surveys",
                "priority": "normal",
                "link": link,
            }),
        );
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
        )))
    }
    Ok(trimmed.to_string())
}

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
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

fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || email.contains(' ') {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// One unguessable response token: 32 random bytes, base64url
/// encoded (43 characters, no padding). Regenerated while the
/// token already exists, so tokens stay unique.
fn generate_token(state: &SurveysState) -> String {
    loop {
        let bytes: [u8; 32] = rand::random();
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        if !state.responses.iter().any(|entry| entry.value().token == token) {
            return token;
        }
    }
}

fn load_survey(state: &SurveysState, raw_id: &str) -> UnifiedResult<Survey> {
    let id = parse_uuid(raw_id, "survey")?;
    state
        .surveys
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Survey not found: {id}")))
}

fn load_question(state: &SurveysState, survey_id: Uuid, raw_id: &str) -> UnifiedResult<SurveyQuestion> {
    let id = parse_uuid(raw_id, "question")?;
    state
        .questions
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|question| question.survey_id == survey_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Question not found: {id}")))
}

fn load_distribution(state: &SurveysState, raw_id: &str) -> UnifiedResult<SurveyDistribution> {
    let id = parse_uuid(raw_id, "distribution")?;
    state
        .distributions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Distribution not found: {id}")))
}

fn load_response(state: &SurveysState, raw_id: &str) -> UnifiedResult<SurveyResponse> {
    let id = parse_uuid(raw_id, "response")?;
    state
        .responses
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Response not found: {id}")))
}

fn questions_for(state: &SurveysState, survey_id: Uuid) -> Vec<SurveyQuestion> {
    let mut items: Vec<SurveyQuestion> = state
        .questions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|question| question.survey_id == survey_id)
        .collect();
    items.sort_by_key(|question| question.position);
    items
}

fn responses_for_survey(state: &SurveysState, survey_id: Uuid) -> Vec<SurveyResponse> {
    let mut items: Vec<SurveyResponse> = state
        .responses
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|response| response.survey_id == survey_id)
        .collect();
    items.sort_by_key(|response| response.created_at);
    items
}

fn answers_for_response(state: &SurveysState, response_id: Uuid) -> Vec<SurveyAnswer> {
    let mut items: Vec<SurveyAnswer> = state
        .answers
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|answer| answer.response_id == response_id)
        .collect();
    items.sort_by_key(|answer| answer.created_at);
    items
}

/// Score bounds for a question type: NPS 0–10, CSAT and rating
/// 1–5, no score bounds for text / multiple choice.
fn score_bounds(question_type: &str) -> (Option<i32>, Option<i32>) {
    match question_type {
        "nps" => (Some(0), Some(10)),
        "csat" | "rating" => (Some(1), Some(5)),
        _ => (None, None),
    }
}

fn valid_question_type(raw: &str) -> UnifiedResult<String> {
    let value = raw.trim().to_lowercase();
    if QUESTION_TYPES.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(UnifiedError::bad_request(format!(
            "question_type must be one of: {}",
            QUESTION_TYPES.join(", ")
        )))
    }
}

fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Invitations, follow-ups, scoring
// ---------------------------------------------------------------------------

/// Issue one invitation (response row) for a recipient on one
/// survey version. Enforces one response per recipient per survey
/// version: returns `None` when the recipient already has a row
/// for this version. The token is unguessable and expires
/// `expires_in_days` after creation. Emits
/// `surveys.response.created` and a `notification.requested`
/// invitation for the recipient.
fn invite_recipient(
    state: &SurveysState,
    survey: &Survey,
    distribution: Option<&SurveyDistribution>,
    recipient_id: &str,
    recipient_email: Option<String>,
    agent_id: Option<String>,
    department_id: Option<String>,
    product_id: Option<String>,
) -> Option<SurveyResponse> {
    if state
        .responses
        .iter()
        .any(|entry| entry.value().survey_id == survey.id && entry.value().recipient_id == recipient_id)
    {
        return None;
    }
    let expires_in_days = distribution.map(|item| item.expires_in_days).unwrap_or(DEFAULT_TOKEN_DAYS);
    let now = Utc::now();
    let response = SurveyResponse {
        id: Uuid::now_v7(),
        survey_id: survey.id,
        distribution_id: distribution.map(|item| item.id),
        recipient_id: recipient_id.to_string(),
        recipient_email: recipient_email.clone(),
        agent_id,
        department_id,
        product_id,
        token: generate_token(state),
        token_expires_at: now + Duration::days(expires_in_days),
        submitted_at: None,
        follow_up_status: "none".to_string(),
        follow_up_task_id: None,
        created_at: now,
        updated_at: now,
    };
    state.responses.insert(response.id, response.clone());
    state.emit(
        "surveys.response.created",
        json!({
            "survey_id": survey.id.to_string(),
            "response_id": response.id.to_string(),
            "recipient_id": recipient_id,
            "distribution_id": distribution.map(|item| item.id.to_string()),
            "token_expires_at": response.token_expires_at.to_rfc3339(),
        }),
    );
    state.notify(
        "surveys.invitation",
        recipient_id,
        &format!("Please share your feedback: {}", survey.title),
        &format!(
            "You are invited to answer the survey \"{}\". Your response link expires on {}.",
            survey.title,
            response.token_expires_at.format("%Y-%m-%d")
        ),
        Some(format!("/api/v1/surveys/public/{}", response.token)),
    );
    Some(response)
}

/// Send the pending invitations of one distribution: the stored
/// pending recipients (supplied at creation) plus any extra
/// recipients passed by the caller. For trigger distributions
/// (`ticket_closed`, `purchase`) this only sends explicitly passed
/// recipients — their real invitations are created by the trigger
/// listeners. Returns how many invitations were newly issued.
fn send_distribution_invitations(
    state: &SurveysState,
    distribution: &mut SurveyDistribution,
    mut recipients: Vec<DistributionRecipient>,
) -> UnifiedResult<usize> {
    let survey = state
        .surveys
        .get(&distribution.survey_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Survey not found: {}", distribution.survey_id)))?;
    if let Some((_, stored)) = state.pending_recipients.remove(&distribution.id) {
        recipients.extend(stored);
    }
    let mut issued = 0usize;
    for recipient in &recipients {
        let recipient_id = recipient.recipient_id.trim();
        if recipient_id.is_empty() || recipient_id.len() > MAX_ID_LEN {
            continue;
        }
        if let Some(email) = &recipient.recipient_email {
            if !valid_email(email) {
                continue;
            }
        }
        let invited = invite_recipient(
            state,
            &survey,
            Some(distribution),
            recipient_id,
            recipient.recipient_email.clone(),
            recipient
                .agent_id
                .clone()
                .or_else(|| distribution.agent_id.clone()),
            recipient
                .department_id
                .clone()
                .or_else(|| distribution.department_id.clone()),
            recipient
                .product_id
                .clone()
                .or_else(|| distribution.product_id.clone()),
        );
        if invited.is_some() {
            issued += 1;
        }
    }
    distribution.sent_count += issued as i64;
    if distribution.trigger == "manual" || distribution.trigger == "scheduled" {
        distribution.status = "sent".to_string();
    }
    distribution.updated_at = Utc::now();
    state.distributions.insert(distribution.id, distribution.clone());
    state.emit(
        "surveys.distribution.sent",
        json!({
            "survey_id": survey.id.to_string(),
            "distribution_id": distribution.id.to_string(),
            "issued": issued,
            "sent_count": distribution.sent_count,
        }),
    );
    Ok(issued)
}

/// Handle a low score on a submitted response: an NPS detractor
/// (0–6) or a CSAT score of 1–2. Requests a follow-up task from
/// the tasks module via `task.create_requested` (request/response:
/// a handler that answers contributes a `task_id` / `id` result),
/// stores the follow-up link on the response, notifies the
/// responsible user when one is configured, and emits
/// `surveys.followup.created`. The follow-up stays `requested`
/// with no task id when no handler answers (see the HONESTY note).
fn create_followup(
    state: &SurveysState,
    survey: &Survey,
    response: &mut SurveyResponse,
    reason: &str,
    low_score: i32,
) {
    let title = format!(
        "Follow up low survey score ({}: {})",
        survey.title, low_score
    );
    let body = format!(
        "Survey \"{}\" (version {}) received a low score ({}) from recipient \"{}\". Response id: {}. Please contact the customer and record the outcome.",
        survey.title, survey.version, reason, response.recipient_id, response.id
    );
    let responsible: Option<String> = match response.distribution_id {
        Some(distribution_id) => state
            .distributions
            .get(&distribution_id)
            .and_then(|entry| entry.value().responsible_user_id.clone()),
        None => None,
    };
    let answer = state.emit_ctx(
        "task.create_requested",
        json!({
            "source": "surveys",
            "title": title,
            "body": body,
            "assignee_id": responsible,
            "department_id": response.department_id,
            "survey_id": survey.id.to_string(),
            "response_id": response.id.to_string(),
        }),
    );
    let mut task_id: Option<String> = None;
    for result in &answer.results {
        if let Some(id) = result
            .get("task_id")
            .or_else(|| result.get("id"))
            .and_then(|value| value.as_str())
        {
            task_id = Some(id.to_string());
            break;
        }
    }
    response.follow_up_task_id = task_id.clone();
    response.follow_up_status = if task_id.is_some() {
        "created".to_string()
    } else {
        "requested".to_string()
    };
    response.updated_at = Utc::now();
    state.responses.insert(response.id, response.clone());
    state.emit(
        "surveys.followup.created",
        json!({
            "survey_id": survey.id.to_string(),
            "response_id": response.id.to_string(),
            "reason": reason,
            "low_score": low_score,
            "task_id": task_id,
            "status": response.follow_up_status,
        }),
    );
    if let Some(user_id) = responsible {
        state.notify(
            "surveys.followup.created",
            &user_id,
            &format!("Low survey score needs follow-up: {}", survey.title),
            &body,
            Some(format!("/api/v1/surveys/responses/{}", response.id)),
        );
    }
}

/// Inspect a submitted response for low scores and create at most
/// one follow-up per response (the worst low score wins: an NPS
/// detractor beats a low CSAT when both are present).
fn maybe_create_followup(state: &SurveysState, survey: &Survey, response: &mut SurveyResponse) {
    if response.follow_up_status != "none" {
        return;
    }
    let answers = answers_for_response(state, response.id);
    let mut worst: Option<(String, i32)> = None;
    for answer in &answers {
        let Some(question) = state.questions.get(&answer.question_id) else {
            continue;
        };
        let Some(score) = answer.score else {
            continue;
        };
        if question.question_type == "nps" && score <= NPS_DETRACTOR_MAX {
            let candidate = ("nps_detractor".to_string(), score);
            if worst.as_ref().map(|(_, worst_score)| score < *worst_score).unwrap_or(true) {
                worst = Some(candidate);
            }
        }
        if question.question_type == "csat" && score <= CSAT_FOLLOWUP_MAX {
            let candidate = ("csat_low".to_string(), score);
            if worst.is_none() {
                worst = Some(candidate);
            }
        }
    }
    if let Some((reason, low_score)) = worst {
        create_followup(state, survey, response, &reason, low_score);
    }
}

/// Per-question score summary, computed on read from the submitted
/// responses. `nps` is %promoters minus %detractors; `csat` is
/// % satisfied (score 4–5). Counts, averages, and the score
/// distribution are included for every scored question.
fn question_stats(
    state: &SurveysState,
    question: &SurveyQuestion,
    responses: &[SurveyResponse],
) -> Value {
    let submitted_ids: Vec<Uuid> = responses
        .iter()
        .filter(|response| response.submitted_at.is_some())
        .map(|response| response.id)
        .collect();
    let scores: Vec<i32> = state
        .answers
        .iter()
        .filter(|entry| {
            entry.value().question_id == question.id
                && submitted_ids.contains(&entry.value().response_id)
        })
        .filter_map(|entry| entry.value().score)
        .collect();
    let total = scores.len() as f64;
    let mut distribution: HashMap<String, u64> = HashMap::new();
    for score in &scores {
        *distribution.entry(score.to_string()).or_default() += 1;
    }
    let mut stats = json!({
        "question_id": question.id.to_string(),
        "question_type": question.question_type,
        "text": question.text,
        "responses": scores.len() as u64,
        "distribution": distribution,
    });
    if total > 0.0 {
        let sum: i32 = scores.iter().sum();
        let average = (sum as f64 / total * 100.0).round() / 100.0;
        if let Value::Object(map) = &mut stats {
            map.insert("average".to_string(), json!(average));
        }
        if question.question_type == "nps" {
            let promoters = scores.iter().filter(|score| **score >= NPS_PROMOTER_MIN).count() as f64;
            let detractors = scores.iter().filter(|score| **score <= NPS_DETRACTOR_MAX).count() as f64;
            let nps = ((promoters - detractors) / total * 100.0).round();
            if let Value::Object(map) = &mut stats {
                map.insert("nps".to_string(), json!(nps));
                map.insert("promoters".to_string(), json!(promoters as u64));
                map.insert("detractors".to_string(), json!(detractors as u64));
                map.insert("passives".to_string(), json!((total - promoters - detractors) as u64));
            }
        }
        if question.question_type == "csat" {
            let satisfied = scores.iter().filter(|score| **score >= CSAT_SATISFIED_MIN).count() as f64;
            let dissatisfied = scores.iter().filter(|score| **score <= CSAT_FOLLOWUP_MAX).count() as f64;
            let csat = (satisfied / total * 100.0).round();
            if let Value::Object(map) = &mut stats {
                map.insert("csat".to_string(), json!(csat));
                map.insert("satisfied".to_string(), json!(satisfied as u64));
                map.insert("dissatisfied".to_string(), json!(dissatisfied as u64));
            }
        }
    }
    stats
}

/// Bucket key for a trend bucket: the day (`YYYY-MM-DD`) for
/// `daily`, the Monday of the week (`YYYY-MM-DD`) for `weekly`.
fn bucket_key(date: DateTime<Utc>, bucket: &str) -> String {
    if bucket == "weekly" {
        let weekday = date.weekday().num_days_from_monday() as i64;
        let monday = date.date_naive() - chrono::Duration::days(weekday);
        monday.format("%Y-%m-%d").to_string()
    } else {
        date.format("%Y-%m-%d").to_string()
    }
}

/// Trend buckets for one survey: per bucket, submitted count, NPS
/// (over NPS answers in the bucket), and CSAT (over CSAT answers
/// in the bucket).
fn trend_buckets(
    state: &SurveysState,
    responses: &[SurveyResponse],
    bucket: &str,
) -> Vec<Value> {
    let mut buckets: HashMap<String, (Vec<i32>, Vec<i32>, u64)> = HashMap::new();
    for response in responses {
        let Some(submitted_at) = response.submitted_at else {
            continue;
        };
        let key = bucket_key(submitted_at, bucket);
        let entry = buckets.entry(key).or_default();
        entry.2 += 1;
        for answer in answers_for_response(state, response.id) {
            let Some(question) = state.questions.get(&answer.question_id) else {
                continue;
            };
            let Some(score) = answer.score else {
                continue;
            };
            if question.question_type == "nps" {
                entry.0.push(score);
            } else if question.question_type == "csat" {
                entry.1.push(score);
            }
        }
    }
    let mut keys: Vec<String> = buckets.keys().cloned().collect();
    keys.sort();
    keys.into_iter()
        .map(|key| {
            let (nps_scores, csat_scores, count) = buckets.remove(&key).unwrap_or_default();
            let nps = if nps_scores.is_empty() {
                Value::Null
            } else {
                let total = nps_scores.len() as f64;
                let promoters = nps_scores.iter().filter(|score| **score >= NPS_PROMOTER_MIN).count() as f64;
                let detractors = nps_scores.iter().filter(|score| **score <= NPS_DETRACTOR_MAX).count() as f64;
                json!(((promoters - detractors) / total * 100.0).round())
            };
            let csat = if csat_scores.is_empty() {
                Value::Null
            } else {
                let total = csat_scores.len() as f64;
                let satisfied = csat_scores.iter().filter(|score| **score >= CSAT_SATISFIED_MIN).count() as f64;
                json!((satisfied / total * 100.0).round())
            };
            json!({
                "bucket": key,
                "responses": count,
                "nps": nps,
                "csat": csat,
            })
        })
        .collect()
}

/// Queue the `surveys.send` scheduler job for a scheduled
/// distribution. Mirrors the events module's `schedule_reminder`:
/// the scheduler answers `{"queued": true, "job_id": ...}` when it
/// knows the kind, and `{"queued": false, "error": ...}` today,
/// because `surveys.send` is not in `KNOWN_KINDS` yet. The
/// distribution is stored either way; the outcome is recorded
/// honestly on `scheduler_status` / `scheduler_job_id`.
fn schedule_distribution(state: &SurveysState, distribution: &mut SurveyDistribution) {
    let Some(bus) = state.bus() else {
        distribution.scheduler_status = "declined".to_string();
        return;
    };
    let Some(scheduled_at) = distribution.scheduled_at else {
        distribution.scheduler_status = "not_scheduled".to_string();
        return;
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": SCHEDULER_KIND_SEND,
            "payload": {
                "survey_id": distribution.survey_id.to_string(),
                "distribution_id": distribution.id.to_string(),
                "send_at": scheduled_at.to_rfc3339(),
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            distribution.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            distribution.scheduler_status = "queued".to_string();
        }
        _ => {
            distribution.scheduler_job_id = None;
            distribution.scheduler_status = "declined".to_string();
        }
    }
}

// ---------------------------------------------------------------------------
// Trigger listeners
// ---------------------------------------------------------------------------

/// Invite a recipient to every published survey version that has
/// an active trigger distribution matching the given trigger and
/// filters. Used by the ticket-closed and purchase listeners.
fn invite_for_trigger(
    state: &SurveysState,
    trigger: &str,
    recipient_id: &str,
    recipient_email: Option<String>,
    agent_id: Option<String>,
    department_id: Option<String>,
    product_id: Option<String>,
) {
    let distributions: Vec<SurveyDistribution> = state
        .distributions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|distribution| {
            distribution.trigger == trigger
                && distribution.status != "cancelled"
                && distribution
                    .department_id
                    .as_ref()
                    .map(|department| department_id.as_deref() == Some(department.as_str()))
                    .unwrap_or(true)
                && distribution
                    .product_id
                    .as_ref()
                    .map(|product| product_id.as_deref() == Some(product.as_str()))
                    .unwrap_or(true)
        })
        .collect();
    for distribution in &distributions {
        let Some(survey) = state.surveys.get(&distribution.survey_id).map(|entry| entry.value().clone()) else {
            continue;
        };
        if survey.status != "published" {
            continue;
        }
        invite_recipient(
            state,
            &survey,
            Some(distribution),
            recipient_id,
            recipient_email.clone(),
            agent_id.clone().or_else(|| distribution.agent_id.clone()),
            department_id.clone().or_else(|| distribution.department_id.clone()),
            product_id.clone().or_else(|| distribution.product_id.clone()),
        );
    }
}

/// A ticket moving to `resolved` or `closed` invites its user to
/// the surveys with an active `ticket_closed` distribution. The
/// tickets module emits `ticket.updated` with the full ticket JSON
/// (id, user_id, department_id, status, assigned_to); there is no
/// separate `ticket.closed` event in this tree.
fn on_ticket_updated(state: SurveysState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let status = opt_string(&data, "status").unwrap_or_default();
    if status != "resolved" && status != "closed" {
        return;
    }
    let Some(user_id) = opt_string(&data, "user_id") else {
        return;
    };
    invite_for_trigger(
        &state,
        "ticket_closed",
        &user_id,
        None,
        opt_string(&data, "assigned_to"),
        opt_string(&data, "department_id"),
        opt_string(&data, "product"),
    );
}

/// A completed order invites its buyer to the surveys with an
/// active `purchase` distribution. Both `store.order.completed`
/// and the legacy `order.paid` event are listened to; the payload
/// is read defensively (`user_id`, `customer_id`, `product_ids`
/// / `product_id`, `total`).
fn on_order_completed(state: SurveysState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = opt_string(&data, "user_id").or_else(|| opt_string(&data, "customer_id")) else {
        return;
    };
    let product_id = opt_string(&data, "product_id").or_else(|| {
        data.get("product_ids")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    invite_for_trigger(
        &state,
        "purchase",
        &user_id,
        opt_string(&data, "customer_email").or_else(|| opt_string(&data, "email")),
        None,
        None,
        product_id,
    );
}

/// Send the pending invitations of the named distribution when the
/// scheduler fires the `surveys.send` job: the recipients supplied
/// at creation are invited now.
fn on_send_requested(state: SurveysState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(raw_id) = opt_string(&data, "distribution_id") else {
        return;
    };
    let Ok(id) = Uuid::parse_str(&raw_id) else {
        return;
    };
    let Some(mut distribution) = state.distributions.get(&id).map(|entry| entry.value().clone()) else {
        return;
    };
    if distribution.status == "cancelled" {
        return;
    }
    let recipients: Vec<DistributionRecipient> = Vec::new();
    let _ = send_distribution_invitations(&state, &mut distribution, recipients);
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct SurveysModule {
    state: SurveysState,
}

impl SurveysModule {
    pub fn new() -> Self {
        Self {
            state: SurveysState::default(),
        }
    }
}

impl Default for SurveysModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SurveysModule {
    fn id(&self) -> &'static str {
        "surveys"
    }

    fn name(&self) -> &'static str {
        "Surveys / NPS / CSAT"
    }

    fn description(&self) -> &'static str {
        "Survey builder with versions (editing a published survey creates a new version; responses reference the version they answered), distribution triggers (ticket close, purchase, scheduled, manual) with tokenized response links, NPS and CSAT scoring computed on read with trends and breakdowns by agent, department, and product, and low-score follow-ups requested from the tasks module."
    }

    fn requires(&self) -> &[&'static str] {
        &["tickets", "store", "tasks", "notifications", "scheduler", "roles", "users"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["tickets", "store", "tasks", "notifications", "scheduler", "roles", "users"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
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
            TICKET_UPDATED,
            "surveys",
            Arc::new(move |ctx: &mut HookContext| on_ticket_updated(state.clone(), ctx)),
        );
        for event in [STORE_ORDER_COMPLETED, ORDER_PAID] {
            let state = self.state.clone();
            bus.register(
                event,
                "surveys",
                Arc::new(move |ctx: &mut HookContext| on_order_completed(state.clone(), ctx)),
            );
        }
        let state = self.state.clone();
        bus.register(
            SEND_REQUESTED,
            "surveys",
            Arc::new(move |ctx: &mut HookContext| on_send_requested(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/surveys")
                .app_data(state)
                .route("", web::get().to(list_surveys))
                .route("", web::post().to(create_survey))
                .route("/public/{token}", web::get().to(get_public_survey))
                .route("/public/{token}", web::post().to(submit_response))
                .route("/responses/{response_id}", web::get().to(get_response))
                .route("/distributions/{id}", web::get().to(get_distribution))
                .route("/distributions/{id}/send", web::post().to(send_distribution))
                .route("/distributions/{id}", web::delete().to(delete_distribution))
                .route("/{id}", web::get().to(get_survey))
                .route("/{id}", web::patch().to(update_survey))
                .route("/{id}", web::delete().to(delete_survey))
                .route("/{id}/publish", web::post().to(publish_survey))
                .route("/{id}/archive", web::post().to(archive_survey))
                .route("/{id}/questions", web::get().to(list_questions))
                .route("/{id}/questions", web::post().to(create_question))
                .route("/{id}/questions/{question_id}", web::get().to(get_question))
                .route("/{id}/questions/{question_id}", web::patch().to(update_question))
                .route("/{id}/questions/{question_id}", web::delete().to(delete_question))
                .route("/{id}/results", web::get().to(survey_results))
                .route("/{id}/responses", web::get().to(list_responses))
                .route("/{id}/distributions", web::get().to(list_distributions))
                .route("/{id}/distributions", web::post().to(create_distribution)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/surveys", owner: "surveys", description: "List surveys (paginated, filter by status, family_id, created_by; every version is a row)." },
            RouteInfo { method: "POST", path: "/api/v1/surveys", owner: "surveys", description: "Create a survey (draft, version 1)." },
            RouteInfo { method: "GET", path: "/api/v1/surveys/{id}", owner: "surveys", description: "Get one survey version with its questions." },
            RouteInfo { method: "PATCH", path: "/api/v1/surveys/{id}", owner: "surveys", description: "Update a draft in place; on a published survey creates a new draft version and returns it." },
            RouteInfo { method: "DELETE", path: "/api/v1/surveys/{id}", owner: "surveys", description: "Delete a draft survey version (with its questions and pending invitations)." },
            RouteInfo { method: "POST", path: "/api/v1/surveys/{id}/publish", owner: "surveys", description: "Publish a draft version (the previously published version of the same family is archived)." },
            RouteInfo { method: "POST", path: "/api/v1/surveys/{id}/archive", owner: "surveys", description: "Archive a survey version." },
            RouteInfo { method: "GET", path: "/api/v1/surveys/{id}/questions", owner: "surveys", description: "List questions of a survey version (position order)." },
            RouteInfo { method: "POST", path: "/api/v1/surveys/{id}/questions", owner: "surveys", description: "Add a question to a draft survey (nps, csat, rating, text, multiple_choice)." },
            RouteInfo { method: "GET", path: "/api/v1/surveys/{id}/questions/{question_id}", owner: "surveys", description: "Get one question." },
            RouteInfo { method: "PATCH", path: "/api/v1/surveys/{id}/questions/{question_id}", owner: "surveys", description: "Update a question of a draft survey." },
            RouteInfo { method: "DELETE", path: "/api/v1/surveys/{id}/questions/{question_id}", owner: "surveys", description: "Delete a question of a draft survey (only when no answers reference it)." },
            RouteInfo { method: "GET", path: "/api/v1/surveys/{id}/results", owner: "surveys", description: "NPS + CSAT scores, per-question stats, breakdowns by agent / department / product, and daily/weekly trends." },
            RouteInfo { method: "GET", path: "/api/v1/surveys/{id}/responses", owner: "surveys", description: "List responses of a survey version (paginated, filter by status, score band, agent_id, department_id, product_id)." },
            RouteInfo { method: "GET", path: "/api/v1/surveys/responses/{response_id}", owner: "surveys", description: "Get one response with its answers and follow-up link." },
            RouteInfo { method: "GET", path: "/api/v1/surveys/{id}/distributions", owner: "surveys", description: "List distributions of a survey version." },
            RouteInfo { method: "POST", path: "/api/v1/surveys/{id}/distributions", owner: "surveys", description: "Create a distribution (trigger manual, ticket_closed, purchase, or scheduled)." },
            RouteInfo { method: "GET", path: "/api/v1/surveys/distributions/{id}", owner: "surveys", description: "Get one distribution." },
            RouteInfo { method: "POST", path: "/api/v1/surveys/distributions/{id}/send", owner: "surveys", description: "Send a distribution's pending invitations now (manual / scheduled)." },
            RouteInfo { method: "DELETE", path: "/api/v1/surveys/distributions/{id}", owner: "surveys", description: "Delete a pending distribution (with its pending invitations)." },
            RouteInfo { method: "GET", path: "/api/v1/surveys/public/{token}", owner: "surveys", description: "PUBLIC: the survey + questions for one response token (no auth; gated by the API Manager ApiAccessRule for this path)." },
            RouteInfo { method: "POST", path: "/api/v1/surveys/public/{token}", owner: "surveys", description: "PUBLIC: submit answers for one response token (no auth; gated by the API Manager ApiAccessRule for this path)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SurveysModule::new()));
}

// ---------------------------------------------------------------------------
// Survey handlers
// ---------------------------------------------------------------------------

fn survey_view(state: &SurveysState, survey: &Survey) -> Value {
    let mut value = serde_json::to_value(survey).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert(
            "questions".to_string(),
            serde_json::to_value(questions_for(state, survey.id)).unwrap_or(Value::Null),
        );
        let versions: Vec<u32> = state
            .surveys
            .iter()
            .filter(|entry| entry.value().family_id == survey.family_id)
            .map(|entry| entry.value().version)
            .collect();
        map.insert(
            "family_versions".to_string(),
            serde_json::to_value(versions).unwrap_or(Value::Null),
        );
    }
    value
}

async fn list_surveys(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let family_filter = params.filters.get("family_id").cloned();
        let creator_filter = params.filters.get("created_by").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .surveys
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|survey| {
                if let Some(status) = &status_filter {
                    if survey.status != *status {
                        return false;
                    }
                }
                if let Some(family) = &family_filter {
                    if survey.family_id.to_string() != *family {
                        return false;
                    }
                }
                if let Some(creator) = &creator_filter {
                    if survey.created_by != *creator {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        survey.title,
                        survey.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|survey| survey_view(&state, &survey))
            .collect();
        let sort = params.allowed_sort(&["title", "created_at", "version"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a
                    .get("title")
                    .and_then(Value::as_str)
                    .cmp(&b.get("title").and_then(Value::as_str)),
                "version" => a
                    .get("version")
                    .and_then(Value::as_u64)
                    .cmp(&b.get("version").and_then(Value::as_u64)),
                _ => a
                    .get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("created_at").and_then(Value::as_str)),
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

async fn create_survey(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    body: web::Json<CreateSurveyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let now = Utc::now();
        let id = Uuid::now_v7();
        let survey = Survey {
            id,
            family_id: id,
            version: 1,
            supersedes: None,
            title,
            description,
            status: "draft".to_string(),
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.surveys.insert(id, survey.clone());
        state.emit(
            "surveys.created",
            json!({
                "survey_id": id.to_string(),
                "family_id": id.to_string(),
                "version": 1,
                "title": survey.title,
                "created_by": caller.actor,
            }),
        );
        Ok(survey_view(&state, &survey))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_survey(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_VIEW)?;
        let survey = load_survey(&state, &path)?;
        Ok(survey_view(&state, &survey))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a survey. A draft is edited in place. A published (or
/// archived) survey is never edited in place: the update creates a
/// new draft version in the same family (questions copied), and
/// the new version is returned — responses keep referencing the
/// version they answered.
async fn update_survey(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSurveyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let input = body.into_inner();
        let survey = load_survey(&state, &path)?;
        let title = match &input.title {
            Some(raw) => Some(require_text(raw, "title", MAX_TITLE_LEN)?),
            None => None,
        };
        let description = match &input.description {
            Some(inner) => match inner {
                Some(raw) => Some(Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?)),
                None => Some(None),
            },
            None => None,
        };
        if survey.status == "draft" {
            let mut updated = survey.clone();
            if let Some(title) = &title {
                updated.title = title.clone();
            }
            if let Some(description) = &description {
                updated.description = description.clone();
            }
            updated.updated_at = Utc::now();
            state.surveys.insert(updated.id, updated.clone());
            state.emit(
                "surveys.updated",
                json!({
                    "survey_id": updated.id.to_string(),
                    "version": updated.version,
                    "updated_by": caller.actor,
                }),
            );
            return Ok(survey_view(&state, &updated));
        }
        let now = Utc::now();
        let next_version = state
            .surveys
            .iter()
            .filter(|entry| entry.value().family_id == survey.family_id)
            .map(|entry| entry.value().version)
            .max()
            .unwrap_or(survey.version)
            + 1;
        let new_id = Uuid::now_v7();
        let new_survey = Survey {
            id: new_id,
            family_id: survey.family_id,
            version: next_version,
            supersedes: Some(survey.id),
            title: title.unwrap_or_else(|| survey.title.clone()),
            description: description.unwrap_or_else(|| survey.description.clone()),
            status: "draft".to_string(),
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.surveys.insert(new_id, new_survey.clone());
        for question in questions_for(&state, survey.id) {
            let copy = SurveyQuestion {
                id: Uuid::now_v7(),
                survey_id: new_id,
                position: question.position,
                question_type: question.question_type,
                text: question.text,
                required: question.required,
                options: question.options,
                min_value: question.min_value,
                max_value: question.max_value,
                created_at: now,
                updated_at: now,
            };
            state.questions.insert(copy.id, copy);
        }
        state.emit(
            "surveys.version_created",
            json!({
                "survey_id": new_id.to_string(),
                "family_id": survey.family_id.to_string(),
                "version": next_version,
                "supersedes": survey.id.to_string(),
                "created_by": caller.actor,
            }),
        );
        Ok(survey_view(&state, &new_survey))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_survey(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let survey = load_survey(&state, &path)?;
        if survey.status != "draft" {
            return Err(UnifiedError::conflict(
                "Only a draft survey version can be deleted; archive a published version instead",
            ));
        }
        if state
            .responses
            .iter()
            .any(|entry| entry.value().survey_id == survey.id && entry.value().submitted_at.is_some())
        {
            return Err(UnifiedError::conflict(
                "This survey version already has submitted responses and cannot be deleted",
            ));
        }
        let question_ids: Vec<Uuid> = questions_for(&state, survey.id)
            .iter()
            .map(|question| question.id)
            .collect();
        for id in question_ids {
            state.questions.remove(&id);
        }
        let response_ids: Vec<Uuid> = state
            .responses
            .iter()
            .filter(|entry| entry.value().survey_id == survey.id)
            .map(|entry| *entry.key())
            .collect();
        for id in response_ids {
            state.responses.remove(&id);
        }
        let distribution_ids: Vec<Uuid> = state
            .distributions
            .iter()
            .filter(|entry| entry.value().survey_id == survey.id)
            .map(|entry| *entry.key())
            .collect();
        for id in distribution_ids {
            state.distributions.remove(&id);
            state.pending_recipients.remove(&id);
        }
        state.surveys.remove(&survey.id);
        state.emit(
            "surveys.deleted",
            json!({
                "survey_id": survey.id.to_string(),
                "family_id": survey.family_id.to_string(),
                "version": survey.version,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "id": survey.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn publish_survey(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let mut survey = load_survey(&state, &path)?;
        if survey.status != "draft" {
            return Err(UnifiedError::conflict(format!(
                "Only a draft survey can be published (current status: {})",
                survey.status
            )));
        }
        if questions_for(&state, survey.id).is_empty() {
            return Err(UnifiedError::conflict(
                "A survey with no questions cannot be published",
            ));
        }
        let now = Utc::now();
        let mut archived: Vec<String> = Vec::new();
        for mut entry in state.surveys.iter_mut() {
            if entry.value().family_id == survey.family_id
                && entry.value().status == "published"
                && entry.value().id != survey.id
            {
                entry.value_mut().status = "archived".to_string();
                entry.value_mut().updated_at = now;
                archived.push(entry.value().id.to_string());
            }
        }
        survey.status = "published".to_string();
        survey.updated_at = now;
        state.surveys.insert(survey.id, survey.clone());
        state.emit(
            "surveys.published",
            json!({
                "survey_id": survey.id.to_string(),
                "family_id": survey.family_id.to_string(),
                "version": survey.version,
                "archived_versions": archived,
                "published_by": caller.actor,
            }),
        );
        Ok(survey_view(&state, &survey))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn archive_survey(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let mut survey = load_survey(&state, &path)?;
        if survey.status == "archived" {
            return Err(UnifiedError::conflict("This survey version is already archived"));
        }
        survey.status = "archived".to_string();
        survey.updated_at = Utc::now();
        state.surveys.insert(survey.id, survey.clone());
        state.emit(
            "surveys.archived",
            json!({
                "survey_id": survey.id.to_string(),
                "family_id": survey.family_id.to_string(),
                "version": survey.version,
                "archived_by": caller.actor,
            }),
        );
        Ok(survey_view(&state, &survey))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Question handlers
// ---------------------------------------------------------------------------

fn validate_options(question_type: &str, options: &[String]) -> UnifiedResult<Vec<String>> {
    if question_type == "multiple_choice" {
        if options.len() < 2 {
            return Err(UnifiedError::bad_request(
                "A multiple choice question needs at least 2 options",
            ));
        }
    } else if !options.is_empty() {
        return Err(UnifiedError::bad_request(
            "options only apply to multiple_choice questions",
        ));
    }
    if options.len() > MAX_OPTIONS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_OPTIONS} options are allowed"
        )));
    }
    let mut cleaned: Vec<String> = Vec::new();
    for raw in options {
        cleaned.push(require_text(raw, "option", MAX_OPTION_LEN)?);
    }
    Ok(cleaned)
}

async fn list_questions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_VIEW)?;
        let survey = load_survey(&state, &path)?;
        let params = query.into_inner();
        Ok(paginate(questions_for(&state, survey.id), &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateQuestionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SurveyQuestion> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let survey = load_survey(&state, &path)?;
        if survey.status != "draft" {
            return Err(UnifiedError::conflict(
                "Questions can only be added to a draft survey; editing a published survey creates a new version",
            ));
        }
        let input = body.into_inner();
        let question_type = valid_question_type(&input.question_type)?;
        let text = require_text(&input.text, "text", MAX_QUESTION_LEN)?;
        let options = validate_options(&question_type, &input.options)?;
        let (min_value, max_value) = score_bounds(&question_type);
        let position = questions_for(&state, survey.id)
            .iter()
            .map(|question| question.position)
            .max()
            .unwrap_or(0)
            + 1;
        let now = Utc::now();
        let question = SurveyQuestion {
            id: Uuid::now_v7(),
            survey_id: survey.id,
            position,
            question_type: question_type.clone(),
            text,
            required: input.required,
            options,
            min_value,
            max_value,
            created_at: now,
            updated_at: now,
        };
        state.questions.insert(question.id, question.clone());
        state.emit(
            "surveys.question.created",
            json!({
                "survey_id": survey.id.to_string(),
                "question_id": question.id.to_string(),
                "question_type": question_type,
                "created_by": caller.actor,
            }),
        );
        Ok(question)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SurveyQuestion> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_VIEW)?;
        let (survey_id, question_id) = path.into_inner();
        let survey = load_survey(&state, &survey_id)?;
        load_question(&state, survey.id, &question_id)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateQuestionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SurveyQuestion> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let (survey_id, question_id) = path.into_inner();
        let survey = load_survey(&state, &survey_id)?;
        if survey.status != "draft" {
            return Err(UnifiedError::conflict(
                "Questions can only be updated on a draft survey; editing a published survey creates a new version",
            ));
        }
        let input = body.into_inner();
        let mut question = load_question(&state, survey.id, &question_id)?;
        if let Some(text) = &input.text {
            question.text = require_text(text, "text", MAX_QUESTION_LEN)?;
        }
        if let Some(required) = input.required {
            question.required = required;
        }
        if let Some(options) = &input.options {
            question.options = validate_options(&question.question_type, options)?;
        }
        question.updated_at = Utc::now();
        state.questions.insert(question.id, question.clone());
        state.emit(
            "surveys.question.updated",
            json!({
                "survey_id": survey.id.to_string(),
                "question_id": question.id.to_string(),
                "updated_by": caller.actor,
            }),
        );
        Ok(question)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let (survey_id, question_id) = path.into_inner();
        let survey = load_survey(&state, &survey_id)?;
        if survey.status != "draft" {
            return Err(UnifiedError::conflict(
                "Questions can only be deleted from a draft survey",
            ));
        }
        let question = load_question(&state, survey.id, &question_id)?;
        if state
            .answers
            .iter()
            .any(|entry| entry.value().question_id == question.id)
        {
            return Err(UnifiedError::conflict(
                "This question already has answers and cannot be deleted",
            ));
        }
        state.questions.remove(&question.id);
        state.emit(
            "surveys.question.deleted",
            json!({
                "survey_id": survey.id.to_string(),
                "question_id": question.id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "id": question.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Results and response handlers
// ---------------------------------------------------------------------------

fn parse_rfc3339(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

async fn survey_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_VIEW)?;
        let survey = load_survey(&state, &path)?;
        let params = query.into_inner();
        let bucket = params
            .filters
            .get("bucket")
            .cloned()
            .unwrap_or_else(|| "daily".to_string());
        if bucket != "daily" && bucket != "weekly" {
            return Err(UnifiedError::bad_request("bucket must be daily or weekly"));
        }
        let from = params.filters.get("from").and_then(|raw| parse_rfc3339(raw));
        let to = params.filters.get("to").and_then(|raw| parse_rfc3339(raw));
        let agent_filter = params.filters.get("agent_id").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let product_filter = params.filters.get("product_id").cloned();
        let mut responses = responses_for_survey(&state, survey.id);
        responses.retain(|response| {
            if let Some(agent) = &agent_filter {
                if response.agent_id.as_deref() != Some(agent.as_str()) {
                    return false;
                }
            }
            if let Some(department) = &department_filter {
                if response.department_id.as_deref() != Some(department.as_str()) {
                    return false;
                }
            }
            if let Some(product) = &product_filter {
                if response.product_id.as_deref() != Some(product.as_str()) {
                    return false;
                }
            }
            if let Some(submitted_at) = response.submitted_at {
                if let Some(from) = from {
                    if submitted_at < from {
                        return false;
                    }
                }
                if let Some(to) = to {
                    if submitted_at > to {
                        return false;
                    }
                }
            } else if from.is_some() || to.is_some() {
                return false;
            }
            true
        });
        let submitted = responses.iter().filter(|response| response.submitted_at.is_some()).count() as u64;
        let questions = questions_for(&state, survey.id);
        let per_question: Vec<Value> = questions
            .iter()
            .map(|question| question_stats(&state, question, &responses))
            .collect();
        // Overall NPS / CSAT across all scored questions of each
        // type (computed on read, never stored).
        let overall = |question_type: &str, score_fn: &dyn Fn(&[i32]) -> Option<f64>| -> Value {
            let mut scores: Vec<i32> = Vec::new();
            for question in questions.iter().filter(|question| question.question_type == question_type) {
                for response in responses.iter().filter(|response| response.submitted_at.is_some()) {
                    for answer in answers_for_response(&state, response.id) {
                        if answer.question_id == question.id {
                            if let Some(score) = answer.score {
                                scores.push(score);
                            }
                        }
                    }
                }
            }
            match score_fn(&scores) {
                Some(value) => json!(value),
                None => Value::Null,
            }
        };
        let nps_value = overall("nps", &|scores: &[i32]| {
            if scores.is_empty() {
                return None;
            }
            let total = scores.len() as f64;
            let promoters = scores.iter().filter(|score| **score >= NPS_PROMOTER_MIN).count() as f64;
            let detractors = scores.iter().filter(|score| **score <= NPS_DETRACTOR_MAX).count() as f64;
            Some(((promoters - detractors) / total * 100.0).round())
        });
        let csat_value = overall("csat", &|scores: &[i32]| {
            if scores.is_empty() {
                return None;
            }
            let total = scores.len() as f64;
            let satisfied = scores.iter().filter(|score| **score >= CSAT_SATISFIED_MIN).count() as f64;
            Some((satisfied / total * 100.0).round())
        });
        // Breakdowns by agent, department, and product: submitted
        // count plus NPS / CSAT within each group.
        let breakdown = |key_fn: &dyn Fn(&SurveyResponse) -> Option<String>| -> Value {
            let mut groups: HashMap<String, Vec<Uuid>> = HashMap::new();
            for response in responses.iter().filter(|response| response.submitted_at.is_some()) {
                if let Some(key) = key_fn(response) {
                    groups.entry(key).or_default().push(response.id);
                }
            }
            let mut keys: Vec<String> = groups.keys().cloned().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                let response_ids = groups.remove(&key).unwrap_or_default();
                let mut nps_scores: Vec<i32> = Vec::new();
                let mut csat_scores: Vec<i32> = Vec::new();
                for response_id in &response_ids {
                    for answer in answers_for_response(&state, *response_id) {
                        let Some(question) = state.questions.get(&answer.question_id) else {
                            continue;
                        };
                        let Some(score) = answer.score else {
                            continue;
                        };
                        if question.question_type == "nps" {
                            nps_scores.push(score);
                        } else if question.question_type == "csat" {
                            csat_scores.push(score);
                        }
                    }
                }
                let nps = if nps_scores.is_empty() {
                    Value::Null
                } else {
                    let total = nps_scores.len() as f64;
                    let promoters = nps_scores.iter().filter(|score| **score >= NPS_PROMOTER_MIN).count() as f64;
                    let detractors = nps_scores.iter().filter(|score| **score <= NPS_DETRACTOR_MAX).count() as f64;
                    json!(((promoters - detractors) / total * 100.0).round())
                };
                let csat = if csat_scores.is_empty() {
                    Value::Null
                } else {
                    let total = csat_scores.len() as f64;
                    let satisfied = csat_scores.iter().filter(|score| **score >= CSAT_SATISFIED_MIN).count() as f64;
                    json!((satisfied / total * 100.0).round())
                };
                out.insert(
                    key,
                    json!({
                        "responses": response_ids.len() as u64,
                        "nps": nps,
                        "csat": csat,
                    }),
                );
            }
            Value::Object(out)
        };
        Ok(json!({
            "survey_id": survey.id.to_string(),
            "family_id": survey.family_id.to_string(),
            "version": survey.version,
            "invited": responses.len() as u64,
            "submitted": submitted,
            "response_rate": if responses.is_empty() { Value::Null } else { json!(((submitted as f64 / responses.len() as f64) * 100.0).round()) },
            "nps": nps_value,
            "csat": csat_value,
            "questions": per_question,
            "by_agent": breakdown(&|response| response.agent_id.clone()),
            "by_department": breakdown(&|response| response.department_id.clone()),
            "by_product": breakdown(&|response| response.product_id.clone()),
            "trends": trend_buckets(&state, &responses, &bucket),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn response_view(state: &SurveysState, response: &SurveyResponse) -> Value {
    let mut value = serde_json::to_value(response).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        // The token is never returned in admin views: it is the
        // bearer credential for the public response link.
        map.remove("token");
        map.insert(
            "answers".to_string(),
            serde_json::to_value(answers_for_response(state, response.id)).unwrap_or(Value::Null),
        );
    }
    value
}

async fn list_responses(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_VIEW)?;
        let survey = load_survey(&state, &path)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let score_band = params.filters.get("score_band").cloned();
        let agent_filter = params.filters.get("agent_id").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let product_filter = params.filters.get("product_id").cloned();
        let mut items: Vec<Value> = responses_for_survey(&state, survey.id)
            .into_iter()
            .filter(|response| {
                if let Some(status) = &status_filter {
                    let submitted = response.submitted_at.is_some();
                    match status.as_str() {
                        "submitted" if !submitted => return false,
                        "pending" if submitted => return false,
                        _ => {}
                    }
                }
                if let Some(agent) = &agent_filter {
                    if response.agent_id.as_deref() != Some(agent.as_str()) {
                        return false;
                    }
                }
                if let Some(department) = &department_filter {
                    if response.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if let Some(product) = &product_filter {
                    if response.product_id.as_deref() != Some(product.as_str()) {
                        return false;
                    }
                }
                if let Some(band) = &score_band {
                    let scores: Vec<i32> = answers_for_response(&state, response.id)
                        .iter()
                        .filter_map(|answer| answer.score)
                        .collect();
                    if scores.is_empty() {
                        return false;
                    }
                    let matches = match band.as_str() {
                        "promoter" => scores.iter().any(|score| *score >= NPS_PROMOTER_MIN),
                        "detractor" => scores.iter().any(|score| *score <= NPS_DETRACTOR_MAX),
                        "satisfied" => scores.iter().any(|score| *score >= CSAT_SATISFIED_MIN),
                        "dissatisfied" => scores.iter().any(|score| *score <= CSAT_FOLLOWUP_MAX),
                        _ => true,
                    };
                    if !matches {
                        return false;
                    }
                }
                true
            })
            .map(|response| response_view(&state, &response))
            .collect();
        items.sort_by(|a, b| {
            b.get("created_at")
                .and_then(Value::as_str)
                .cmp(&a.get("created_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_response(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_VIEW)?;
        let response = load_response(&state, &path)?;
        Ok(response_view(&state, &response))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Distribution handlers
// ---------------------------------------------------------------------------

async fn list_distributions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_VIEW)?;
        let survey = load_survey(&state, &path)?;
        let params = query.into_inner();
        let trigger_filter = params.filters.get("trigger").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<SurveyDistribution> = state
            .distributions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|distribution| {
                if distribution.survey_id != survey.id {
                    return false;
                }
                if let Some(trigger) = &trigger_filter {
                    if distribution.trigger != *trigger {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if distribution.status != *status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|distribution| distribution.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_distribution(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateDistributionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SurveyDistribution> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let survey = load_survey(&state, &path)?;
        if survey.status != "published" {
            return Err(UnifiedError::conflict(
                "Distributions can only be created for a published survey version",
            ));
        }
        let input = body.into_inner();
        let trigger = input.trigger.trim().to_lowercase();
        if !TRIGGERS.contains(&trigger.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "trigger must be one of: {}",
                TRIGGERS.join(", ")
            )));
        }
        if trigger == "scheduled" && input.scheduled_at.is_none() {
            return Err(UnifiedError::bad_request(
                "A scheduled distribution needs scheduled_at",
            ));
        }
        if let Some(scheduled_at) = input.scheduled_at {
            if scheduled_at <= Utc::now() {
                return Err(UnifiedError::bad_request("scheduled_at must be in the future"));
            }
        }
        if input.recipients.len() > MAX_RECIPIENTS {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_RECIPIENTS} recipients are allowed per distribution"
            )));
        }
        for recipient in &input.recipients {
            if recipient.recipient_id.trim().is_empty() || recipient.recipient_id.len() > MAX_ID_LEN {
                return Err(UnifiedError::bad_request(
                    "Every recipient needs a non-empty recipient_id (max 255 characters)",
                ));
            }
            if let Some(email) = &recipient.recipient_email {
                if !valid_email(email) {
                    return Err(UnifiedError::bad_request(format!("Invalid recipient email: {email}")));
                }
            }
        }
        let expires_in_days = match input.expires_in_days {
            Some(days) if (1..=365).contains(&days) => days,
            Some(_) => {
                return Err(UnifiedError::bad_request(
                    "expires_in_days must be between 1 and 365",
                ))
            }
            None => DEFAULT_TOKEN_DAYS,
        };
        let now = Utc::now();
        let mut distribution = SurveyDistribution {
            id: Uuid::now_v7(),
            survey_id: survey.id,
            trigger: trigger.clone(),
            department_id: optional_text(&input.department_id, "department_id", MAX_ID_LEN)?,
            product_id: optional_text(&input.product_id, "product_id", MAX_ID_LEN)?,
            agent_id: optional_text(&input.agent_id, "agent_id", MAX_ID_LEN)?,
            responsible_user_id: optional_text(
                &input.responsible_user_id,
                "responsible_user_id",
                MAX_ID_LEN,
            )?,
            scheduled_at: input.scheduled_at,
            expires_in_days,
            status: "pending".to_string(),
            scheduler_status: "not_scheduled".to_string(),
            scheduler_job_id: None,
            sent_count: 0,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        if !input.recipients.is_empty() {
            state
                .pending_recipients
                .insert(distribution.id, input.recipients);
        }
        if trigger == "scheduled" {
            schedule_distribution(&state, &mut distribution);
        }
        state.distributions.insert(distribution.id, distribution.clone());
        state.emit(
            "surveys.distribution.created",
            json!({
                "survey_id": survey.id.to_string(),
                "distribution_id": distribution.id.to_string(),
                "trigger": trigger,
                "scheduler_status": distribution.scheduler_status,
                "created_by": caller.actor,
            }),
        );
        Ok(distribution)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_distribution(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SurveyDistribution> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_VIEW)?;
        load_distribution(&state, &path)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn send_distribution(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SendDistributionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let mut distribution = load_distribution(&state, &path)?;
        if distribution.status == "cancelled" {
            return Err(UnifiedError::conflict("A cancelled distribution cannot be sent"));
        }
        if distribution.trigger == "ticket_closed" || distribution.trigger == "purchase" {
            return Err(UnifiedError::conflict(
                "Trigger distributions send through their trigger events, not through a manual send",
            ));
        }
        let input = body.into_inner();
        if input.recipients.len() > MAX_RECIPIENTS {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_RECIPIENTS} recipients are allowed per send"
            )));
        }
        for recipient in &input.recipients {
            if recipient.recipient_id.trim().is_empty() || recipient.recipient_id.len() > MAX_ID_LEN {
                return Err(UnifiedError::bad_request(
                    "Every recipient needs a non-empty recipient_id (max 255 characters)",
                ));
            }
            if let Some(email) = &recipient.recipient_email {
                if !valid_email(email) {
                    return Err(UnifiedError::bad_request(format!("Invalid recipient email: {email}")));
                }
            }
        }
        let issued = send_distribution_invitations(&state, &mut distribution, input.recipients)?;
        Ok(json!({
            "distribution_id": distribution.id.to_string(),
            "issued": issued,
            "sent_count": distribution.sent_count,
            "status": distribution.status,
            "sent_by": caller.actor,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_distribution(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let distribution = load_distribution(&state, &path)?;
        if distribution.status != "pending" {
            return Err(UnifiedError::conflict(
                "Only a pending distribution can be deleted",
            ));
        }
        let pending_ids: Vec<Uuid> = state
            .responses
            .iter()
            .filter(|entry| {
                entry.value().distribution_id == Some(distribution.id)
                    && entry.value().submitted_at.is_none()
            })
            .map(|entry| *entry.key())
            .collect();
        for id in pending_ids {
            state.responses.remove(&id);
        }
        state.pending_recipients.remove(&distribution.id);
        state.distributions.remove(&distribution.id);
        Ok(json!({ "deleted": true, "id": distribution.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Public response handlers (token-gated; see the public-access note)
// ---------------------------------------------------------------------------

fn find_response_by_token(state: &SurveysState, token: &str) -> UnifiedResult<SurveyResponse> {
    let token = token.trim();
    if token.is_empty() || token.len() > MAX_ID_LEN {
        return Err(UnifiedError::bad_request("Invalid response token"));
    }
    state
        .responses
        .iter()
        .map(|entry| entry.value().clone())
        .find(|response| response.token == token)
        .ok_or_else(|| UnifiedError::not_found("Response not found for this token"))
}

async fn get_public_survey(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let response = find_response_by_token(&state, &path)?;
        if response.submitted_at.is_some() {
            return Err(UnifiedError::conflict("This response was already submitted"));
        }
        if response.token_expires_at < Utc::now() {
            return Err(UnifiedError::bad_request("This response link has expired"));
        }
        let survey = state
            .surveys
            .get(&response.survey_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Survey not found: {}", response.survey_id)))?;
        Ok(json!({
            "survey_id": survey.id.to_string(),
            "title": survey.title,
            "description": survey.description,
            "version": survey.version,
            "token_expires_at": response.token_expires_at.to_rfc3339(),
            "questions": questions_for(&state, survey.id),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn submit_response(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    path: web::Path<String>,
    body: web::Json<SubmitResponseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut response = find_response_by_token(&state, &path)?;
        if response.submitted_at.is_some() {
            return Err(UnifiedError::conflict("This response was already submitted"));
        }
        if response.token_expires_at < Utc::now() {
            return Err(UnifiedError::bad_request("This response link has expired"));
        }
        let survey = state
            .surveys
            .get(&response.survey_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Survey not found: {}", response.survey_id)))?;
        if survey.status == "archived" {
            return Err(UnifiedError::conflict(
                "This survey version is archived and no longer accepts responses",
            ));
        }
        let questions = questions_for(&state, survey.id);
        if questions.is_empty() {
            return Err(UnifiedError::conflict("This survey has no questions"));
        }
        let input = body.into_inner();
        if input.answers.is_empty() {
            return Err(UnifiedError::bad_request("At least one answer is required"));
        }
        if input.answers.len() > questions.len() {
            return Err(UnifiedError::bad_request(
                "More answers were given than the survey has questions",
            ));
        }
        let mut seen: Vec<Uuid> = Vec::new();
        let mut validated: Vec<SurveyAnswer> = Vec::new();
        let now = Utc::now();
        for submitted in &input.answers {
            if seen.contains(&submitted.question_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Duplicate answer for question {}",
                    submitted.question_id
                )));
            }
            seen.push(submitted.question_id);
            let Some(question) = questions.iter().find(|item| item.id == submitted.question_id) else {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown question: {}",
                    submitted.question_id
                )));
            };
            let mut answer = SurveyAnswer {
                id: Uuid::now_v7(),
                response_id: response.id,
                question_id: question.id,
                score: None,
                text: None,
                choice: None,
                created_at: now,
            };
            match question.question_type.as_str() {
                "nps" | "csat" | "rating" => {
                    let Some(score) = submitted.score else {
                        return Err(UnifiedError::bad_request(format!(
                            "Question \"{}\" needs a score",
                            question.text
                        )));
                    };
                    let (min, max) = score_bounds(&question.question_type);
                    if let (Some(min), Some(max)) = (min, max) {
                        if score < min || score > max {
                            return Err(UnifiedError::bad_request(format!(
                                "Score for \"{}\" must be between {min} and {max}",
                                question.text
                            )));
                        }
                    }
                    answer.score = Some(score);
                }
                "text" => {
                    let Some(raw) = &submitted.text else {
                        return Err(UnifiedError::bad_request(format!(
                            "Question \"{}\" needs a text answer",
                            question.text
                        )));
                    };
                    answer.text = Some(require_text(raw, "answer", MAX_TEXT_LEN)?);
                }
                "multiple_choice" => {
                    let Some(choice) = &submitted.choice else {
                        return Err(UnifiedError::bad_request(format!(
                            "Question \"{}\" needs a choice",
                            question.text
                        )));
                    };
                    let choice = require_text(choice, "choice", MAX_OPTION_LEN)?;
                    if !question.options.contains(&choice) {
                        return Err(UnifiedError::bad_request(format!(
                            "Choice \"{choice}\" is not one of the options for \"{}\"",
                            question.text
                        )));
                    }
                    answer.choice = Some(choice);
                }
                _ => {
                    return Err(UnifiedError::bad_request(format!(
                        "Unsupported question type: {}",
                        question.question_type
                    )));
                }
            }
            validated.push(answer);
        }
        for question in &questions {
            if question.required && !seen.contains(&question.id) {
                return Err(UnifiedError::bad_request(format!(
                    "Required question \"{}\" was not answered",
                    question.text
                )));
            }
        }
        for answer in validated {
            state.answers.insert(answer.id, answer);
        }
        response.submitted_at = Some(now);
        response.updated_at = now;
        state.responses.insert(response.id, response.clone());
        state.emit(
            "surveys.response.received",
            json!({
                "survey_id": survey.id.to_string(),
                "response_id": response.id.to_string(),
                "recipient_id": response.recipient_id,
                "submitted_at": now.to_rfc3339(),
            }),
        );
        maybe_create_followup(&state, &survey, &mut response);
        Ok(json!({
            "submitted": true,
            "response_id": response.id.to_string(),
            "survey_id": survey.id.to_string(),
            "follow_up_status": response.follow_up_status,
            "follow_up_task_id": response.follow_up_task_id,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
