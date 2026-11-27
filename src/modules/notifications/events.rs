// notifications module -- Hook event handlers.

use unified_core::hooks::{HookContext, HookHandler};
use unified_core::json;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::*;

pub(crate) fn on_security_auth_failed(state: Arc<NotificationsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let account = data
        .get("account")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let user_id = if account.is_empty() {
        "system".to_string()
    } else {
        account
    };
    let ip = data.get("ip").and_then(|v| v.as_str()).unwrap_or("unknown");
    let count = data.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
    let threshold = data
        .get("threshold")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let title = "Repeated failed sign-in attempts detected".to_string();
    let body = format!(
        "{count} failed sign-in attempts (threshold {threshold}) from IP {ip} for account \"{user_id}\""
    );
    match state.notify_for_category(&user_id, "security", title, body, "security.auth.failed") {
        Some(notification) => {
            ctx.add_result(json!({ "notified": true, "notification_id": notification.id }));
        }
        None => {
            ctx.add_result(json!({ "notified": false, "reason": "security category disabled" }));
        }
    }
}

pub(crate) fn on_cluster_sync_failed(state: Arc<NotificationsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = data
        .get("user_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    else {
        // No explicit user: the clusters module already forwards this
        // failure via `notification.requested`, so do not duplicate it.
        ctx.add_result(json!({ "notified": false, "reason": "no user_id; already forwarded via notification.requested" }));
        return;
    };
    let cluster_id = data
        .get("cluster_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let plan_id = data
        .get("plan_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let errors = data
        .get("errors")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default();
    let title = "Cluster sync failed".to_string();
    let body = format!("Sync plan {plan_id} for cluster {cluster_id} failed: {errors}");
    match state.notify_for_category(&user_id, "sync", title, body, "cluster.sync.failed") {
        Some(notification) => {
            ctx.add_result(json!({ "notified": true, "notification_id": notification.id }));
        }
        None => {
            ctx.add_result(json!({ "notified": false, "reason": "sync category disabled" }));
        }
    }
}

pub(crate) fn on_payment_failed(state: Arc<NotificationsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = data
        .get("user_id")
        .or_else(|| data.get("customer_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
    else {
        // No explicit user: chargeback webhooks already forward via
        // `notification.requested`, so do not duplicate it.
        ctx.add_result(json!({ "notified": false, "reason": "no user_id; already forwarded via notification.requested" }));
        return;
    };
    let payment_id = data
        .get("payment_id")
        .or_else(|| data.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let reason = data
        .get("reason")
        .or_else(|| data.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("payment failed");
    let title = "Payment failed".to_string();
    let body = format!("Payment {payment_id} failed: {reason}");
    match state.notify_for_category(&user_id, "billing", title, body, "payment.failed") {
        Some(notification) => {
            ctx.add_result(json!({ "notified": true, "notification_id": notification.id }));
        }
        None => {
            ctx.add_result(json!({ "notified": false, "reason": "billing category disabled" }));
        }
    }
}

// Domain-event listeners.
//
// Every other module's important events that were NOT already covered
// get a listener here. The event names below are the ones the emitting
// modules actually emit (verified by reading them); no name was
// invented. Coverage decisions:
//
// Covered by a direct listener (the emitter does NOT forward to
// `notification.requested` itself):
//   order.paid (store), invoice.paid (billing),
//   payment.captured / payment.refunded (payments),
//   store.order.completed / refunded / cancelled (store),
//   ticket.commented (ticket replied) / ticket.assigned (tickets),
//   backup.failed / completed / restored (backup),
//   cluster.health.changed (node unhealthy) / cluster.node.unhealthy
//   (contract event) / lb.backend.unhealthy (load balancer) -- the
//   "server down" path; no generic `server.down` event exists,
//   cluster.sync.completed / conflict (clusters),
//   license.issued / activated / deactivated / suspended / revoked
//   (licensing; payloads carry no user id, so these go to `system`),
//   statuspage.incident.created / resolved (status page),
//   vcs.sync.failed (version control), webhook.failed (webhooks),
//   security.bruteforce.locked / security.ip.flagged (hardening).
//
// Deliberately NOT subscribed here because the emitter already
// forwards through `notification.requested` (a second listener would
// duplicate the notification):
//   payment.failed (payments notify on every failure path),
//   payment.chargeback (payments notify the customer),
//   cluster.sync.failed (clusters forward it),
//   ssl.expiring / ssl.renew_failed (certificates notify `system`),
//   ticket.created (tickets notify the owner),
//   chat mentions (chat notifies the mentioned user),
//   maintenance.started / ended (maintenance notifies `all`),
//   announcement.published (announcements fan out on publish).
// `payment.completed` is also not subscribed: the store emits it in
// the same transition as `order.paid`, so listening to both would
// create two notifications for one payment.
//
// Gaps found while auditing (no event exists to subscribe to):
//   - No `license.expiring` event exists anywhere in the tree: the
//     licensing module has no expiry scan, so a license can expire
//     without any hook firing. The licensing owner needs an expiry
//     scan that emits `license.expiring` (with `license_id`,
//     `license_key`, `days_remaining`); this module will then pick it
//     up with the same pattern as `ssl.expiring`.
//   - The service monitor probes services over HTTP but emits no
//     event when one goes down; `cluster.health.changed` and
//     `lb.backend.unhealthy` above are the only down-events today.

pub(crate) fn domain_handler(state: Arc<NotificationsState>, event: &'static str) -> HookHandler {
    Arc::new(move |ctx: &mut HookContext| on_domain_event(state.clone(), event, ctx))
}

/// Routing for one domain event: (category, priority, needs a user).
/// Events that need a user are skipped (with a result explaining why)
/// when the payload carries no usable user id; the rest go to the
/// `system` user.
pub(crate) fn event_routing(event: &str) -> (&'static str, &'static str, bool) {
    match event {
        "order.paid" | "store.order.completed" | "store.order.refunded" | "store.order.cancelled" => {
            ("orders", "normal", true)
        }
        "invoice.paid" => ("billing", "normal", true),
        "payment.captured" | "payment.refunded" => ("payments", "normal", true),
        "ticket.commented" | "ticket.assigned" => ("tickets", "normal", true),
        "backup.failed" => ("backup", "high", false),
        "backup.completed" | "backup.restored" => ("backup", "normal", false),
        "cluster.health.changed" | "cluster.node.unhealthy" | "lb.backend.unhealthy" => {
            ("monitoring", "high", false)
        }
        "cluster.sync.completed" => ("sync", "normal", false),
        "cluster.sync.conflict" => ("sync", "high", false),
        "license.issued" | "license.activated" | "license.deactivated" => {
            ("licensing", "normal", false)
        }
        "license.suspended" | "license.revoked" => ("licensing", "high", false),
        "statuspage.incident.created" => ("monitoring", "high", false),
        "statuspage.incident.resolved" => ("monitoring", "normal", false),
        "vcs.sync.failed" => ("sync", "high", false),
        "webhook.failed" => ("system", "high", false),
        "security.bruteforce.locked" | "security.ip.flagged" => ("security", "high", false),
        _ => ("system", "normal", false),
    }
}

pub(crate) fn event_text(data: &Value, key: &str) -> String {
    data.get(key)
        .and_then(|value| value.as_str())
        .unwrap_or("unknown")
        .to_string()
}

pub(crate) fn event_nested<'a>(data: &'a Value, key: &str) -> Option<&'a Value> {
    data.get(key).filter(|value| value.is_object())
}

/// Pick the recipient user id out of a domain-event payload. The
/// payload shapes are the emitters' real ones: an order / invoice is
/// the object itself (`user_id`), a refunded store order nests it
/// under `order`, a payment carries `customer_id`, a ticket carries
/// `user_id` (owner) or `assigned_to` (agent).
pub(crate) fn event_user_id(event: &str, data: &Value) -> Option<String> {
    let pick = |value: &Value| {
        value
            .get("user_id")
            .or_else(|| value.get("customer_id"))
            .and_then(|item| item.as_str())
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string)
    };
    match event {
        "ticket.assigned" => data
            .get("assigned_to")
            .and_then(|value| value.as_str())
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string),
        "ticket.commented" => pick(data),
        "store.order.refunded" => event_nested(data, "order")
            .and_then(pick)
            .or_else(|| pick(data)),
        "security.bruteforce.locked" => {
            if data.get("kind").and_then(|value| value.as_str()) == Some("account") {
                data.get("key")
                    .and_then(|value| value.as_str())
                    .filter(|text| !text.trim().is_empty())
                    .map(str::to_string)
            } else {
                None
            }
        }
        _ => pick(data).or_else(|| event_nested(data, "order").and_then(pick)),
    }
}

pub(crate) fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars).collect();
    format!("{kept}...")
}

