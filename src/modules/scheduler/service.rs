// Scheduler - Job dispatch and queue worker.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::json;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sha2::Digest;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::time::{sleep, Duration};

use super::validation::schedule_matches;
use super::{Job, JobRun, SchedulerState, known_kinds_list, new_id, now};

/// Dispatch one unit of work by kind. This is the real handler for both
/// scheduled jobs and queued items.
pub(crate) fn dispatch(state: &Arc<SchedulerState>, kind: &str, payload: &Value) -> UnifiedResult<Value> {
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
            // chain of queue items - one per dispatch, forever - and
            // never charge anything. Two guards break the chain:
            // the emitted event carries `via_scheduler: true` so the
            // handler can tell a scheduler-dispatched request apart
            // from an operator request (the handler must check it
            // before re-enqueuing), and this scheduler suppresses a
            // repeat dispatch for the same subscription within 30
            // seconds. Legitimate renewals are one period apart, far
            // outside the window; the guard can be removed once the
            // Payments module checks `via_scheduler`.
            // Guard key: the subscription id when the payload has
            // one. A payload without `subscription_id` has no
            // identity to key on, so it is keyed by the SHA-256 hex
            // of its canonical JSON instead: identical id-less
            // payloads still share one 30-second suppression bucket
            // (the loop guard keeps working), while distinct id-less
            // renewals no longer suppress each other through a
            // single shared empty-string key.
            let guard_key = match payload
                .get("subscription_id")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
            {
                Some(subscription_id) => subscription_id.to_string(),
                None => {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    let canonical = serde_json::to_string(payload).unwrap_or_default();
                    let mut hasher = sha2::Sha256::new();
                    hasher.update(canonical.as_bytes());
                    let digest = hasher.finalize();
                    let mut hex = String::with_capacity(digest.len() * 2);
                    for &byte in digest.iter() {
                        hex.push(HEX[(byte >> 4) as usize] as char);
                        hex.push(HEX[(byte & 0x0f) as usize] as char);
                    }
                    format!("no-id:{hex}")
                }
            };
            if let Some(last) = state.renew_guard.get(&guard_key) {
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
                .insert(guard_key, std::time::Instant::now());
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
            // event - the module emits it when a user saves a
            // non-immediate digest preference, and nothing listens to
            // it - so this arm never emits it.
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
        "backup.verify" => {
            // The Backup module listens to `backup.verify_requested`
            // and verifies runs stale per `backup.verify_interval_hours`.
            state.emit("backup.verify_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "backup.verify_requested" }))
        }
        "backup.prune" => {
            // The Backup module listens to `backup.prune_requested`.
            state.emit("backup.prune_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "backup.prune_requested" }))
        }
        "export.run" => {
            // The Import/Export module listens to
            // `export.run_requested` and runs the stored schedule
            // named by `export_schedule_id` / `job_id`.
            state.emit("export.run_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "export.run_requested" }))
        }
        "cluster.sync.retry_due" => {
            // The Clusters module registered a handler for this
            // exact name (`cluster.sync.retry_due`), not a
            // `_requested` spelling, so the kind itself is emitted.
            state.emit("cluster.sync.retry_due", payload.clone());
            Ok(json!({ "dispatched": true, "event": "cluster.sync.retry_due" }))
        }
        "webinar.start" => {
            // The Webinars module listens to `webinar.start_requested`.
            state.emit("webinar.start_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "webinar.start_requested" }))
        }
        "webinar.reminder" => {
            // The Webinars module listens to
            // `webinar.reminder_requested`.
            state.emit("webinar.reminder_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "webinar.reminder_requested" }))
        }
        "appointments.reminders.send" => {
            // The Appointments module listens to
            // `appointments.reminders.send_requested` and sends the
            // due reminder through `notification.requested`.
            state.emit("appointments.reminders.send_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "appointments.reminders.send_requested" }))
        }
        "marketing.campaign.send" => {
            // The Marketing module registered a handler for this
            // exact name, so the kind itself is emitted.
            state.emit("marketing.campaign.send", payload.clone());
            Ok(json!({ "dispatched": true, "event": "marketing.campaign.send" }))
        }
        "marketing.sequence.process_due" => {
            // The Marketing module registered a handler for this
            // exact name, so the kind itself is emitted.
            state.emit("marketing.sequence.process_due", payload.clone());
            Ok(json!({ "dispatched": true, "event": "marketing.sequence.process_due" }))
        }
        "field_service.plans.check" => {
            // The Field Service module listens to
            // `field_service.plans.check_requested`.
            state.emit("field_service.plans.check_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "field_service.plans.check_requested" }))
        }
        "calendar.feeds.prune" => {
            // The Calendar module listens to
            // `calendar.feeds.prune_requested` and deletes expired
            // tokens plus revoked tokens older than 30 days.
            state.emit("calendar.feeds.prune_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "calendar.feeds.prune_requested" }))
        }
        "holidays.sync" => {
            // The Holidays module listens to `holidays.sync_requested`.
            state.emit("holidays.sync_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "holidays.sync_requested" }))
        }
        "holidays.sync_all" => {
            // The Holidays module listens to
            // `holidays.sync_all_requested`.
            state.emit("holidays.sync_all_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "holidays.sync_all_requested" }))
        }
        "tenant.destroy_due" => {
            // The Tenant Destroy module registered a handler for
            // this exact name; it lists the due destroy requests.
            // The sweep itself runs here, not in the hook handler
            // (hook handlers have no `AppContext`): the state
            // holds the live context's handles from `Module::init`
            // and rebuilds the context for
            // `tenant_destroy::process_due_destroys`, which drops
            // every grace-expired tenant's schema and returns the
            // destroyed request ids. Without a stored context the
            // arm fails closed - returning `dispatched: true`
            // for work that never ran would silently drop the
            // sweep.
            state.emit("tenant.destroy_due", payload.clone());
            let Some(ctx) = state.app_context() else {
                return Err(UnifiedError::internal(
                    "Scheduler has no application context: the tenant.destroy_due sweep cannot run until the scheduler module is initialised",
                ));
            };
            let destroyed = crate::modules::tenant_destroy::process_due_destroys(&ctx)?;
            let destroyed: Vec<String> = destroyed.iter().map(|id| id.to_string()).collect();
            Ok(json!({ "dispatched": true, "event": "tenant.destroy_due", "destroyed": destroyed }))
        }
        "approvals.sweep_expiry" => {
            // The Approvals module listens to
            // `approvals.sweep_expiry_requested` and runs the same
            // expiry sweep as `POST /requests/sweep-expiry`.
            state.emit("approvals.sweep_expiry_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "approvals.sweep_expiry_requested" }))
        }
        "license.expiry.check" => {
            // The Licensing module listens to
            // `license.expiry.check_requested`; renewal reminders
            // and grace alerts ride on that scan.
            state.emit("license.expiry.check_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "license.expiry.check_requested" }))
        }
        "auditors.expire" => {
            // The Auditors module listens to
            // `auditors.expire_requested`.
            state.emit("auditors.expire_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "auditors.expire_requested" }))
        }
        "logistics.maintenance.remind" => {
            // The Logistics module listens to
            // `logistics.maintenance.remind_requested`.
            state.emit("logistics.maintenance.remind_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "logistics.maintenance.remind_requested" }))
        }
        "web3.poll" => {
            // The Web3 module listens to `web3.poll_requested` and
            // polls every watched address on a background thread
            // (see `on_poll_requested` in `src/modules/web3.rs`).
            // The poll itself is async network work, so the hook
            // only starts it - the dispatch result is honest about
            // that: the listener exists and the poll is running.
            state.emit("web3.poll_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "web3.poll_requested" }))
        }
        "timetracker.screenshots.purge" => {
            // The TimeTracker module listens to
            // `timetracker.screenshots.purge_requested`.
            state.emit("timetracker.screenshots.purge_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "timetracker.screenshots.purge_requested" }))
        }
        "storage.lifecycle.run" => {
            // The Storage Manager listens to
            // `storage.lifecycle.run_requested`.
            state.emit("storage.lifecycle.run_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "storage.lifecycle.run_requested" }))
        }
        "storage.byo.health_check" => {
            // The Storage Manager listens to
            // `storage.byo.health_check_requested`.
            state.emit("storage.byo.health_check_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "storage.byo.health_check_requested" }))
        }
        "events.reminders.send" => {
            // The Events module listens to
            // `events.reminders.send_requested`.
            state.emit("events.reminders.send_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "events.reminders.send_requested" }))
        }
        "todos.reminders.send" => {
            // The Todos module listens to
            // `todos.reminders.send_requested`.
            state.emit("todos.reminders.send_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "todos.reminders.send_requested" }))
        }
        "saas_domains.reservations.expire" => {
            // The SaaS Domains module registered a handler for this
            // exact name, so the kind itself is emitted.
            state.emit("saas_domains.reservations.expire", payload.clone());
            Ok(json!({ "dispatched": true, "event": "saas_domains.reservations.expire" }))
        }
        "ssl.cloudflare.recheck" => {
            // The Certificates module registered a handler for this
            // exact name, so the kind itself is emitted.
            state.emit("ssl.cloudflare.recheck", payload.clone());
            Ok(json!({ "dispatched": true, "event": "ssl.cloudflare.recheck" }))
        }
        "advisor.evaluate" => {
            // The Advisor module listens to
            // `advisor.evaluate_requested`.
            state.emit("advisor.evaluate_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "advisor.evaluate_requested" }))
        }
        "billing.subscriptions.scan" => {
            // The Billing module registered a handler for this exact
            // name, so the kind itself is emitted.
            state.emit("billing.subscriptions.scan", payload.clone());
            Ok(json!({ "dispatched": true, "event": "billing.subscriptions.scan" }))
        }
        "saas.product_subdomain.expire_grace" => {
            // The SaaS Domains module registered a handler for this
            // exact name (the product-subdomain grace sweep), so the
            // kind itself is emitted.
            state.emit("saas.product_subdomain.expire_grace", payload.clone());
            Ok(json!({ "dispatched": true, "event": "saas.product_subdomain.expire_grace" }))
        }
        "workflows.run" => {
            // The Workflows module listens to
            // `workflows.run_requested` and runs the scheduled
            // workflow named by `workflow_id` in the payload. The
            // payload is forwarded unchanged.
            state.emit("workflows.run_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "workflows.run_requested" }))
        }
        "contracts.expiry_alert" => {
            // The Contracts module listens to
            // `contracts.expiry_alert_requested` and sends the due
            // alert for the contract named by `contract_id` in the
            // payload (`days_before` is the armed offset,
            // `send_at` the scheduled send time). The payload is
            // forwarded unchanged.
            state.emit("contracts.expiry_alert_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "contracts.expiry_alert_requested" }))
        }
        "assets.warranty.check" => {
            // The Assets module listens to
            // `assets.warranty.check_requested` and runs the
            // warranty / licence expiry check. The payload is
            // forwarded unchanged.
            state.emit("assets.warranty.check_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "assets.warranty.check_requested" }))
        }
        "inbox.unsnooze" => {
            // The Inbox module listens to
            // `inbox.unsnooze_requested` and clears the snooze on
            // the thread named by `thread_id` in the payload
            // (`unsnooze_at` is the scheduled re-appearance time).
            // The payload is forwarded unchanged.
            state.emit("inbox.unsnooze_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "inbox.unsnooze_requested" }))
        }
        other => Err(UnifiedError::unprocessable(format!(
            "Unknown job kind: {other}. Known kinds: {}",
            known_kinds_list()
        ))),
    }
}

/// Claim one queue item for dispatch, atomically: the status check
/// and the move to `processing` happen under the same DashMap shard
/// lock (`get_mut`), so two workers racing for the same item id
/// cannot both win - the loser sees `processing` (or `done` /
/// `failed`) instead of `queued` and returns without dispatching.
/// Returns the claimed item, or `None` when the item is gone or
/// already claimed / finished.
fn claim_queue_item(state: &Arc<SchedulerState>, item_id: &str, attempt: u32) -> Option<super::QueueItem> {
    let mut entry = state.queue.get_mut(item_id)?;
    if entry.status != "queued" {
        return None;
    }
    entry.status = "processing".to_string();
    entry.attempts = attempt;
    entry.updated_at = now();
    Some(entry.value().clone())
}

/// Background worker for one queue item: up to 3 attempts with
/// exponential backoff (2s, 4s, 8s). Each attempt first claims the
/// item (see `claim_queue_item`); a worker that loses the claim
/// returns at once, so one queue item is never dispatched twice.
pub(crate) async fn process_queue_item(state: Arc<SchedulerState>, item_id: String) {
    let max_attempts = state
        .queue
        .get(&item_id)
        .map(|entry| entry.value().max_attempts)
        .unwrap_or(3);
    for attempt in 1..=max_attempts {
        let Some(item) = claim_queue_item(&state, &item_id, attempt) else {
            return;
        };
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
pub(crate) fn spawn_queue_worker(state: Arc<SchedulerState>, item_id: String) {
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

// Built-in jobs seeded at startup

/// Id of the built-in `tenant.destroy_due` job seeded by
/// `seed_builtin_jobs`. A fixed id keeps the seeded job
/// recognisable in the jobs API; an admin can still edit or
/// delete it like any other job.
pub(crate) const DESTROY_DUE_JOB_ID: &str = "builtin-tenant-destroy-due";

/// Default interval (seconds) of the built-in `tenant.destroy_due`
/// job: grace-expired destroy requests are swept once an hour.
/// The interval is only the seeded default - the job remains
/// editable through `PUT /api/v1/scheduler/jobs/{id}`.
pub(crate) const DESTROY_DUE_INTERVAL_SECS: u64 = 3600;

/// Seed the built-in jobs. Idempotent: when a job of kind
/// `tenant.destroy_due` already exists - seeded on an earlier
/// start, or created by an admin - nothing is added, so restarts
/// never duplicate the sweep. Called from `Module::init` before
/// the ticker starts, so the first tick already has the job.
pub(crate) fn seed_builtin_jobs(state: &Arc<SchedulerState>) {
    if state
        .jobs
        .iter()
        .any(|entry| entry.value().kind == "tenant.destroy_due")
    {
        return;
    }
    // A job with the fixed id but a different kind must never be
    // clobbered: fall back to a fresh id in that pathological
    // case instead of overwriting an unrelated job.
    let id = if state.jobs.contains_key(DESTROY_DUE_JOB_ID) {
        new_id()
    } else {
        DESTROY_DUE_JOB_ID.to_string()
    };
    let at = now();
    let job = Job {
        id: id.clone(),
        name: "Tenant destroy due sweep".to_string(),
        kind: "tenant.destroy_due".to_string(),
        schedule: DESTROY_DUE_INTERVAL_SECS.to_string(),
        payload: json!({}),
        enabled: true,
        created_at: at,
        updated_at: at,
    };
    state.jobs.insert(id, job);
}

// Ticker: scheduled jobs actually fire

/// Jobs due at `at`, in creation order. An interval job (`"300"`)
/// is due once its interval elapsed since its last fire, or since
/// `created_at` when it never fired. A cron job is due when its
/// fields match `at` and it did not already fire in this minute.
/// Disabled jobs are never due.
pub(crate) fn due_jobs(state: &Arc<SchedulerState>, at: DateTime<Utc>) -> Vec<Job> {
    let mut jobs: Vec<Job> = state
        .jobs
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|job| job.enabled)
        .filter(|job| {
            if let Ok(seconds) = job.schedule.trim().parse::<u64>() {
                let baseline = state
                    .last_run
                    .get(&job.id)
                    .map(|entry| *entry.value())
                    .unwrap_or(job.created_at);
                (at.timestamp() - baseline.timestamp()) as i128 >= seconds as i128
            } else {
                schedule_matches(&job.schedule, at)
                    && state
                        .last_run
                        .get(&job.id)
                        .map(|entry| entry.value().timestamp() / 60 != at.timestamp() / 60)
                        .unwrap_or(true)
            }
        })
        .collect();
    jobs.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    jobs
}

/// Fire every due job once through `dispatch` and record a `JobRun`
/// for each, exactly as a manual `run_job` would. The last-fire
/// timestamp is recorded even for a failed dispatch, so a failing
/// job waits out its interval (or the next cron minute) instead of
/// retrying on every tick. Returns the recorded runs.
pub(crate) fn run_due_jobs(state: &Arc<SchedulerState>) -> Vec<JobRun> {
    let at = now();
    let due = due_jobs(state, at);
    let mut runs = Vec::with_capacity(due.len());
    for job in due {
        let started_at = at;
        let outcome = dispatch(state, &job.kind, &job.payload);
        let finished_at = now();
        let (status, result_value) = match &outcome {
            Ok(value) => ("success".to_string(), value.clone()),
            Err(error) => (
                "failed".to_string(),
                json!({ "error": error.message, "code": error.code }),
            ),
        };
        let run = JobRun {
            id: super::new_id(),
            job_id: job.id.clone(),
            kind: job.kind.clone(),
            status,
            result: result_value,
            started_at,
            finished_at,
        };
        state.runs.insert(run.id.clone(), run.clone());
        state.last_run.insert(job.id.clone(), at);
        runs.push(run);
    }
    runs
}

/// Start the ticker: a background task that calls `run_due_jobs`
/// every 30 seconds for the lifetime of the process, so jobs with a
/// schedule fire on their own instead of only on manual runs. The
/// first tick runs immediately, so jobs already past due fire on
/// startup. Starting twice is a no-op (`ticker_started`). Called
/// from `Module::init`, which runs inside the server runtime; when
/// no runtime exists, a dedicated thread with its own runtime is
/// used instead, mirroring `spawn_queue_worker`.
pub(crate) fn start_ticker(state: Arc<SchedulerState>) {
    if state
        .ticker_started
        .swap(true, Ordering::SeqCst)
    {
        return;
    }
    let task = async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            run_due_jobs(&state);
        }
    };
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(task);
    } else {
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(task);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::hooks::HookContext;
    use std::sync::Mutex;

    fn state() -> Arc<SchedulerState> {
        Arc::new(SchedulerState::new())
    }

    #[test]
    fn dispatch_contracts_expiry_alert_emits_requested_event() {
        let state = state();
        let payload = json!({
            "contract_id": "11111111-1111-1111-1111-111111111111",
            "days_before": 7,
            "send_at": "2026-11-15T00:00:00Z",
        });
        let outcome = dispatch(&state, "contracts.expiry_alert", &payload)
            .expect("dispatch of a known kind must succeed");
        assert_eq!(
            outcome.get("dispatched").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            outcome.get("event").and_then(Value::as_str),
            Some("contracts.expiry_alert_requested")
        );
    }

    #[test]
    fn dispatch_assets_warranty_check_emits_requested_event() {
        let state = state();
        let payload = json!({ "checked_at": "2026-11-22T08:00:00Z" });
        let outcome = dispatch(&state, "assets.warranty.check", &payload)
            .expect("dispatch of a known kind must succeed");
        assert_eq!(
            outcome.get("dispatched").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            outcome.get("event").and_then(Value::as_str),
            Some("assets.warranty.check_requested")
        );
    }

    #[test]
    fn dispatch_contracts_expiry_alert_forwards_payload_unchanged() {
        // A listener on the bus records the exact payload the arm
        // emitted, proving the alert payload (`contract_id`,
        // `days_before`, `send_at`) reaches the Contracts module
        // unchanged.
        let state = state();
        let seen: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let seen_clone = Arc::clone(&seen);
        state
            .hooks
            .read()
            .expect("hooks lock poisoned")
            .register(
                "contracts.expiry_alert_requested",
                "scheduler-test",
                Arc::new(move |ctx: &mut HookContext| {
                    *seen_clone.lock().expect("seen lock poisoned") = Some(ctx.data.clone());
                }),
            );
        let payload = json!({
            "contract_id": "11111111-1111-1111-1111-111111111111",
            "days_before": 30,
            "send_at": "2026-11-15T00:00:00Z",
        });
        dispatch(&state, "contracts.expiry_alert", &payload)
            .expect("dispatch of a known kind must succeed");
        let seen = seen.lock().expect("seen lock poisoned").clone();
        assert_eq!(seen, Some(payload));
    }

    #[test]
    fn dispatch_assets_warranty_check_forwards_payload_unchanged() {
        let state = state();
        let seen: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let seen_clone = Arc::clone(&seen);
        state
            .hooks
            .read()
            .expect("hooks lock poisoned")
            .register(
                "assets.warranty.check_requested",
                "scheduler-test",
                Arc::new(move |ctx: &mut HookContext| {
                    *seen_clone.lock().expect("seen lock poisoned") = Some(ctx.data.clone());
                }),
            );
        let payload = json!({ "checked_at": "2026-11-22T08:00:00Z" });
        dispatch(&state, "assets.warranty.check", &payload)
            .expect("dispatch of a known kind must succeed");
        let seen = seen.lock().expect("seen lock poisoned").clone();
        assert_eq!(seen, Some(payload));
    }

    #[test]
    fn auditors_expire_is_a_known_kind() {
        // The kind the auditors module enqueues (`auditors.expire`)
        // must be accepted by the scheduler, or every expiry job
        // is declined at enqueue time.
        assert!(known_kinds_list().contains("auditors.expire"));
    }

    #[test]
    fn dispatch_auditors_expire_emits_requested_event() {
        let state = state();
        let payload = json!({
            "auditor_id": "11111111-1111-1111-1111-111111111111",
            "run_at": "2026-11-22T08:00:00Z",
        });
        let outcome = dispatch(&state, "auditors.expire", &payload)
            .expect("dispatch of a known kind must succeed");
        assert_eq!(
            outcome.get("dispatched").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            outcome.get("event").and_then(Value::as_str),
            Some("auditors.expire_requested")
        );
    }

    #[test]
    fn dispatch_auditors_expire_forwards_payload_unchanged() {
        // A listener on the bus records the exact payload the arm
        // emitted, proving the expiry payload (`auditor_id`,
        // `run_at`) reaches the Auditors module unchanged - the
        // event name and payload the `auditors.expire_requested`
        // listener expects.
        let state = state();
        let seen: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let seen_clone = Arc::clone(&seen);
        state
            .hooks
            .read()
            .expect("hooks lock poisoned")
            .register(
                "auditors.expire_requested",
                "scheduler-test",
                Arc::new(move |ctx: &mut HookContext| {
                    *seen_clone.lock().expect("seen lock poisoned") = Some(ctx.data.clone());
                }),
            );
        let payload = json!({
            "auditor_id": "11111111-1111-1111-1111-111111111111",
            "run_at": "2026-11-22T08:00:00Z",
        });
        dispatch(&state, "auditors.expire", &payload)
            .expect("dispatch of a known kind must succeed");
        let seen = seen.lock().expect("seen lock poisoned").clone();
        assert_eq!(seen, Some(payload));
    }

    #[test]
    fn dispatch_inbox_unsnooze_emits_requested_event() {
        // The Inbox module registers `on_unsnooze_requested` for
        // `inbox.unsnooze_requested` (inbox/module.rs); the arm
        // must emit exactly that event name.
        let state = state();
        let payload = json!({
            "thread_id": "11111111-1111-1111-1111-111111111111",
            "unsnooze_at": "2026-11-22T08:00:00Z",
        });
        let outcome = dispatch(&state, "inbox.unsnooze", &payload)
            .expect("dispatch of a known kind must succeed");
        assert_eq!(
            outcome.get("dispatched").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            outcome.get("event").and_then(Value::as_str),
            Some("inbox.unsnooze_requested")
        );
    }

    #[test]
    fn dispatch_inbox_unsnooze_forwards_payload_unchanged() {
        let state = state();
        let seen: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let seen_clone = Arc::clone(&seen);
        state
            .hooks
            .read()
            .expect("hooks lock poisoned")
            .register(
                "inbox.unsnooze_requested",
                "scheduler-test",
                Arc::new(move |ctx: &mut HookContext| {
                    *seen_clone.lock().expect("seen lock poisoned") = Some(ctx.data.clone());
                }),
            );
        let payload = json!({
            "thread_id": "11111111-1111-1111-1111-111111111111",
            "unsnooze_at": "2026-11-22T08:00:00Z",
        });
        dispatch(&state, "inbox.unsnooze", &payload)
            .expect("dispatch of a known kind must succeed");
        let seen = seen.lock().expect("seen lock poisoned").clone();
        assert_eq!(seen, Some(payload));
    }

    #[test]
    fn renew_guard_suppresses_identical_id_less_renewal() {
        let state = state();
        let payload = json!({ "amount": 10 });
        let first = dispatch(&state, "payment.subscription.renew", &payload)
            .expect("first dispatch must succeed");
        assert_eq!(first.get("dispatched").and_then(Value::as_bool), Some(true));
        let second = dispatch(&state, "payment.subscription.renew", &payload)
            .expect("second dispatch must succeed");
        assert_eq!(second.get("dispatched").and_then(Value::as_bool), Some(false));
    }

    #[test]
    fn renew_guard_does_not_suppress_distinct_id_less_renewals() {
        let state = state();
        let first = dispatch(
            &state,
            "payment.subscription.renew",
            &json!({ "amount": 10 }),
        )
        .expect("first dispatch must succeed");
        assert_eq!(first.get("dispatched").and_then(Value::as_bool), Some(true));
        let second = dispatch(
            &state,
            "payment.subscription.renew",
            &json!({ "amount": 20 }),
        )
        .expect("second dispatch must succeed");
        assert_eq!(second.get("dispatched").and_then(Value::as_bool), Some(true));
    }

    #[test]
    fn renew_guard_still_suppresses_same_subscription_id() {
        let state = state();
        let payload = json!({ "subscription_id": "sub-1" });
        let first = dispatch(&state, "payment.subscription.renew", &payload)
            .expect("first dispatch must succeed");
        assert_eq!(first.get("dispatched").and_then(Value::as_bool), Some(true));
        let second = dispatch(&state, "payment.subscription.renew", &payload)
            .expect("second dispatch must succeed");
        assert_eq!(second.get("dispatched").and_then(Value::as_bool), Some(false));
    }

    fn queue_item(id: &str, kind: &str, status: &str) -> super::QueueItem {
        super::QueueItem {
            id: id.to_string(),
            kind: kind.to_string(),
            payload: json!({}),
            status: status.to_string(),
            attempts: 0,
            max_attempts: 1,
            last_error: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn dispatch_counter(state: &Arc<SchedulerState>, event: &str) -> Arc<Mutex<u32>> {
        let count: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
        let count_clone = Arc::clone(&count);
        state
            .hooks
            .read()
            .expect("hooks lock poisoned")
            .register(
                event,
                "scheduler-test",
                Arc::new(move |_ctx: &mut HookContext| {
                    *count_clone.lock().expect("count lock poisoned") += 1;
                }),
            );
        count
    }

    #[tokio::test]
    async fn process_queue_item_skips_already_claimed_item() {
        let state = state();
        let count = dispatch_counter(&state, "mail.send_requested");
        state
            .queue
            .insert("item-1".to_string(), queue_item("item-1", "mail.send", "processing"));
        process_queue_item(state.clone(), "item-1".to_string()).await;
        assert_eq!(*count.lock().expect("count lock poisoned"), 0);
        assert_eq!(
            state.queue.get("item-1").map(|e| e.value().status.clone()),
            Some("processing".to_string())
        );
    }

    #[tokio::test]
    async fn two_workers_for_same_item_dispatch_only_once() {
        let state = state();
        let count = dispatch_counter(&state, "mail.send_requested");
        state
            .queue
            .insert("item-1".to_string(), queue_item("item-1", "mail.send", "queued"));
        tokio::join!(
            process_queue_item(state.clone(), "item-1".to_string()),
            process_queue_item(state.clone(), "item-1".to_string()),
        );
        assert_eq!(*count.lock().expect("count lock poisoned"), 1);
        assert_eq!(
            state.queue.get("item-1").map(|e| e.value().status.clone()),
            Some("done".to_string())
        );
    }

    fn test_job(id: &str, schedule: &str, created_seconds_ago: i64) -> Job {
        Job {
            id: id.to_string(),
            name: "test job".to_string(),
            kind: "workflows.run".to_string(),
            schedule: schedule.to_string(),
            payload: json!({}),
            enabled: true,
            created_at: Utc::now() - chrono::Duration::seconds(created_seconds_ago),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn due_jobs_fires_overdue_interval_job_only() {
        let state = state();
        let overdue = test_job("job-overdue", "300", 3600);
        let fresh = test_job("job-fresh", "3600", 0);
        let mut disabled = test_job("job-disabled", "300", 3600);
        disabled.enabled = false;
        state.jobs.insert(overdue.id.clone(), overdue);
        state.jobs.insert(fresh.id.clone(), fresh);
        state.jobs.insert(disabled.id.clone(), disabled);
        let due = due_jobs(&state, Utc::now());
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, "job-overdue");
    }

    #[test]
    fn due_jobs_fires_matching_cron_once_per_minute() {
        let state = state();
        let job = test_job("job-cron", "* * * * *", 3600);
        state.jobs.insert(job.id.clone(), job);
        let at = Utc::now();
        assert_eq!(due_jobs(&state, at).len(), 1);
        state.last_run.insert("job-cron".to_string(), at);
        assert!(due_jobs(&state, at).is_empty());
    }

    #[test]
    fn due_jobs_skips_cron_that_does_not_match_now() {
        use chrono::Timelike;
        let state = state();
        let at = Utc::now();
        let other_minute = (at.minute() + 30) % 60;
        let schedule = format!("{other_minute} * * * *");
        let job = test_job("job-cron", &schedule, 3600);
        state.jobs.insert(job.id.clone(), job);
        assert!(due_jobs(&state, at).is_empty());
    }

    #[test]
    fn run_due_jobs_records_run_and_does_not_refire_immediately() {
        let state = state();
        let job = test_job("job-run", "300", 3600);
        state.jobs.insert(job.id.clone(), job);
        let runs = run_due_jobs(&state);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].job_id, "job-run");
        assert_eq!(runs[0].status, "success");
        assert_eq!(state.runs.len(), 1);
        assert!(state.last_run.contains_key("job-run"));
        assert!(run_due_jobs(&state).is_empty());
    }

    #[test]
    fn start_ticker_is_idempotent() {
        let state = state();
        assert!(!state.ticker_started.load(Ordering::SeqCst));
        // Starting the ticker twice must not spawn a second loop;
        // the flag is the only part observable under test (the loop
        // itself runs in the background).
        start_ticker(state.clone());
        assert!(state.ticker_started.load(Ordering::SeqCst));
        start_ticker(state.clone());
        assert!(state.ticker_started.load(Ordering::SeqCst));
    }

    fn test_app_context() -> unified_core::context::AppContext {
        // `AppContext` has no `Default` (and is not `Clone`), so
        // tests build one from a default config. Construction is
        // pure: the filesystem cache only stores its directory and
        // the storage registry starts empty with default targets.
        unified_core::context::AppContext::new(
            unified_core::config::AppConfig::default(),
            Arc::new(unified_core::hooks::HookBus::new()),
            Arc::new(unified_core::modules::ModuleRegistry::new()),
        )
    }

    #[test]
    fn dispatch_tenant_destroy_due_fails_closed_without_app_context() {
        // A state built by hand (no `Module::init`, so no stored
        // context) must never claim the sweep ran: the arm errors
        // instead of returning `dispatched: true` for work that
        // did not happen.
        let state = state();
        let outcome = dispatch(&state, "tenant.destroy_due", &json!({}));
        assert!(outcome.is_err());
    }

    #[test]
    fn builtin_destroy_due_job_seeded_exactly_once_after_two_inits() {
        // Two `init` calls (two restarts) must leave exactly one
        // job of kind `tenant.destroy_due`, with the hourly
        // default interval, enabled, under the fixed built-in id.
        use unified_core::modules::Module;

        let ctx = test_app_context();
        let module = crate::modules::scheduler::module::SchedulerModule::new();
        module.init(&ctx).expect("first init must succeed");
        module.init(&ctx).expect("second init must succeed");
        let state = module.state_for_test();
        let seeded: Vec<Job> = state
            .jobs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|job| job.kind == "tenant.destroy_due")
            .collect();
        assert_eq!(seeded.len(), 1);
        assert_eq!(seeded[0].id, DESTROY_DUE_JOB_ID);
        assert_eq!(seeded[0].schedule, DESTROY_DUE_INTERVAL_SECS.to_string());
        assert!(seeded[0].enabled);
    }

    #[test]
    fn dispatch_tenant_destroy_due_with_context_and_no_due_requests_succeeds_empty() {
        // With a stored context and zero due requests the sweep
        // runs and reports success with an empty destroyed list.
        // (`process_due_destroys` returns `Ok` with no ids when
        // nothing is due, and any per-request failure is recorded
        // on that request instead of failing the sweep, so the
        // empty list holds even if a stale due request lingers in
        // the shared in-memory store.)
        let state = state();
        let ctx = test_app_context();
        state.set_context(&ctx);
        let outcome = dispatch(&state, "tenant.destroy_due", &json!({}))
            .expect("dispatch with a stored context must succeed");
        assert_eq!(
            outcome.get("dispatched").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            outcome.get("event").and_then(Value::as_str),
            Some("tenant.destroy_due")
        );
        assert_eq!(outcome.get("destroyed"), Some(&json!([])));
    }
}
