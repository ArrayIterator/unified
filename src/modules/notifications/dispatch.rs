// notifications module -- Outbound dispatch: email, channels, templates, and recipients.

use unified_core::hooks::HookContext;
use unified_core::json;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::*;

pub(crate) fn on_notification_requested(state: Arc<NotificationsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let kind = data
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("general")
        .to_string();
    let (recipients, audience_label) = match resolve_recipients(&state, &data) {
        Ok(resolved) => resolved,
        Err(error) => {
            state.emit(
                "notification.audience.unresolved",
                json!({
                    "audience": data.get("audience"),
                    "kind": kind,
                    "error": error,
                }),
            );
            ctx.add_result(json!({ "error": error, "delivered": 0 }));
            return;
        }
    };
    // Chat mentions carry no title/body, so synthesize readable ones
    // from the kind instead of storing an empty notification.
    let title = data
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| match kind.as_str() {
            "chat.mention" => "You were mentioned in chat".to_string(),
            "ticket" => "Ticket update".to_string(),
            "invoice" => "Invoice update".to_string(),
            "license" => "License update".to_string(),
            other => format!("Notification: {other}"),
        });
    let body = data
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let category = data
        .get("category")
        .and_then(|v| v.as_str())
        .filter(|value| NOTIFICATION_CATEGORIES.contains(value))
        .map(str::to_string);
    let module = data
        .get("module")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let priority = data
        .get("priority")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let link = data
        .get("link")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let email_template_id = data
        .get("email_template_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mut requested_channels: Vec<String> = data
        .get("channels")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if requested_channels.is_empty() {
        if let Some(channel) = data.get("channel").and_then(|v| v.as_str()) {
            requested_channels.push(channel.to_string());
        }
    }
    // An unknown channel must fail the request here, not silently
    // become a delivered in-app record downstream: the HTTP handlers
    // validate channels, and the hook path must hold the same rule.
    if !requested_channels.is_empty() {
        if let Err(error) = validate_channels(&requested_channels) {
            ctx.add_result(json!({ "error": error.message, "delivered": 0 }));
            return;
        }
    }
    let mut variables: HashMap<String, Value> = HashMap::new();
    if let Value::Object(map) = &data {
        for (key, value) in map {
            variables.insert(key.clone(), value.clone());
        }
    }
    variables.insert("title".to_string(), Value::String(title.clone()));
    variables.insert("body".to_string(), Value::String(body.clone()));
    let mut delivered = 0usize;
    let mut notification_ids: Vec<String> = Vec::new();
    for (user_id, email) in recipients {
        variables.insert("user_id".to_string(), Value::String(user_id.clone()));
        let input = CreateNotification {
            user_id: user_id.clone(),
            user_ids: Vec::new(),
            emails: Vec::new(),
            audience: Some(audience_label.clone()),
            title: title.clone(),
            body: body.clone(),
            kind: Some(kind.clone()),
            category: category.clone(),
            module: module.clone(),
            priority: priority.clone(),
            link: link.clone(),
            channels: requested_channels.clone(),
            email_template_id: email_template_id.clone(),
            email: email.clone(),
        };
        match state.create_notification(input) {
            Ok(notification) => {
                notification_ids.push(notification.id.clone());
                delivered += 1;
                render_and_dispatch(
                    &state,
                    &notification,
                    &variables,
                    email,
                    email_template_id.as_deref(),
                    &requested_channels,
                );
            }
            Err(error) => {
                ctx.add_result(json!({ "error": error.message, "user_id": user_id }));
            }
        }
    }
    ctx.add_result(json!({ "delivered": delivered, "notification_ids": notification_ids }));
}

