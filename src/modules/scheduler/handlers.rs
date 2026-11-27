// Scheduler - Job, run and queue handlers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, web, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;

use super::{CreateJob, EnqueueRequest, Job, JobRun, QueueItem, SchedulerState, UpdateJob, dispatch,
    known_kinds_list, matches_search, new_id, now, paginate, process_queue_item, respond_created,
    valid_kind, valid_schedule};

/// Capability names checked through the `role.check` hook, following
/// the `workflows.view` / `workflows.manage` naming of the
/// neighbouring modules.
pub(crate) const PERM_SCHEDULER_VIEW: &str = "scheduler.view";
pub(crate) const PERM_SCHEDULER_MANAGE: &str = "scheduler.manage";

// Authentication and authorization

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the contracts, workflows and
/// assets modules.
pub(crate) struct Caller {
    #[allow(dead_code)]
    actor: String,
    role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook on the scheduler's hook bus (wired to the
/// application bus by `Module::init`). Fails closed: no answer means
/// not allowed - before `init` the bus is empty, so every check
/// fails closed rather than letting an unauthenticated caller in.
pub(crate) fn check_capability(state: &SchedulerState, role: &str, permission: &str) -> bool {
    let bus = state
        .hooks
        .read()
        .expect("hooks lock poisoned")
        .clone();
    let context = bus.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(
    state: &SchedulerState,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(state, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Authenticate the caller and require `permission`, returning the
/// error response to send when the gate fails. Every handler calls
/// this first: reads need `scheduler.view`, writes (create, update,
/// delete, run, enqueue) need `scheduler.manage`. Without this gate
/// anyone could create, run or delete jobs and enqueue any kind -
/// including `tenant.destroy_due`, `backup.run`,
/// `payment.subscription.renew`, `newsletter.send` and
/// `marketing.campaign.send`.
fn authorize(
    state: &Arc<SchedulerState>,
    req: &HttpRequest,
    permission: &str,
) -> Result<Caller, HttpResponse> {
    match caller(req).and_then(|caller| {
        require_permission(state, &caller.role, permission)?;
        Ok(caller)
    }) {
        Ok(caller) => Ok(caller),
        Err(error) => Err(json::error_response(&error, false)),
    }
}

// Job handlers

pub(crate) async fn list_jobs(
    state: web::Data<Arc<SchedulerState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(response) = authorize(&state, &req, PERM_SCHEDULER_VIEW) {
        return response;
    }
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

pub(crate) async fn create_job(
    state: web::Data<Arc<SchedulerState>>,
    req: HttpRequest,
    body: web::Json<CreateJob>,
) -> HttpResponse {
    if let Err(response) = authorize(&state, &req, PERM_SCHEDULER_MANAGE) {
        return response;
    }
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

pub(crate) async fn get_job(
    state: web::Data<Arc<SchedulerState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(response) = authorize(&state, &req, PERM_SCHEDULER_VIEW) {
        return response;
    }
    let result: UnifiedResult<Job> = state
        .jobs
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Job not found"));
    json::respond(result, false)
}

pub(crate) async fn update_job(
    state: web::Data<Arc<SchedulerState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateJob>,
) -> HttpResponse {
    if let Err(response) = authorize(&state, &req, PERM_SCHEDULER_MANAGE) {
        return response;
    }
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

pub(crate) async fn delete_job(
    state: web::Data<Arc<SchedulerState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(response) = authorize(&state, &req, PERM_SCHEDULER_MANAGE) {
        return response;
    }
    let result: UnifiedResult<Value> = (|| {
        state
            .jobs
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Job not found"))
    })();
    json::respond(result, false)
}

pub(crate) async fn run_job(
    state: web::Data<Arc<SchedulerState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(response) = authorize(&state, &req, PERM_SCHEDULER_MANAGE) {
        return response;
    }
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

pub(crate) async fn list_runs(
    state: web::Data<Arc<SchedulerState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(response) = authorize(&state, &req, PERM_SCHEDULER_VIEW) {
        return response;
    }
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

// Queue handlers

pub(crate) async fn list_queue(
    state: web::Data<Arc<SchedulerState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(response) = authorize(&state, &req, PERM_SCHEDULER_VIEW) {
        return response;
    }
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

pub(crate) async fn enqueue(
    state: web::Data<Arc<SchedulerState>>,
    req: HttpRequest,
    body: web::Json<EnqueueRequest>,
) -> HttpResponse {
    if let Err(response) = authorize(&state, &req, PERM_SCHEDULER_MANAGE) {
        return response;
    }
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
            process_queue_item(worker_state.get_ref().clone(), item_id).await;
        });
        Ok(item)
    })();
    respond_created(result)
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use unified_core::hooks::HookContext;
    use actix_web::test::TestRequest;

    fn state() -> Arc<SchedulerState> {
        Arc::new(SchedulerState::new())
    }

    fn authed_request() -> HttpRequest {
        verified_request("user-1", "admin")
    }

    /// Answer `role.check` on the state's bus: allow the given
    /// permission for any role, deny everything else.
    fn grant(state: &Arc<SchedulerState>, permission: &'static str) {
        state
            .hooks
            .read()
            .expect("hooks lock poisoned")
            .register(
                "role.check",
                "scheduler-test",
                Arc::new(move |ctx: &mut HookContext| {
                    let wanted = ctx
                        .data
                        .get("permission")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if wanted == permission {
                        ctx.set("allowed", Value::Bool(true));
                    }
                }),
            );
    }

    #[test]
    fn caller_rejects_missing_headers_with_401() {
        let request = TestRequest::default().to_http_request();
        let error = caller(&request).expect_err("missing headers must fail");
        assert_eq!(error.code, 401);
        let request = TestRequest::default().to_http_request();
        let error = caller(&request).expect_err("missing role must fail");
        assert_eq!(error.code, 401);
        let caller = caller(&authed_request()).expect("headers present");
        assert_eq!(caller.role, "admin");
    }

    #[test]
    fn require_permission_fails_closed_without_role_check_answer() {
        let state = state();
        let error = require_permission(&state, "admin", PERM_SCHEDULER_VIEW)
            .expect_err("no role.check answer must deny");
        assert_eq!(error.code, 403);
    }

    #[test]
    fn authorize_allows_granted_permission() {
        let state = state();
        grant(&state, PERM_SCHEDULER_VIEW);
        let caller = authorize(&state, &authed_request(), PERM_SCHEDULER_VIEW)
            .expect("granted permission must pass");
        assert_eq!(caller.role, "admin");
        assert!(authorize(&state, &authed_request(), PERM_SCHEDULER_MANAGE).is_err());
    }

    #[tokio::test]
    async fn list_jobs_rejects_unauthenticated_caller() {
        let state = state();
        let request = TestRequest::default().to_http_request();
        let response = list_jobs(
            web::Data::new(state),
            request,
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status().as_u16(), 401);
    }

    #[tokio::test]
    async fn list_jobs_allows_caller_with_view_permission() {
        let state = state();
        grant(&state, PERM_SCHEDULER_VIEW);
        let response = list_jobs(
            web::Data::new(state),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status().as_u16(), 200);
    }

    #[tokio::test]
    async fn enqueue_rejects_caller_with_view_only_permission() {
        let state = state();
        grant(&state, PERM_SCHEDULER_VIEW);
        let response = enqueue(
            web::Data::new(state),
            authed_request(),
            web::Json(EnqueueRequest {
                kind: "mail.send".to_string(),
                payload: None,
            }),
        )
        .await;
        assert_eq!(response.status().as_u16(), 403);
    }
}