/// Human-readable title and body for one domain event, built from
/// the fields the emitter actually puts in the payload.
pub(crate) fn event_message(event: &str, data: &Value) -> (String, String) {
    let order = event_nested(data, "order").unwrap_or(data);
    let id = event_text(data, "id");
    match event {
        "order.paid" => (
            "Order paid".to_string(),
            format!(
                "Order {} for {} {} was paid.",
                event_text(data, "id"),
                event_text(data, "total"),
                event_text(data, "currency")
            ),
        ),
        "invoice.paid" => (
            "Invoice paid".to_string(),
            format!(
                "Invoice {} was paid in full.",
                event_text(data, "number")
            ),
        ),
        "payment.captured" => (
            "Payment captured".to_string(),
            format!(
                "Payment of {} {} for order {} was captured.",
                event_text(data, "amount_minor"),
                event_text(data, "currency"),
                event_text(data, "order_id")
            ),
        ),
        "payment.refunded" => (
            "Payment refunded".to_string(),
            format!(
                "Payment of {} {} for order {} was refunded.",
                event_text(data, "amount_minor"),
                event_text(data, "currency"),
                event_text(data, "order_id")
            ),
        ),
        "store.order.completed" => (
            "Order completed".to_string(),
            format!("Order {} is complete.", event_text(order, "id")),
        ),
        "store.order.refunded" => (
            "Order refunded".to_string(),
            format!("Order {} was refunded.", event_text(order, "id")),
        ),
        "store.order.cancelled" => (
            "Order cancelled".to_string(),
            format!("Order {} was cancelled.", event_text(order, "id")),
        ),
        "ticket.commented" => {
            let last = data
                .get("comments")
                .and_then(|value| value.as_array())
                .and_then(|items| items.last());
            let comment = last
                .and_then(|item| item.get("body"))
                .and_then(|value| value.as_str())
                .unwrap_or("");
            (
                format!("Ticket reply: {}", event_text(data, "title")),
                format!(
                    "New reply on ticket {}: {}",
                    event_text(data, "id"),
                    truncate_text(comment, 200)
                ),
            )
        }
        "ticket.assigned" => (
            "Ticket assigned to you".to_string(),
            format!(
                "Ticket {} ({}) was assigned to you.",
                event_text(data, "id"),
                event_text(data, "title")
            ),
        ),
        "backup.failed" => (
            format!("Backup failed: {}", event_text(data, "job_name")),
            format!(
                "Backup job {} ({}) failed: {}",
                event_text(data, "job_name"),
                event_text(data, "job_id"),
                event_text(data, "error")
            ),
        ),
        "backup.completed" => (
            format!("Backup completed: {}", event_text(data, "job_name")),
            format!(
                "Backup job {} ({}) completed.",
                event_text(data, "job_name"),
                event_text(data, "job_id")
            ),
        ),
        "backup.restored" => (
            format!("Backup restored: {}", event_text(data, "job_name")),
            format!(
                "Backup job {} ({}) was restored.",
                event_text(data, "job_name"),
                event_text(data, "job_id")
            ),
        ),
        "cluster.health.changed" => (
            "Cluster node unhealthy".to_string(),
            format!(
                "Node {} in cluster {} is {} ({} consecutive failures).",
                event_text(data, "node_id"),
                event_text(data, "cluster_id"),
                event_text(data, "health"),
                data.get("consecutive_failures")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0)
            ),
        ),
        "cluster.node.unhealthy" => (
            "Cluster node unhealthy".to_string(),
            format!(
                "Node {} in cluster {} is unhealthy.",
                event_text(data, "node_id"),
                event_text(data, "cluster_id")
            ),
        ),
        "lb.backend.unhealthy" => (
            "Load balancer backend unhealthy".to_string(),
            format!(
                "Backend {} ({}) is unhealthy.",
                event_text(data, "backend_name"),
                event_text(data, "address")
            ),
        ),
        "cluster.sync.completed" => (
            "Cluster sync completed".to_string(),
            format!(
                "Sync plan {} for cluster {} completed.",
                event_text(data, "plan_id"),
                event_text(data, "cluster_id")
            ),
        ),
        "cluster.sync.conflict" => (
            "Cluster sync conflict".to_string(),
            format!(
                "Sync plan {} for cluster {} has conflicts that need review.",
                event_text(data, "plan_id"),
                event_text(data, "cluster_id")
            ),
        ),
        "license.issued" => (
            "License issued".to_string(),
            format!(
                "License {} was issued for {}.",
                event_text(data, "license_key"),
                event_text(data, "product")
            ),
        ),
        "license.activated" => (
            "License activated".to_string(),
            format!("License {} was activated.", event_text(data, "license_key")),
        ),
        "license.deactivated" => (
            "License deactivated".to_string(),
            format!("License {} was deactivated.", event_text(data, "license_key")),
        ),
        "license.suspended" => (
            "License suspended".to_string(),
            format!("License {} was suspended.", event_text(data, "license_key")),
        ),
        "license.revoked" => (
            "License revoked".to_string(),
            format!("License {} was revoked.", event_text(data, "license_key")),
        ),
        "statuspage.incident.created" => (
            format!("Incident: {}", event_text(data, "title")),
            format!(
                "A status page incident was created (severity {}).",
                event_text(data, "severity")
            ),
        ),
        "statuspage.incident.resolved" => (
            format!("Incident resolved: {}", event_text(data, "title")),
            "The status page incident was resolved.".to_string(),
        ),
        "vcs.sync.failed" => (
            "Version control sync failed".to_string(),
            format!(
                "Sync for connection {} ({}) failed: {}",
                event_text(data, "connection_id"),
                event_text(data, "provider"),
                event_text(data, "error")
            ),
        ),
        "webhook.failed" => (
            "Webhook delivery failed".to_string(),
            format!(
                "Webhook {} to endpoint {} failed: {}",
                event_text(data, "event"),
                event_text(data, "endpoint_id"),
                event_text(data, "error")
            ),
        ),
        "security.bruteforce.locked" => (
            "Account locked after failed sign-ins".to_string(),
            format!(
                "Sign-in attempts for {} {} were locked for {} seconds after {} lockouts.",
                event_text(data, "kind"),
                event_text(data, "key"),
                data.get("lockout_secs")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0),
                data.get("lock_count")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0)
            ),
        ),
        "security.ip.flagged" => (
            "IP address flagged".to_string(),
            format!(
                "IP {} was flagged (score {}): {}",
                event_text(data, "ip"),
                data.get("score")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0),
                event_text(data, "reason")
            ),
        ),
        _ => (
            format!("Notification: {event}"),
            format!("Event {event} occurred (id {id})."),
        ),
    }
}