/// Send one email through the real chain, in order:
/// `email.send_requested` (Email Providers module, per-connection id;
/// payload `{ connection_id?, to, subject, body_text?, template_id?,
/// variables?, metadata? }` -- exactly the shape its hook handler
/// answers) and, only when no provider handler queues it, the Mail
/// Manager fallback `mail.send_requested` (`{ to, subject, body_text?,
/// template_id?, variables? }`). A `template_id` hands rendering to
/// the GrapeJS Email Templates module via `template.render_requested`
/// inside the provider / mail worker; this function never renders a
/// template itself. Returns the outcome; the caller records it as a
/// delivery attempt. Nothing here is silent: when neither handler
/// answers, the outcome carries the error.
pub(crate) fn send_email(
    state: &Arc<NotificationsState>,
    to: &str,
    subject: &str,
    body_text: &str,
    template_id: Option<&str>,
    variables: &HashMap<String, Value>,
    connection_id: Option<String>,
    metadata: Value,
) -> EmailSendOutcome {
    let mut payload = json!({
        "connection_id": connection_id,
        "to": to,
        "subject": subject,
        "body_text": body_text,
        "metadata": metadata,
    });
    if let Some(template_id) = template_id {
        payload["template_id"] = Value::String(template_id.to_string());
        payload["variables"] =
            serde_json::to_value(variables).unwrap_or(Value::Object(Default::default()));
    }
    let provider_answer = state.bus().emit("email.send_requested", payload);
    if let Some(result) = provider_answer.results.first() {
        if result.get("error").is_none()
            && result
                .get("queued")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        {
            return EmailSendOutcome {
                queued: true,
                job_id: result
                    .get("job_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                via: "email_providers",
                error: None,
            };
        }
    }
    let mut mail_payload = json!({
        "to": to,
        "subject": subject,
        "body_text": body_text,
    });
    if let Some(template_id) = template_id {
        mail_payload["template_id"] = Value::String(template_id.to_string());
        mail_payload["variables"] =
            serde_json::to_value(variables).unwrap_or(Value::Object(Default::default()));
    }
    let mail_answer = state.bus().emit("mail.send_requested", mail_payload);
    if let Some(result) = mail_answer.results.first() {
        if result.get("error").is_none()
            && result
                .get("queued")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        {
            return EmailSendOutcome {
                queued: true,
                job_id: result
                    .get("job_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                via: "mail",
                error: None,
            };
        }
        if let Some(error) = result.get("error").and_then(|value| value.as_str()) {
            return EmailSendOutcome {
                queued: false,
                job_id: None,
                via: "mail",
                error: Some(error.to_string()),
            };
        }
    }
    EmailSendOutcome {
        queued: false,
        job_id: None,
        via: "none",
        error: Some("No email handler answered: neither an email provider nor the Mail Manager is registered".to_string()),
    }
}

/// Dispatch one stored notification through an explicit channel list
/// (used when no notification template exists for the event).
/// Email goes through `send_email` (providers first, Mail Manager
/// fallback); a webhook delivery is emitted as
/// `notification.webhook` for the webhooks module; in-app delivery
/// already happened (the notification is stored) and is recorded as
/// `delivered`. Every channel records a delivery attempt. Broadcast
/// notifications (`user_id` "all") have no single user whose
/// preferences could apply, so their channels are used as given;
/// per-recipient notifications respect the recipient's effective
/// channels (per-category override, else global). A user who disabled
/// a channel gets a `skipped` attempt for it, never a delivery.

pub(crate) fn dispatch_channels(
    state: &Arc<NotificationsState>,
    notification: &Notification,
    channels: &[String],
    email: Option<String>,
    email_template_id: Option<&str>,
    variables: &HashMap<String, Value>,
    connection_id: Option<String>,
) {
    let effective = if notification.user_id == "all" {
        channels.to_vec()
    } else {
        state.effective_channels(&notification.user_id, &notification.category)
    };
    let notification_id = Some(notification.id.clone());
    for channel in channels {
        match channel.as_str() {
            "email" => {
                if !effective.iter().any(|item| item == "email") {
                    state.record_delivery(
                        notification_id.clone(),
                        "email",
                        "skipped",
                        None,
                        None,
                        email.clone(),
                        Some(notification.title.clone()),
                        email_template_id.map(str::to_string),
                        Some("email channel is disabled for this user".to_string()),
                        0,
                    );
                    continue;
                }
                let Some(to) = email.clone() else {
                    let attempt = state.record_delivery(
                        notification_id.clone(),
                        "email",
                        "failed",
                        None,
                        None,
                        None,
                        Some(notification.title.clone()),
                        email_template_id.map(str::to_string),
                        Some("no recipient email address was supplied".to_string()),
                        1,
                    );
                    state.emit(
                        "notification.delivery.failed",
                        json!({
                            "attempt_id": attempt.id,
                            "notification_id": notification.id,
                            "channel": "email",
                            "error": "no recipient email address was supplied",
                            "attempts": 1,
                            "retryable": true,
                        }),
                    );
                    continue;
                };
                let outcome = send_email(
                    state,
                    &to,
                    &notification.title,
                    &notification.body,
                    email_template_id,
                    variables,
                    connection_id.clone(),
                    json!({ "notification_id": notification.id, "event": notification.kind }),
                );
                if outcome.queued {
                    state.record_delivery(
                        notification_id.clone(),
                        "email",
                        "queued",
                        Some(outcome.via.to_string()),
                        outcome.job_id,
                        Some(to),
                        Some(notification.title.clone()),
                        email_template_id.map(str::to_string),
                        None,
                        1,
                    );
                } else {
                    let reason = outcome
                        .error
                        .unwrap_or_else(|| "email send was not queued".to_string());
                    let attempt = state.record_delivery(
                        notification_id.clone(),
                        "email",
                        "failed",
                        Some(outcome.via.to_string()),
                        None,
                        Some(to),
                        Some(notification.title.clone()),
                        email_template_id.map(str::to_string),
                        Some(reason.clone()),
                        1,
                    );
                    state.emit(
                        "notification.delivery.failed",
                        json!({
                            "attempt_id": attempt.id,
                            "notification_id": notification.id,
                            "channel": "email",
                            "error": reason,
                            "attempts": 1,
                            "retryable": true,
                        }),
                    );
                }
            }
            "webhook" => {
                state.emit(
                    "notification.webhook",
                    json!({
                        "event": notification.kind,
                        "notification_id": notification.id,
                        "user_id": notification.user_id,
                        "title": notification.title,
                        "body": notification.body,
                        "kind": notification.kind,
                    }),
                );
                // The webhooks module owns the final delivery: it
                // subscribes to `notification.webhook`, delivers the
                // payload to every matching endpoint, and reports the
                // outcome as `webhook.delivered` / `webhook.failed`,
                // which this module subscribes to
                // (`on_webhook_delivery`) to resolve this attempt.
                // Its own delivery records live there. This attempt
                // stays `queued` until one of those outcomes arrives.
                state.record_delivery(
                    notification_id.clone(),
                    "webhook",
                    "queued",
                    None,
                    None,
                    None,
                    Some(notification.title.clone()),
                    None,
                    None,
                    1,
                );
            }
            "in_app" => {
                // In-app delivery needs no dispatch: the notification
                // is already stored, which IS the delivery.
                state.record_delivery(
                    notification_id.clone(),
                    "in_app",
                    "delivered",
                    None,
                    None,
                    None,
                    Some(notification.title.clone()),
                    None,
                    None,
                    1,
                );
            }
            unknown => {
                // An unknown channel never delivered anything: record
                // it as failed (never "delivered") and surface the
                // failure, so a typo or an unimplemented channel
                // cannot fabricate a successful delivery. Validation
                // in the HTTP handlers and in
                // `on_notification_requested` rejects unknown
                // channels first; this arm is the defence in depth
                // for template channels and direct callers.
                let reason = format!("Unknown notification channel: {unknown}");
                let attempt = state.record_delivery(
                    notification_id.clone(),
                    unknown,
                    "failed",
                    None,
                    None,
                    None,
                    Some(notification.title.clone()),
                    None,
                    Some(reason.clone()),
                    1,
                );
                state.emit(
                    "notification.delivery.failed",
                    json!({
                        "attempt_id": attempt.id,
                        "notification_id": notification.id,
                        "channel": unknown,
                        "error": reason,
                        "attempts": 1,
                        "retryable": false,
                    }),
                );
            }
        }
    }
}

/// Dispatch a rendered notification through the channels stored on
/// its template. Email goes through the Email Providers module with
/// the template's provider connection, falling back to the Mail
/// Manager when no provider handler answers. Webhook delivery is
/// emitted as `notification.webhook` for the webhooks module to
/// deliver. In-app delivery already happened (the notification is
/// stored). The user's channel preferences still gate email: a user
/// who disabled email never receives one.

pub(crate) fn dispatch_template_channels(
    state: &Arc<NotificationsState>,
    template: &NotificationTemplate,
    notification: &Notification,
    email: Option<String>,
    variables: &HashMap<String, Value>,
) {
    dispatch_channels(
        state,
        notification,
        &template.channels,
        email,
        template.email_template_id.as_deref(),
        variables,
        template.email_connection_id.clone(),
    );
}

/// Render one stored notification through its event template (when
/// one exists) and dispatch it. Without a template the stored
/// title/body are dispatched through the requested channels instead.
/// `email_template_id` is the GrapeJS template id supplied with the
/// request; a notification template's own id wins over it.

pub(crate) fn render_and_dispatch(
    state: &Arc<NotificationsState>,
    notification: &Notification,
    variables: &HashMap<String, Value>,
    email: Option<String>,
    email_template_id: Option<&str>,
    requested_channels: &[String],
) {
    let template = state.template_for(&notification.kind, "en");
    match template {
        Some(template) => {
            let mut rendered = notification.clone();
            rendered.title = render_template_text(&template.title_template, variables);
            rendered.body = render_template_text(&template.body_template, variables);
            state
                .notifications
                .insert(rendered.id.clone(), rendered.clone());
            dispatch_template_channels(state, &template, &rendered, email, variables);
        }
        None => {
            let channels: Vec<String> = if requested_channels.is_empty() {
                vec!["in_app".to_string()]
            } else {
                requested_channels.to_vec()
            };
            dispatch_channels(
                state,
                notification,
                &channels,
                email,
                email_template_id,
                variables,
                None,
            );
        }
    }
}

/// Resolve the recipients of a `notification.requested` payload.
/// Returns the resolved recipients as `(user_id, email)` pairs plus
/// the audience label stored on each notification. Resolution order:
/// explicit `user_ids` first, then broadcast (`all` / `users` / a
/// `user_id` of "all"), then a registered role / department / segment
/// audience (`admins` resolves through the registered `role:admin`
/// audience), then a single `user_id`. An unregistered role /
/// department / segment audience is an error: the notification is
/// delivered to nobody rather than to the wrong people.

pub(crate) fn resolve_recipients(
    state: &Arc<NotificationsState>,
    data: &Value,
) -> Result<(Vec<(String, Option<String>)>, String), String> {
    let clean = |value: &Value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    let user_ids: Vec<String> = data
        .get("user_ids")
        .and_then(|value| value.as_array())
        .map(|items| items.iter().filter_map(clean).collect())
        .unwrap_or_default();
    let emails: Vec<String> = data
        .get("emails")
        .and_then(|value| value.as_array())
        .map(|items| items.iter().filter_map(clean).collect())
        .unwrap_or_default();
    let single_email = data.get("email").and_then(clean);
    let audience = data
        .get("audience")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .or_else(|| {
            data.get("role")
                .and_then(|value| value.as_str())
                .map(|role| format!("role:{}", role.trim()))
        })
        .or_else(|| {
            data.get("department")
                .and_then(|value| value.as_str())
                .map(|department| format!("department:{}", department.trim()))
        })
        .or_else(|| {
            data.get("segment")
                .and_then(|value| value.as_str())
                .map(|segment| format!("segment:{}", segment.trim()))
        });
    let single_user = data
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if !user_ids.is_empty() {
        let mut recipients: Vec<(String, Option<String>)> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for (index, user_id) in user_ids.into_iter().enumerate() {
            if seen.contains(&user_id) {
                continue;
            }
            seen.push(user_id.clone());
            let email = emails
                .get(index)
                .cloned()
                .or_else(|| single_email.clone());
            recipients.push((user_id, email));
        }
        let label = audience.unwrap_or_else(|| "user".to_string());
        return Ok((recipients, label));
    }
    if audience.as_deref() == Some("all")
        || audience.as_deref() == Some("users")
        || single_user == "all"
    {
        return Ok((vec![("all".to_string(), None)], "all".to_string()));
    }
    if let Some(audience) = audience {
        let registry_key = if audience == "admins" {
            "role:admin".to_string()
        } else {
            audience.clone()
        };
        if audience == "admins"
            || audience.starts_with("role:")
            || audience.starts_with("department:")
            || audience.starts_with("segment:")
        {
            return match state.audience_members(&registry_key) {
                Some(members) if !members.is_empty() => Ok((
                    members
                        .into_iter()
                        .map(|member| (member.user_id, member.email))
                        .collect(),
                    registry_key,
                )),
                _ => Err(format!(
                    "Audience {audience} has no registered members; register them with POST /api/v1/notifications/audiences before sending"
                )),
            };
        }
        return Err(format!("Unknown audience {audience}"));
    }
    if !single_user.is_empty() {
        return Ok((vec![(single_user, single_email)], "user".to_string()));
    }
    Err("A notification.requested payload needs user_id, user_ids or audience".to_string())
}

// Failure-event listeners.
//
// The centre subscribes to the verified failure events so alerts land
// here instead of only in logs:
//
//   - `security.auth.failed` -- emitted by the hardening layer
//     (`unified_core::hardening::security_events::AUTH_FAILED`) when a
//     failed-auth cluster reaches its threshold. Payload: `key`, `ip`,
//     `account`, `count`, `threshold` (no `user_id`). The notification
//     goes to the `account` when one is present, otherwise `system`.
//   - `cluster.sync.failed` -- emitted by the clusters module
//     (`CLUSTER_SYNC_FAILED`) with `plan_id`, `cluster_id`, `node_id`,
//     `direction`, `errors`. The clusters module already forwards the
//     same failure through `notification.requested` (user `system`),
//     so this listener only creates a notification when the payload
//     carries an explicit `user_id`: without one, creating here would
//     duplicate the notification the clusters module already sent.
//   - `payment.failed` -- defined by the payments module
//     (`PAYMENT_FAILED`) for chargeback webhooks, which also forward a
//     `notification.requested`. No emit call site for the raw event
//     exists in the tree yet, so this listener is currently dormant;
//     it creates a billing notification when a payload with a
//     `user_id` arrives. The same user-id guard prevents duplicates
//     from emitters that already forward via `notification.requested`.
//
// All three listeners respect the target user's category preferences
// through `notify_for_category` (security / sync / billing).


#[cfg(test)]
mod tests {
    use super::*;

    fn test_notification() -> Notification {
        Notification {
            id: "n-1".to_string(),
            user_id: "user-1".to_string(),
            title: "Hello".to_string(),
            body: "Body".to_string(),
            kind: "general".to_string(),
            category: "system".to_string(),
            module: "notifications".to_string(),
            priority: "normal".to_string(),
            link: None,
            audience: "user".to_string(),
            read: false,
            created_at: now(),
        }
    }

    #[test]
    fn unknown_channel_is_recorded_failed_not_delivered() {
        let state = Arc::new(NotificationsState::new());
        let notification = test_notification();
        dispatch_channels(
            &state,
            &notification,
            &["sms".to_string()],
            None,
            None,
            &HashMap::new(),
            None,
        );
        let attempts: Vec<DeliveryAttempt> = state
            .deliveries
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].channel, "sms");
        assert_eq!(attempts[0].status, "failed");
        assert!(attempts[0]
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Unknown notification channel"));
        // Nothing is ever recorded as a delivered in-app attempt for
        // a channel that was never requested.
        assert!(attempts.iter().all(|attempt| attempt.status != "delivered"));
    }

    #[test]
    fn known_channel_still_delivers_in_app() {
        let state = Arc::new(NotificationsState::new());
        let notification = test_notification();
        dispatch_channels(
            &state,
            &notification,
            &["in_app".to_string()],
            None,
            None,
            &HashMap::new(),
            None,
        );
        let attempts: Vec<DeliveryAttempt> = state
            .deliveries
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].channel, "in_app");
        assert_eq!(attempts[0].status, "delivered");
    }

    #[test]
    fn hook_request_rejects_unknown_channels_without_storing() {
        let state = Arc::new(NotificationsState::new());
        let mut ctx = HookContext::new(
            "notification.requested",
            json!({
                "user_id": "user-1",
                "title": "Hello",
                "body": "Body",
                "channels": ["sms"],
            }),
        );
        on_notification_requested(state.clone(), &mut ctx);
        assert!(state.notifications.is_empty());
        assert!(state.deliveries.is_empty());
        let result = ctx.results.first().expect("a result is reported");
        assert_eq!(result.get("delivered").and_then(|v| v.as_u64()), Some(0));
        assert!(result.get("error").and_then(|v| v.as_str()).is_some());
    }

    #[test]
    fn hook_request_accepts_known_channels() {
        let state = Arc::new(NotificationsState::new());
        let mut ctx = HookContext::new(
            "notification.requested",
            json!({
                "user_id": "user-1",
                "title": "Hello",
                "body": "Body",
                "channels": ["in_app"],
            }),
        );
        on_notification_requested(state.clone(), &mut ctx);
        assert_eq!(state.notifications.len(), 1);
        let result = ctx.results.first().expect("a result is reported");
        assert_eq!(result.get("delivered").and_then(|v| v.as_u64()), Some(1));
    }
}
