// Scheduler - Hook handler for scheduler enqueue events.

use unified_core::hooks::HookContext;
use unified_core::json;
use serde_json::{json, Value};
use std::sync::Arc;

use super::{QueueItem, SchedulerState, new_id, now, spawn_queue_worker, valid_kind};

/// Answer `scheduler.enqueue` (from the Mail module, and any other
/// module): create a queued job from the payload and report its id.
/// Accepts both shapes: `{ "kind", "payload" }`, and the Mail module's
/// `{ "job_id", "queue", "event", "payload" }`, where the kind is
/// derived from `event` (`mail.send_requested` -> `mail.send`) or
/// `queue` (`mail` -> `mail.send`). A supplied `job_id` is kept as the
/// queue item id so the Mail module's `mail.deliver` callback can find
/// the same job; the id is also merged into the stored payload so
/// dispatch routes the item to delivery instead of re-requesting it.
pub(crate) fn on_scheduler_enqueue(state: Arc<SchedulerState>, ctx: &mut HookContext) {
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
                    "backup.verify_requested" => "backup.verify".to_string(),
                    "backup.prune_requested" => "backup.prune".to_string(),
                    "export.run_requested" => "export.run".to_string(),
                    "cluster.sync.retry_due" => "cluster.sync.retry_due".to_string(),
                    "webinar.start_requested" => "webinar.start".to_string(),
                    "webinar.reminder_requested" => "webinar.reminder".to_string(),
                    "appointments.reminders.send_requested" => {
                        "appointments.reminders.send".to_string()
                    }
                    "marketing.campaign.send" => "marketing.campaign.send".to_string(),
                    "marketing.sequence.process_due" => {
                        "marketing.sequence.process_due".to_string()
                    }
                    "field_service.plans.check_requested" => {
                        "field_service.plans.check".to_string()
                    }
                    "calendar.feeds.prune_requested" => "calendar.feeds.prune".to_string(),
                    "holidays.sync_requested" => "holidays.sync".to_string(),
                    "holidays.sync_all_requested" => "holidays.sync_all".to_string(),
                    "tenant.destroy_due" => "tenant.destroy_due".to_string(),
                    "approvals.sweep_expiry_requested" => "approvals.sweep_expiry".to_string(),
                    "license.expiry.check_requested" => "license.expiry.check".to_string(),
                    "auditors.expire_requested" => "auditors.expire".to_string(),
                    "logistics.maintenance.remind_requested" => {
                        "logistics.maintenance.remind".to_string()
                    }
                    "web3.poll_requested" => "web3.poll".to_string(),
                    "timetracker.screenshots.purge_requested" => {
                        "timetracker.screenshots.purge".to_string()
                    }
                    "storage.lifecycle.run_requested" => "storage.lifecycle.run".to_string(),
                    "storage.byo.health_check_requested" => {
                        "storage.byo.health_check".to_string()
                    }
                    "events.reminders.send_requested" => "events.reminders.send".to_string(),
                    "todos.reminders.send_requested" => "todos.reminders.send".to_string(),
                    "saas_domains.reservations.expire" => {
                        "saas_domains.reservations.expire".to_string()
                    }
                    "ssl.cloudflare.recheck" => "ssl.cloudflare.recheck".to_string(),
                    "advisor.evaluate_requested" => "advisor.evaluate".to_string(),
                    "billing.subscriptions.scan" => "billing.subscriptions.scan".to_string(),
                    "saas.product_subdomain.expire_grace" => {
                        "saas.product_subdomain.expire_grace".to_string()
                    }
                    "workflows.run_requested" => "workflows.run".to_string(),
                    "contracts.expiry_alert_requested" => {
                        "contracts.expiry_alert".to_string()
                    }
                    "assets.warranty.check_requested" => {
                        "assets.warranty.check".to_string()
                    }
                    "inbox.unsnooze_requested" => "inbox.unsnooze".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::hooks::HookContext;

    fn enqueue_by_event(event: &str) -> (Arc<SchedulerState>, HookContext) {
        let state = Arc::new(SchedulerState::new());
        let mut ctx = HookContext::new(
            "scheduler.enqueue",
            json!({ "event": event, "payload": { "probe": true } }),
        );
        on_scheduler_enqueue(state.clone(), &mut ctx);
        (state, ctx)
    }

    #[test]
    fn enqueue_maps_workflows_run_requested_event() {
        let (state, ctx) = enqueue_by_event("workflows.run_requested");
        assert_eq!(
            ctx.results.first().and_then(|r| r.get("queued")).and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(state.queue.iter().count(), 1);
        assert_eq!(
            state.queue.iter().next().map(|e| e.value().kind.clone()),
            Some("workflows.run".to_string())
        );
    }

    #[test]
    fn enqueue_maps_contracts_expiry_alert_requested_event() {
        let (state, ctx) = enqueue_by_event("contracts.expiry_alert_requested");
        assert_eq!(
            ctx.results.first().and_then(|r| r.get("queued")).and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            state.queue.iter().next().map(|e| e.value().kind.clone()),
            Some("contracts.expiry_alert".to_string())
        );
    }

    #[test]
    fn enqueue_maps_assets_warranty_check_requested_event() {
        let (state, ctx) = enqueue_by_event("assets.warranty.check_requested");
        assert_eq!(
            ctx.results.first().and_then(|r| r.get("queued")).and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            state.queue.iter().next().map(|e| e.value().kind.clone()),
            Some("assets.warranty.check".to_string())
        );
    }

    #[test]
    fn enqueue_maps_inbox_unsnooze_requested_event() {
        let (state, ctx) = enqueue_by_event("inbox.unsnooze_requested");
        assert_eq!(
            ctx.results.first().and_then(|r| r.get("queued")).and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            state.queue.iter().next().map(|e| e.value().kind.clone()),
            Some("inbox.unsnooze".to_string())
        );
    }

    #[test]
    fn enqueue_rejects_unknown_event_name() {
        let (_state, ctx) = enqueue_by_event("nothing.known_requested");
        assert_eq!(
            ctx.results.first().and_then(|r| r.get("queued")).and_then(Value::as_bool),
            Some(false)
        );
    }
}