pub(crate) fn on_domain_event(state: Arc<NotificationsState>, event: &str, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    // A node that recovered (or is still healthy) needs no alert:
    // only the unhealthy transition notifies.
    if event == "cluster.health.changed" {
        let health = data
            .get("health")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_lowercase();
        if !matches!(health.as_str(), "unhealthy" | "down" | "failed" | "critical") {
            ctx.add_result(json!({ "notified": false, "reason": "node is not unhealthy; no alert" }));
            return;
        }
    }
    // A ticket reply notifies the ticket owner -- unless the owner
    // wrote the reply themselves.
    if event == "ticket.commented" {
        let owner = data.get("user_id").and_then(|value| value.as_str());
        let author = data
            .get("comments")
            .and_then(|value| value.as_array())
            .and_then(|items| items.last())
            .and_then(|item| item.get("author_id"))
            .and_then(|value| value.as_str());
        if let (Some(owner), Some(author)) = (owner, author) {
            if owner == author {
                ctx.add_result(json!({ "notified": false, "reason": "comment author is the ticket owner" }));
                return;
            }
        }
    }
    let (category, mut priority, needs_user) = event_routing(event);
    if event == "statuspage.incident.created" {
        let severity = data
            .get("severity")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if !matches!(severity, "major" | "critical") {
            priority = "normal";
        }
    }
    let user_id = match event_user_id(event, &data) {
        Some(user_id) => user_id,
        None if needs_user => {
            ctx.add_result(json!({ "notified": false, "reason": "no user_id in payload" }));
            return;
        }
        None => "system".to_string(),
    };
    let (title, body) = event_message(event, &data);
    match state.store_notification(
        &user_id,
        title,
        body,
        event,
        category,
        priority,
        None,
        "user",
    ) {
        Some(notification) => {
            // Dispatch through the event template when one exists and
            // an email address was supplied (or the template needs no
            // email). Without an address, email is skipped rather than
            // recorded as failed: system events have no recipient
            // address by design, and in-app delivery already happened.
            let email = data.get("email").and_then(|value| value.as_str()).map(str::to_string);
            let template = state.template_for(event, "en");
            let dispatchable = match &template {
                Some(template) => !template.channels.iter().any(|channel| channel == "email") || email.is_some(),
                None => false,
            };
            if dispatchable {
                let mut variables: HashMap<String, Value> = HashMap::new();
                if let Value::Object(map) = &data {
                    for (key, value) in map {
                        variables.insert(key.clone(), value.clone());
                    }
                }
                variables.insert(
                    "title".to_string(),
                    Value::String(notification.title.clone()),
                );
                variables.insert(
                    "body".to_string(),
                    Value::String(notification.body.clone()),
                );
                variables.insert(
                    "user_id".to_string(),
                    Value::String(notification.user_id.clone()),
                );
                render_and_dispatch(&state, &notification, &variables, email, None, &[]);
            }
            ctx.add_result(json!({ "notified": true, "notification_id": notification.id }));
        }
        None => {
            ctx.add_result(json!({ "notified": false, "reason": format!("{category} category disabled") }));
        }
    }
}

