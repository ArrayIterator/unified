// Scheduler / Queue module.
//
// Named jobs with an interval or cron-like schedule, run history, and a
// background queue with retries and backoff. Job kinds dispatch to real
// handlers that emit the corresponding hook (`mail.send_requested`,
// `geo.update_requested`, `ssl.renew`, `cleanup.requested`) and record a
// run result. Unknown kinds fail honestly with 422.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use tokio::time::{sleep, Duration};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub name: String,
    /// `mail.send`, `geo.update`, `ssl.renew`, `cleanup`,
    /// `backup.run`, `cluster.health`, `cluster.sync`,
    /// `payment.subscription.renew`, `report.generate`,
    /// `newsletter.send`, `notification.digest`,
    /// `notification.digest.send`, `meeting.start`,
    /// `announcement.publish_due`, `ticket.sla.check`.
    pub kind: String,
    /// Interval in seconds (`"300"`) or a cron-like expression
    /// (`"*/5 * * * *"`).
    pub schedule: String,
    pub payload: Value,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateJob {
    pub name: String,
    pub kind: String,
    pub schedule: String,
    pub payload: Option<Value>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateJob {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub schedule: Option<String>,
    pub payload: Option<Value>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRun {
    pub id: String,
    pub job_id: String,
    pub kind: String,
    /// `success` or `failed`.
    pub status: String,
    pub result: Value,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: String,
    pub kind: String,
    pub payload: Value,
    /// `queued`, `processing`, `done`, `failed`.
    pub status: String,
    pub attempts: u32,
    pub max_attempts: u32,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EnqueueRequest {
    pub kind: String,
    pub payload: Option<Value>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct SchedulerState {
    hooks: RwLock<Arc<HookBus>>,
    jobs: DashMap<String, Job>,
    runs: DashMap<String, JobRun>,
    queue: DashMap<String, QueueItem>,
    /// Last dispatch time per subscription for the
    /// `payment.subscription.renew` loop guard (see `dispatch`).
    renew_guard: DashMap<String, std::time::Instant>,
}

impl SchedulerState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            jobs: DashMap::new(),
            runs: DashMap::new(),
            queue: DashMap::new(),
            renew_guard: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
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

/// Known job kinds. Anything else is rejected with 422 when it is run.
const KNOWN_KINDS: &[&str] = &[
    "mail.send",
    "geo.update",
    "ssl.renew",
    "cleanup",
    "backup.run",
    "cluster.health",
    "cluster.sync",
    "payment.subscription.renew",
    "report.generate",
    "newsletter.send",
    "notification.digest",
    "notification.digest.send",
    "meeting.start",
    "announcement.publish_due",
    "ticket.sla.check",
];

/// Known job kinds as a comma-separated list for error messages.
fn known_kinds_list() -> String {
    KNOWN_KINDS.join(", ")
}

fn valid_kind(kind: &str) -> bool {
    KNOWN_KINDS.contains(&kind)
}

/// A schedule is either an interval in seconds or five cron-like fields
/// where each field is `*`, `*/N`, or a plain number.
fn valid_schedule(schedule: &str) -> bool {
    let schedule = schedule.trim();
    if schedule.is_empty() {
        return false;
    }
    if schedule.parse::<u64>().is_ok() {
        return true;
    }
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    fields.iter().all(|field| {
        if *field == "*" {
            return true;
        }
        if let Some(step) = field.strip_prefix("*/") {
            return step.parse::<u32>().is_ok_and(|n| n > 0);
        }
        field.parse::<u32>().is_ok()
    })
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

/// Dispatch one unit of work by kind. This is the real handler for both
/// scheduled jobs and queued items.
fn dispatch(state: &Arc<SchedulerState>, kind: &str, payload: &Value) -> UnifiedResult<Value> {
    match kind {
        "mail.send" => {
            // A payload carrying a `job_id` is a mail job that is already
            // queued in the Mail module (it arrived via `scheduler.enqueue`
            // from `mail.send_requested`): deliver that job instead of
            // re-requesting it, otherwise the mail would loop forever
            // (send_requested -> enqueue -> send_requested -> ...).
            if let Some(job_id) = payload.get("job_id").and_then(|value| value.as_str()) {
                state.emit("mail.deliver", json!({ "job_id": job_id }));
                return Ok(json!({ "dispatched": true, "event": "mail.deliver", "job_id": job_id }));
            }
            state.emit("mail.send_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "mail.send_requested" }))
        }
        "geo.update" => {
            state.emit("geo.update_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "geo.update_requested" }))
        }
        "ssl.renew" => {
            state.emit(events::SSL_RENEW, payload.clone());
            Ok(json!({ "dispatched": true, "event": events::SSL_RENEW }))
        }
        "cleanup" => {
            // Real cleanup: drop finished queue items older than 7 days.
            let cutoff = now() - chrono::Duration::days(7);
            let mut removed = 0u64;
            let stale: Vec<String> = state
                .queue
                .iter()
                .filter(|entry| {
                    (entry.value().status == "done" || entry.value().status == "failed")
                        && entry.value().updated_at < cutoff
                })
                .map(|entry| entry.key().clone())
                .collect();
            for id in stale {
                state.queue.remove(&id);
                removed += 1;
            }
            state.emit("cleanup.requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "cleanup.requested", "removed_queue_items": removed }))
        }
        "backup.run" => {
            // The Backup module listens to `backup.run_requested` and
            // runs the job named by `backup_job_id` (or `job_id`) in
            // the payload. The payload is forwarded unchanged.
            state.emit("backup.run_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "backup.run_requested" }))
        }
        "cluster.health" => {
            // The Clusters module listens to
            // `cluster.health.check_requested` and probes the node
            // named by `node_id` in the payload.
            state.emit("cluster.health.check_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "cluster.health.check_requested" }))
        }
        "cluster.sync" => {
            // The Clusters module listens to `cluster.sync.requested`
            // and runs the sync plan named by `plan_id` in the
            // payload (`dry_run` selects a preview run).
            state.emit("cluster.sync.requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "cluster.sync.requested" }))
        }
        "payment.subscription.renew" => {
            // The Payments module listens to
            // `payment.subscription.renew_requested` and charges the
            // subscription named by `subscription_id` in the payload.
            //
            // Loop guard: that handler answers a renewal request by
            // asking the scheduler to run it again (it re-emits
            // `scheduler.enqueue` with this same kind), so emitting
            // the event unconditionally would spawn an unbounded
            // chain of queue items — one per dispatch, forever — and
            // never charge anything. Two guards break the chain:
            // the emitted event carries `via_scheduler: true` so the
            // handler can tell a scheduler-dispatched request apart
            // from an operator request (the handler must check it
            // before re-enqueuing), and this scheduler suppresses a
            // repeat dispatch for the same subscription within 30
            // seconds. Legitimate renewals are one period apart, far
            // outside the window; the guard can be removed once the
            // Payments module checks `via_scheduler`.
            let subscription_id = payload
                .get("subscription_id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(last) = state.renew_guard.get(&subscription_id) {
                if last.elapsed() < Duration::from_secs(30) {
                    return Ok(json!({
                        "dispatched": false,
                        "event": "payment.subscription.renew_requested",
                        "reason": "Duplicate renewal suppressed: a renewal for this subscription was dispatched less than 30 seconds ago",
                    }));
                }
            }
            state
                .renew_guard
                .insert(subscription_id, std::time::Instant::now());
            let mut event_payload = payload.clone();
            if let Value::Object(map) = &mut event_payload {
                map.insert("via_scheduler".to_string(), Value::Bool(true));
            }
            state.emit("payment.subscription.renew_requested", event_payload);
            Ok(json!({ "dispatched": true, "event": "payment.subscription.renew_requested" }))
        }
        "report.generate" => {
            // The Analytics module listens to
            // `analytics.report.run_requested` and runs the report
            // named by `definition_id` in the payload.
            state.emit("analytics.report.run_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "analytics.report.run_requested" }))
        }
        "newsletter.send" => {
            // The Newsletter module listens to
            // `newsletter.campaign.send` and sends the campaign named
            // by `campaign_id` in the payload.
            state.emit("newsletter.campaign.send", payload.clone());
            Ok(json!({ "dispatched": true, "event": "newsletter.campaign.send" }))
        }
        "notification.digest" | "notification.digest.send" => {
            // The Notifications module listens to
            // `notification.digest.send` and sends one digest email
            // for the user named by `user_id` in the payload
            // (`email` overrides the user's address when present).
            // Both kind spellings dispatch to the same event:
            // `notification.digest` is the short kind, matching the
            // `newsletter.send` pattern, and
            // `notification.digest.send` is the alias that spells the
            // event name out. The payload is forwarded unchanged.
            // Note: `notification.digest.requested` is a different
            // event — the module emits it when a user saves a
            // non-immediate digest preference, and nothing listens to
            // it — so this arm never emits it.
            state.emit("notification.digest.send", payload.clone());
            Ok(json!({ "dispatched": true, "event": "notification.digest.send" }))
        }
        "meeting.start" => {
            // The Meetings module listens to
            // `meeting.start_requested` and starts the room named by
            // `room_id` in the payload. The payload is forwarded
            // unchanged.
            state.emit("meeting.start_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "meeting.start_requested" }))
        }
        "announcement.publish_due" => {
            // The Announcements module listens to
            // `announcement.publish_due` and publishes every
            // announcement whose publish time has arrived. The
            // payload is forwarded unchanged.
            state.emit("announcement.publish_due", payload.clone());
            Ok(json!({ "dispatched": true, "event": "announcement.publish_due" }))
        }
        "ticket.sla.check" => {
            // The Tickets module listens to
            // `ticket.sla.check_requested` and scans for SLA
            // breaches, emitting `ticket.sla.breached` for each one
            // it finds. The payload is forwarded unchanged.
            state.emit("ticket.sla.check_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "ticket.sla.check_requested" }))
        }
        other => Err(UnifiedError::unprocessable(format!(
            "Unknown job kind: {other}. Known kinds: {}",
            known_kinds_list()
        ))),
    }
}

/// Background worker for one queue item: up to 3 attempts with
/// exponential backoff (2s, 4s, 8s).
async fn process_queue_item(state: Arc<SchedulerState>, item_id: String) {
    let max_attempts = state
        .queue
        .get(&item_id)
        .map(|entry| entry.value().max_attempts)
        .unwrap_or(3);
    for attempt in 1..=max_attempts {
        let Some(item) = state.queue.get(&item_id).map(|entry| entry.value().clone()) else {
            return;
        };
        if let Some(mut entry) = state.queue.get_mut(&item_id) {
            entry.status = "processing".to_string();
            entry.attempts = attempt;
            entry.updated_at = now();
        }
        match dispatch(&state, &item.kind, &item.payload) {
            Ok(_) => {
                if let Some(mut entry) = state.queue.get_mut(&item_id) {
                    entry.status = "done".to_string();
                    entry.last_error = None;
                    entry.updated_at = now();
                }
                return;
            }
            Err(error) => {
                if attempt >= max_attempts {
                    if let Some(mut entry) = state.queue.get_mut(&item_id) {
                        entry.status = "failed".to_string();
                        entry.last_error = Some(error.message.clone());
                        entry.updated_at = now();
                    }
                    return;
                }
                if let Some(mut entry) = state.queue.get_mut(&item_id) {
                    entry.status = "queued".to_string();
                    entry.last_error = Some(error.message.clone());
                    entry.updated_at = now();
                }
                sleep(Duration::from_secs(2u64.pow(attempt))).await;
            }
        }
    }
}

/// Start the background worker for one queue item. Hook handlers are
/// synchronous and may run outside a runtime, so spawn on the current
/// runtime when there is one and fall back to a dedicated thread with
/// its own runtime otherwise.
fn spawn_queue_worker(state: Arc<SchedulerState>, item_id: String) {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(async move {
            process_queue_item(state, item_id).await;
        });
    } else {
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(process_queue_item(state, item_id));
        });
    }
}

/// Answer `scheduler.enqueue` (from the Mail module, and any other
/// module): create a queued job from the payload and report its id.
/// Accepts both shapes: `{ "kind", "payload" }`, and the Mail module's
/// `{ "job_id", "queue", "event", "payload" }`, where the kind is
/// derived from `event` (`mail.send_requested` -> `mail.send`) or
/// `queue` (`mail` -> `mail.send`). A supplied `job_id` is kept as the
/// queue item id so the Mail module's `mail.deliver` callback can find
/// the same job; the id is also merged into the stored payload so
/// dispatch routes the item to delivery instead of re-requesting it.
fn on_scheduler_enqueue(state: Arc<SchedulerState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let kind = data
        .get("kind")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            data.get("event")
                .and_then(|value| value.as_str())
                .map(|event| match event {
                    "mail.send_requested" => "mail.send".to_string(),
                    "geo.update_requested" => "geo.update".to_string(),
                    "ssl.renew" => "ssl.renew".to_string(),
                    "cleanup.requested" => "cleanup".to_string(),
                    "backup.run_requested" => "backup.run".to_string(),
                    "cluster.health.check_requested" => "cluster.health".to_string(),
                    "cluster.sync.requested" => "cluster.sync".to_string(),
                    "payment.subscription.renew_requested" => {
                        "payment.subscription.renew".to_string()
                    }
                    "analytics.report.run_requested" => "report.generate".to_string(),
                    "newsletter.campaign.send" => "newsletter.send".to_string(),
                    "notification.digest.send" => "notification.digest.send".to_string(),
                    "meeting.start_requested" => "meeting.start".to_string(),
                    "announcement.publish_due" => "announcement.publish_due".to_string(),
                    "ticket.sla.check_requested" => "ticket.sla.check".to_string(),
                    other => other.to_string(),
                })
        })
        .or_else(|| {
            data.get("queue")
                .and_then(|value| value.as_str())
                .map(|queue| match queue {
                    "mail" => "mail.send".to_string(),
                    other => other.to_string(),
                })
        });
    let Some(kind) = kind else {
        ctx.add_result(json!({
            "queued": false,
            "error": "scheduler.enqueue requires 'kind' (or a known 'event'/'queue')",
        }));
        return;
    };
    if !valid_kind(&kind) {
        ctx.add_result(json!({
            "queued": false,
            "error": format!("Unknown job kind: {kind}"),
        }));
        return;
    }
    let mut payload = data.get("payload").cloned().unwrap_or(Value::Null);
    let job_id = data
        .get("job_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_else(new_id);
    if let Value::Object(map) = &mut payload {
        map.entry("job_id")
            .or_insert_with(|| Value::String(job_id.clone()));
    }
    let item = QueueItem {
        id: job_id.clone(),
        kind,
        payload,
        status: "queued".to_string(),
        attempts: 0,
        max_attempts: 3,
        last_error: None,
        created_at: now(),
        updated_at: now(),
    };
    state.queue.insert(item.id.clone(), item.clone());
    spawn_queue_worker(state.clone(), item.id.clone());
    ctx.add_result(json!({ "job_id": job_id, "queued": true }));
}

// ---------------------------------------------------------------------------
// Job handlers
// ---------------------------------------------------------------------------

async fn list_jobs(
    state: web::Data<Arc<SchedulerState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<Job> = state
        .jobs
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|job| {
            matches_search(&format!("{} {}", job.name, job.kind), &params)
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| job.kind == *kind)
                && params.filters.get("enabled").is_none_or(|flag| {
                    job.enabled == (*flag == "true" || *flag == "1")
                })
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_job(
    state: web::Data<Arc<SchedulerState>>,
    body: web::Json<CreateJob>,
) -> HttpResponse {
    let result: UnifiedResult<Job> = (|| {
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Job name must not be empty"));
        }
        if !valid_kind(&input.kind) {
            return Err(UnifiedError::unprocessable(format!(
                "Unknown job kind: {}. Known kinds: {}",
                input.kind,
                known_kinds_list()
            )));
        }
        if !valid_schedule(&input.schedule) {
            return Err(UnifiedError::bad_request(
                "Schedule must be an interval in seconds or a cron-like expression such as \"*/5 * * * *\"",
            ));
        }
        let job = Job {
            id: new_id(),
            name: input.name.trim().to_string(),
            kind: input.kind,
            schedule: input.schedule.trim().to_string(),
            payload: input.payload.unwrap_or(Value::Null),
            enabled: input.enabled.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state.jobs.insert(job.id.clone(), job.clone());
        Ok(job)
    })();
    respond_created(result)
}

async fn get_job(
    state: web::Data<Arc<SchedulerState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Job> = state
        .jobs
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Job not found"));
    json::respond(result, false)
}

async fn update_job(
    state: web::Data<Arc<SchedulerState>>,
    path: web::Path<String>,
    body: web::Json<UpdateJob>,
) -> HttpResponse {
    let result: UnifiedResult<Job> = (|| {
        let input = body.into_inner();
        let mut job = state
            .jobs
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Job not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Job name must not be empty"));
            }
            job.name = name.trim().to_string();
        }
        if let Some(kind) = input.kind {
            if !valid_kind(&kind) {
                return Err(UnifiedError::unprocessable(format!(
                    "Unknown job kind: {kind}. Known kinds: {}",
                    known_kinds_list()
                )));
            }
            job.kind = kind;
        }
        if let Some(schedule) = input.schedule {
            if !valid_schedule(&schedule) {
                return Err(UnifiedError::bad_request(
                    "Schedule must be an interval in seconds or a cron-like expression such as \"*/5 * * * *\"",
                ));
            }
            job.schedule = schedule.trim().to_string();
        }
        if let Some(payload) = input.payload {
            job.payload = payload;
        }
        if let Some(enabled) = input.enabled {
            job.enabled = enabled;
        }
        job.updated_at = now();
        Ok(job.clone())
    })();
    json::respond(result, false)
}

async fn delete_job(
    state: web::Data<Arc<SchedulerState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .jobs
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Job not found"))
    })();
    json::respond(result, false)
}

async fn run_job(
    state: web::Data<Arc<SchedulerState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let job = state
            .jobs
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Job not found"))?;
        if !job.enabled {
            return Err(UnifiedError::unprocessable("Job is disabled"));
        }
        let started_at = now();
        let outcome = dispatch(&state, &job.kind, &job.payload);
        let finished_at = now();
        let (status, result_value) = match &outcome {
            Ok(value) => ("success".to_string(), value.clone()),
            Err(error) => (
                "failed".to_string(),
                json!({ "error": error.message, "code": error.code }),
            ),
        };
        let run = JobRun {
            id: new_id(),
            job_id: job.id.clone(),
            kind: job.kind.clone(),
            status,
            result: result_value.clone(),
            started_at,
            finished_at,
        };
        state.runs.insert(run.id.clone(), run.clone());
        match outcome {
            Ok(_) => Ok(json!({ "run": run })),
            Err(error) => Err(error),
        }
    })();
    json::respond(result, false)
}

async fn list_runs(
    state: web::Data<Arc<SchedulerState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let job_id = path.into_inner();
    if !state.jobs.contains_key(&job_id) {
        return json::error_response(&UnifiedError::not_found("Job not found"), false);
    }
    let mut items: Vec<JobRun> = state
        .runs
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|run| {
            run.job_id == job_id
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| run.status == *status)
        })
        .collect();
    items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

// ---------------------------------------------------------------------------
// Queue handlers
// ---------------------------------------------------------------------------

async fn list_queue(
    state: web::Data<Arc<SchedulerState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<QueueItem> = state
        .queue
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            params
                .filters
                .get("kind")
                .is_none_or(|kind| item.kind == *kind)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| item.status == *status)
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn enqueue(
    state: web::Data<Arc<SchedulerState>>,
    body: web::Json<EnqueueRequest>,
) -> HttpResponse {
    let result: UnifiedResult<QueueItem> = (|| {
        let input = body.into_inner();
        if !valid_kind(&input.kind) {
            return Err(UnifiedError::unprocessable(format!(
                "Unknown job kind: {}. Known kinds: {}",
                input.kind,
                known_kinds_list()
            )));
        }
        let item = QueueItem {
            id: new_id(),
            kind: input.kind,
            payload: input.payload.unwrap_or(Value::Null),
            status: "queued".to_string(),
            attempts: 0,
            max_attempts: 3,
            last_error: None,
            created_at: now(),
            updated_at: now(),
        };
        state.queue.insert(item.id.clone(), item.clone());
        let worker_state = state.clone();
        let item_id = item.id.clone();
        tokio::spawn(async move {
            process_queue_item(worker_state, item_id).await;
        });
        Ok(item)
    })();
    respond_created(result)
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct SchedulerModule {
    state: Arc<SchedulerState>,
}

impl SchedulerModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(SchedulerState::new()),
        }
    }
}

impl Default for SchedulerModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SchedulerModule {
    fn id(&self) -> &'static str {
        "scheduler"
    }

    fn name(&self) -> &'static str {
        "Scheduler"
    }

    fn description(&self) -> &'static str {
        "Scheduler and queue: named jobs with run history and a background queue with retries."
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
        bus.register(
            "scheduler.enqueue",
            "scheduler",
            Arc::new(move |ctx: &mut HookContext| {
                on_scheduler_enqueue(state.clone(), ctx)
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/scheduler/jobs", owner: "scheduler", description: "List jobs" },
            RouteInfo { method: "POST", path: "/api/v1/scheduler/jobs", owner: "scheduler", description: "Create job" },
            RouteInfo { method: "GET", path: "/api/v1/scheduler/jobs/{id}", owner: "scheduler", description: "Get job" },
            RouteInfo { method: "PUT", path: "/api/v1/scheduler/jobs/{id}", owner: "scheduler", description: "Update job" },
            RouteInfo { method: "DELETE", path: "/api/v1/scheduler/jobs/{id}", owner: "scheduler", description: "Delete job" },
            RouteInfo { method: "POST", path: "/api/v1/scheduler/jobs/{id}/run", owner: "scheduler", description: "Run job" },
            RouteInfo { method: "GET", path: "/api/v1/scheduler/jobs/{id}/runs", owner: "scheduler", description: "List runs" },
            RouteInfo { method: "GET", path: "/api/v1/scheduler/queue", owner: "scheduler", description: "List queue" },
            RouteInfo { method: "POST", path: "/api/v1/scheduler/queue", owner: "scheduler", description: "Enqueue" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/scheduler")
                .route("/jobs", web::get().to(list_jobs))
                .route("/jobs", web::post().to(create_job))
                .route("/jobs/{id}", web::get().to(get_job))
                .route("/jobs/{id}", web::put().to(update_job))
                .route("/jobs/{id}", web::delete().to(delete_job))
                .route("/jobs/{id}/run", web::post().to(run_job))
                .route("/jobs/{id}/runs", web::get().to(list_runs))
                .route("/queue", web::get().to(list_queue))
                .route("/queue", web::post().to(enqueue)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SchedulerModule::new()));
}