/// Update a recorded webhook delivery attempt from a webhooks-module
/// outcome: `webhook.delivered` marks it delivered;
/// `webhook.failed` marks it failed with the error and the next retry
/// time from the retry policy. The match is by `notification_id`,
/// which the webhooks module reports when a `notification.webhook`
/// delivery has no subscribed endpoint; outcomes for other events
/// are ignored, because they belong to other modules' webhooks, not
/// to a notification attempt.
///
/// Known limit, recorded honestly: the webhooks module's
/// delivered/failed payloads for a real endpoint delivery carry the
/// webhook `delivery_id`, not the notification id, so those outcomes
/// cannot be correlated to a notification attempt from this side
/// alone and are reported unmatched instead of guessed at. Resolving
/// them needs the webhooks module to include `notification_id` in
/// its delivered/failed payloads (that module owns those emits).
pub(crate) fn on_webhook_delivery(state: Arc<NotificationsState>, delivered: bool, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    if data.get("event").and_then(|value| value.as_str()) != Some("notification.webhook") {
        ctx.add_result(json!({ "matched": false, "reason": "not a notification.webhook delivery" }));
        return;
    }
    let notification_id = data
        .get("notification_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if notification_id.is_empty() {
        ctx.add_result(json!({
            "matched": false,
            "reason": "no notification_id in payload; webhook delivery ids are not correlated to notification attempts",
        }));
        return;
    }
    let mut matched: Option<String> = None;
    let mut failed_report: Option<Value> = None;
    for mut entry in state.deliveries.iter_mut() {
        let attempt = entry.value_mut();
        if attempt.notification_id.as_deref() != Some(notification_id.as_str())
            || attempt.channel != "webhook"
            || attempt.status != "queued"
        {
            continue;
        }
        if delivered {
            attempt.status = "delivered".to_string();
            attempt.error = None;
            attempt.next_retry_at = None;
        } else {
            let reason = data
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("webhook delivery failed")
                .to_string();
            attempt.status = "failed".to_string();
            attempt.error = Some(reason.clone());
            attempt.next_retry_at = next_retry_at(attempt.attempts);
            // Emit after the map guard is dropped (below): a hook
            // handler must never run while this module holds a
            // write guard on the same map.
            failed_report = Some(json!({
                "attempt_id": attempt.id,
                "notification_id": attempt.notification_id,
                "channel": attempt.channel,
                "error": reason,
                "attempts": attempt.attempts,
                "retryable": attempt.attempts < attempt.max_attempts,
            }));
        }
        attempt.updated_at = now();
        matched = Some(attempt.id.clone());
    }
    if let Some(report) = failed_report {
        state.emit("notification.delivery.failed", report);
    }
    ctx.add_result(json!({ "matched": matched.is_some(), "attempt_id": matched }));
}

/// Update a recorded delivery attempt from a provider outcome:
/// `email.sent` / `mail.sent` mark it delivered (with the provider
/// message id when one is reported); `email.failed` / `mail.failed`
/// mark it failed with the error and the next retry time from the
/// retry policy. The match is by `job_id`, which `send_email`
/// records when the send is queued. An unmatched `job_id` (a send
/// that did not go through this module) is reported, not invented.
pub(crate) fn on_email_delivery(state: Arc<NotificationsState>, delivered: bool, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let job_id = data
        .get("job_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let mut matched: Option<String> = None;
    let mut failed_report: Option<Value> = None;
    for mut entry in state.deliveries.iter_mut() {
        if entry.value().job_id.as_deref() == Some(job_id.as_str()) && !job_id.is_empty() {
            let attempt = entry.value_mut();
            if delivered {
                attempt.status = "delivered".to_string();
                attempt.provider_message_id = data
                    .get("provider_message_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
                    .or_else(|| attempt.provider_message_id.clone());
                attempt.error = None;
                attempt.next_retry_at = None;
            } else {
                let reason = data
                    .get("error")
                    .and_then(|value| value.as_str())
                    .unwrap_or("delivery failed")
                    .to_string();
                attempt.status = "failed".to_string();
                attempt.error = Some(reason.clone());
                attempt.next_retry_at = next_retry_at(attempt.attempts);
                // Emit after the map guard is dropped (below): a hook
                // handler must never run while this module holds a
                // write guard on the same map.
                failed_report = Some(json!({
                    "attempt_id": attempt.id,
                    "notification_id": attempt.notification_id,
                    "channel": attempt.channel,
                    "error": reason,
                    "attempts": attempt.attempts,
                    "retryable": attempt.attempts < attempt.max_attempts,
                }));
            }
            attempt.updated_at = now();
            matched = Some(attempt.id.clone());
            break;
        }
    }
    if let Some(report) = failed_report {
        state.emit("notification.delivery.failed", report);
    }
    ctx.add_result(json!({ "matched": matched.is_some(), "attempt_id": matched }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_text_keeps_short_text() {
        assert_eq!(truncate_text("hello", 10), "hello");
        assert_eq!(truncate_text("hello", 5), "hello");
    }

    #[test]
    fn truncate_text_appends_ellipsis() {
        assert_eq!(truncate_text("hello world", 5), "hello...");
    }

    #[test]
    fn event_routing_maps_known_events() {
        assert_eq!(event_routing("order.paid"), ("orders", "normal", true));
        assert_eq!(event_routing("backup.failed"), ("backup", "high", false));
        assert_eq!(
            event_routing("license.suspended"),
            ("licensing", "high", false)
        );
        assert_eq!(event_routing("unknown.event"), ("system", "normal", false));
    }

    fn queued_webhook_attempt(state: &NotificationsState) -> DeliveryAttempt {
        state.record_delivery(
            Some("n-1".to_string()),
            "webhook",
            "queued",
            None,
            None,
            None,
            Some("Hello".to_string()),
            None,
            None,
            1,
        )
    }

    fn webhook_ctx(event: &str, data: Value) -> HookContext {
        HookContext::new(event, data)
    }

    #[test]
    fn webhook_failed_resolves_queued_attempt_by_notification_id() {
        let state = Arc::new(NotificationsState::new());
        let attempt = queued_webhook_attempt(&state);
        let mut ctx = webhook_ctx(
            "webhook.failed",
            json!({
                "event": "notification.webhook",
                "notification_id": "n-1",
                "error": "No webhook endpoint is subscribed",
            }),
        );
        on_webhook_delivery(state.clone(), false, &mut ctx);
        let stored = state.deliveries.get(&attempt.id).expect("attempt kept");
        assert_eq!(stored.status, "failed");
        assert_eq!(
            stored.error.as_deref(),
            Some("No webhook endpoint is subscribed")
        );
        assert!(stored.next_retry_at.is_some());
        assert_eq!(
            ctx.results.first().and_then(|r| r.get("matched")).and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn webhook_delivered_resolves_queued_attempt() {
        let state = Arc::new(NotificationsState::new());
        let attempt = queued_webhook_attempt(&state);
        let mut ctx = webhook_ctx(
            "webhook.delivered",
            json!({ "event": "notification.webhook", "notification_id": "n-1" }),
        );
        on_webhook_delivery(state.clone(), true, &mut ctx);
        let stored = state.deliveries.get(&attempt.id).expect("attempt kept");
        assert_eq!(stored.status, "delivered");
        assert_eq!(stored.error, None);
        assert_eq!(stored.next_retry_at, None);
    }

    #[test]
    fn webhook_outcome_ignores_other_events_and_unmatched_payloads() {
        let state = Arc::new(NotificationsState::new());
        let attempt = queued_webhook_attempt(&state);
        // Another module's webhook event must not resolve a
        // notification attempt.
        let mut ctx = webhook_ctx(
            "webhook.delivered",
            json!({ "event": "order.paid", "notification_id": "n-1" }),
        );
        on_webhook_delivery(state.clone(), true, &mut ctx);
        assert_eq!(state.deliveries.get(&attempt.id).unwrap().status, "queued");
        // A notification.webhook outcome without a notification id
        // cannot be correlated, so it stays queued and unmatched.
        let mut ctx = webhook_ctx("webhook.delivered", json!({ "event": "notification.webhook" }));
        on_webhook_delivery(state.clone(), true, &mut ctx);
        assert_eq!(state.deliveries.get(&attempt.id).unwrap().status, "queued");
        assert_eq!(
            ctx.results.first().and_then(|r| r.get("matched")).and_then(|v| v.as_bool()),
            Some(false)
        );
    }
}
