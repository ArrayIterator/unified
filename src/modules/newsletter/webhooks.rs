// Newsletter module - provider webhook handling.
//
// Split from `src/modules/newsletter.rs`; the module root re-exports the public API of this module.

use unified_core::error::UnifiedError;
use unified_core::json as api_json;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::constants::{EVENT_EMAIL_PROVIDER_WEBHOOK_VERIFY, EVENT_NEWSLETTER_UNSUBSCRIBED, PERM_NEWSLETTER_VIEW};
use super::helpers::{find_subscriber, now_string};
use super::state::{
    NewsletterState, authorize, caller_company, campaigns_store, lists_store, require_tenant,
    subscribers_store,
};
use super::tracking::{record_tracking};
use super::types::{Subscriber};

// ---------------------------------------------------------------------------
// Provider webhooks.
// ---------------------------------------------------------------------------

/// One normalized webhook event, mapped honestly from what each
/// provider actually delivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebhookAction {
    Delivered,
    Opened,
    Clicked,
    Bounced,
    Complained,
    Unsubscribed,
    /// Seen and logged, but no subscriber or stats change (for
    /// example a soft bounce or a deferred delivery).
    LogOnly,
}

#[derive(Debug, Clone)]
pub(crate) struct NormalizedEvent {
    pub(crate) email: String,
    pub(crate) action: WebhookAction,
    pub(crate) campaign_id: Option<String>,
    pub(crate) subscriber_id: Option<String>,
}

pub(crate) fn metadata_ids(value: &Value) -> (Option<String>, Option<String>) {
    let campaign_id = value
        .get("campaign_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            value
                .get("metadata")
                .and_then(|v| v.get("campaign_id"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
    let subscriber_id = value
        .get("subscriber_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            value
                .get("metadata")
                .and_then(|v| v.get("subscriber_id"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
    (campaign_id, subscriber_id)
}

pub(crate) fn normalize_webhook_events(provider: &str, body: &Value) -> Vec<NormalizedEvent> {
    let mut events = Vec::new();
    let mut push = |events: &mut Vec<NormalizedEvent>,
                    email: &str,
                    action: WebhookAction,
                    campaign_id: Option<String>,
                    subscriber_id: Option<String>| {
        if email.trim().is_empty() {
            return;
        }
        events.push(NormalizedEvent {
            email: email.trim().to_lowercase(),
            action,
            campaign_id,
            subscriber_id,
        });
    };
    match provider {
        "sendgrid" => {
            // SendGrid posts a JSON array of event objects.
            if let Some(items) = body.as_array() {
                for item in items {
                    let email = item.get("email").and_then(|v| v.as_str()).unwrap_or("");
                    let (campaign_id, subscriber_id) = metadata_ids(item);
                    let action = match item.get("event").and_then(|v| v.as_str()) {
                        Some("delivered") => WebhookAction::Delivered,
                        Some("open") => WebhookAction::Opened,
                        Some("click") => WebhookAction::Clicked,
                        Some("bounce") | Some("dropped") => WebhookAction::Bounced,
                        Some("spamreport") => WebhookAction::Complained,
                        Some("unsubscribe") => WebhookAction::Unsubscribed,
                        _ => WebhookAction::LogOnly,
                    };
                    push(&mut events, email, action, campaign_id, subscriber_id);
                }
            }
        }
        "brevo" => {
            // Brevo posts one JSON object per event.
            let email = body.get("email").and_then(|v| v.as_str()).unwrap_or("");
            let action = match body.get("event").and_then(|v| v.as_str()) {
                Some("delivered") | Some("sent") => WebhookAction::Delivered,
                Some("opened") => WebhookAction::Opened,
                Some("clicked") => WebhookAction::Clicked,
                Some("hardBounce") | Some("invalid") | Some("blocked") => WebhookAction::Bounced,
                Some("spam") => WebhookAction::Complained,
                Some("unsubscribed") => WebhookAction::Unsubscribed,
                _ => WebhookAction::LogOnly,
            };
            push(&mut events, email, action, None, None);
        }
        "mailgun" => {
            // Mailgun posts form fields, or JSON with an `event-data`
            // object (the endpoint normalizes form bodies to JSON).
            let data = body.get("event-data").unwrap_or(body);
            let email = data
                .get("recipient")
                .and_then(|v| v.as_str())
                .or_else(|| data.get("email").and_then(|v| v.as_str()))
                .unwrap_or("");
            let (campaign_id, subscriber_id) = metadata_ids(data);
            let action = match data.get("event").and_then(|v| v.as_str()) {
                Some("delivered") => WebhookAction::Delivered,
                Some("opened") => WebhookAction::Opened,
                Some("clicked") => WebhookAction::Clicked,
                Some("complained") => WebhookAction::Complained,
                Some("unsubscribed") => WebhookAction::Unsubscribed,
                Some("bounced") => WebhookAction::Bounced,
                Some("failed") => {
                    let permanent = data
                        .get("severity")
                        .and_then(|v| v.as_str())
                        .is_some_and(|severity| severity == "permanent");
                    if permanent {
                        WebhookAction::Bounced
                    } else {
                        WebhookAction::LogOnly
                    }
                }
                _ => WebhookAction::LogOnly,
            };
            push(&mut events, email, action, campaign_id, subscriber_id);
        }
        "postmark" => {
            // Postmark posts one JSON object per event.
            let email = body.get("Email").and_then(|v| v.as_str()).unwrap_or("");
            let metadata = body.get("Metadata").cloned().unwrap_or(Value::Null);
            let (campaign_id, subscriber_id) = metadata_ids(&metadata);
            let action = match body.get("RecordType").and_then(|v| v.as_str()) {
                Some("Delivery") => WebhookAction::Delivered,
                Some("Open") => WebhookAction::Opened,
                Some("Click") => WebhookAction::Clicked,
                Some("Bounce") => {
                    let hard = body
                        .get("Type")
                        .and_then(|v| v.as_str())
                        .is_some_and(|bounce_type| bounce_type == "HardBounce");
                    if hard {
                        WebhookAction::Bounced
                    } else {
                        WebhookAction::LogOnly
                    }
                }
                Some("SpamComplaint") => WebhookAction::Complained,
                Some("SubscriptionChange") => {
                    match body.get("SuppressionReason").and_then(|v| v.as_str()) {
                        Some("RecipientSpamComplaint") => WebhookAction::Complained,
                        Some("RecipientUnsubscribed") => WebhookAction::Unsubscribed,
                        _ => WebhookAction::LogOnly,
                    }
                }
                _ => WebhookAction::LogOnly,
            };
            push(&mut events, email, action, campaign_id, subscriber_id);
        }
        "ses" => {
            // SES delivers SNS envelopes: { "Message": "{...}" }.
            // Event-publishing messages (Open/Click) arrive as plain
            // JSON with `eventType` instead.
            if let Some(event_type) = body.get("eventType").and_then(|v| v.as_str()) {
                let email = body
                    .get("mail")
                    .and_then(|v| v.get("destination"))
                    .and_then(|v| v.as_array())
                    .and_then(|items| items.first())
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let action = match event_type {
                    "Delivery" => WebhookAction::Delivered,
                    "Open" => WebhookAction::Opened,
                    "Click" => WebhookAction::Clicked,
                    "Bounce" => WebhookAction::Bounced,
                    "Complaint" => WebhookAction::Complained,
                    _ => WebhookAction::LogOnly,
                };
                push(&mut events, email, action, None, None);
            } else if let Some(message) = body.get("Message").and_then(|v| v.as_str()) {
                if let Ok(inner) = serde_json::from_str::<Value>(message) {
                    let notification_type = inner
                        .get("notificationType")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    match notification_type {
                        "Bounce" => {
                            let permanent = inner
                                .get("bounce")
                                .and_then(|v| v.get("bounceType"))
                                .and_then(|v| v.as_str())
                                .is_some_and(|bounce_type| bounce_type == "Permanent");
                            let action = if permanent {
                                WebhookAction::Bounced
                            } else {
                                WebhookAction::LogOnly
                            };
                            if let Some(recipients) = inner
                                .get("bounce")
                                .and_then(|v| v.get("bouncedRecipients"))
                                .and_then(|v| v.as_array())
                            {
                                for recipient in recipients {
                                    let email = recipient
                                        .get("emailAddress")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    push(&mut events, email, action, None, None);
                                }
                            }
                        }
                        "Complaint" => {
                            if let Some(recipients) = inner
                                .get("complaint")
                                .and_then(|v| v.get("complainedRecipients"))
                                .and_then(|v| v.as_array())
                            {
                                for recipient in recipients {
                                    let email = recipient
                                        .get("emailAddress")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    push(
                                        &mut events,
                                        email,
                                        WebhookAction::Complained,
                                        None,
                                        None,
                                    );
                                }
                            }
                        }
                        "Delivery" => {
                            if let Some(recipients) = inner
                                .get("mail")
                                .and_then(|v| v.get("destination"))
                                .and_then(|v| v.as_array())
                            {
                                for recipient in recipients {
                                    let email = recipient.as_str().unwrap_or("");
                                    push(&mut events, email, WebhookAction::Delivered, None, None);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        "resend" => {
            // Resend posts { "type": "email.delivered", "data": {...} }.
            let event_type = body.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let data = body.get("data").unwrap_or(body);
            let email = data
                .get("to")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|v| v.as_str())
                .or_else(|| data.get("email").and_then(|v| v.as_str()))
                .unwrap_or("");
            let (campaign_id, subscriber_id) = metadata_ids(data);
            let action = match event_type {
                "email.delivered" | "email.sent" => WebhookAction::Delivered,
                "email.opened" => WebhookAction::Opened,
                "email.clicked" => WebhookAction::Clicked,
                "email.bounced" => WebhookAction::Bounced,
                "email.complained" => WebhookAction::Complained,
                _ => WebhookAction::LogOnly,
            };
            push(&mut events, email, action, campaign_id, subscriber_id);
        }
        "sparkpost" => {
            // SparkPost posts a JSON array of { "msys": { "message_event": {...} } }.
            let items: Vec<&Value> = match body.as_array() {
                Some(items) => items.iter().collect(),
                None => vec![body],
            };
            for item in items {
                let event = item
                    .get("msys")
                    .and_then(|v| v.get("message_event"))
                    .unwrap_or(item);
                let email = event
                    .get("rcpt_to")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let metadata = event.get("metadata").cloned().unwrap_or(Value::Null);
                let (campaign_id, subscriber_id) = metadata_ids(&metadata);
                let action = match event.get("type").and_then(|v| v.as_str()) {
                    Some("delivery") => WebhookAction::Delivered,
                    Some("open") => WebhookAction::Opened,
                    Some("click") => WebhookAction::Clicked,
                    Some("spam_complaint") => WebhookAction::Complained,
                    Some("unsubscribe") | Some("list_unsubscribe") => {
                        WebhookAction::Unsubscribed
                    }
                    Some("bounce") => {
                        // SparkPost bounce classes 10, 30, 90 are
                        // permanent (undetermined, invalid, blocked);
                        // 20 and 40 are transient.
                        let permanent = event
                            .get("bounce_class")
                            .and_then(|v| v.as_u64())
                            .is_some_and(|class| matches!(class, 10 | 30 | 90));
                        if permanent {
                            WebhookAction::Bounced
                        } else {
                            WebhookAction::LogOnly
                        }
                    }
                    _ => WebhookAction::LogOnly,
                };
                push(&mut events, email, action, campaign_id, subscriber_id);
            }
        }
        "mailchimp" => {
            // Mandrill posts a form field `mandrill_events` holding a
            // JSON array of { "event", "msg": { "email", "metadata" } }.
            let items = body
                .get("mandrill_events")
                .and_then(|v| v.as_str())
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default();
            for item in items {
                let msg = item.get("msg").unwrap_or(&item);
                let email = msg.get("email").and_then(|v| v.as_str()).unwrap_or("");
                let (campaign_id, subscriber_id) = metadata_ids(msg);
                let action = match item.get("event").and_then(|v| v.as_str()) {
                    Some("send") => WebhookAction::Delivered,
                    Some("open") => WebhookAction::Opened,
                    Some("click") => WebhookAction::Clicked,
                    Some("hard_bounce") => WebhookAction::Bounced,
                    Some("spam") => WebhookAction::Complained,
                    Some("unsub") => WebhookAction::Unsubscribed,
                    _ => WebhookAction::LogOnly,
                };
                push(&mut events, email, action, campaign_id, subscriber_id);
            }
        }
        _ => {}
    }
    events
}

/// Resolve the subscriber an event belongs to, scoped by the
/// event's own identifiers: an explicit subscriber id wins, then the
/// event campaign's list, and only when the email belongs to exactly
/// one subscriber does the bare email resolve. An email shared by
/// several lists with no scoping id is ambiguous and resolves to
/// nothing, so a bounce for list A can never flip the same email's
/// subscriber in list B.
pub(crate) fn find_scoped_subscriber(event: &NormalizedEvent) -> Option<Subscriber> {
    if let Some(subscriber_id) = &event.subscriber_id {
        let found = subscribers_store()
            .get(subscriber_id)
            .map(|entry| entry.value().clone())?;
        if found.email.eq_ignore_ascii_case(event.email.trim()) {
            return Some(found);
        }
        return None;
    }
    if let Some(campaign_id) = &event.campaign_id {
        let campaign = campaigns_store()
            .get(campaign_id)
            .map(|entry| entry.value().clone())?;
        return find_subscriber(&campaign.list_id, &event.email);
    }
    let matches: Vec<Subscriber> = subscribers_store()
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|subscriber| subscriber.email.eq_ignore_ascii_case(event.email.trim()))
        .collect();
    if matches.len() == 1 {
        matches.into_iter().next()
    } else {
        None
    }
}

pub(crate) fn is_ambiguous_subscriber_email(email: &str) -> bool {
    let needle = email.trim().to_lowercase();
    subscribers_store()
        .iter()
        .filter(|entry| entry.value().email.to_lowercase() == needle)
        .count()
        > 1
}

/// True when the campaign exists and belongs to the given list, so
/// webhook stats are only bumped on the campaign the event is for.
fn campaign_in_list(campaign_id: &str, list_id: &str) -> bool {
    campaigns_store()
        .get(campaign_id)
        .is_some_and(|entry| entry.value().list_id == list_id)
}

pub(crate) fn apply_webhook_event(state: &NewsletterState, event: &NormalizedEvent) -> String {
    if event.action == WebhookAction::LogOnly {
        return "logged".to_string();
    }
    if let (Some(campaign_id), Some(subscriber_id)) =
        (&event.campaign_id, &event.subscriber_id)
    {
        match event.action {
            WebhookAction::Opened | WebhookAction::Clicked => {
                let kind = if event.action == WebhookAction::Opened {
                    "open"
                } else {
                    "click"
                };
                // record_tracking verifies the campaign and the
                // subscriber exist and belong to the same list.
                if record_tracking(state, campaign_id, subscriber_id, kind, None) {
                    return if event.action == WebhookAction::Opened {
                        "tracked_open".to_string()
                    } else {
                        "tracked_click".to_string()
                    };
                }
                return "unknown_subscriber".to_string();
            }
            _ => {}
        }
    }
    let Some(subscriber) = find_scoped_subscriber(event) else {
        return if is_ambiguous_subscriber_email(&event.email) {
            "ambiguous_subscriber".to_string()
        } else {
            "unknown_subscriber".to_string()
        };
    };
    match event.action {
        WebhookAction::Delivered => {
            if let Some(campaign_id) = &event.campaign_id {
                if campaign_in_list(campaign_id, &subscriber.list_id) {
                    if let Some(mut campaign) = campaigns_store().get_mut(campaign_id) {
                        campaign.stats.delivered += 1;
                    }
                }
            }
            "delivered".to_string()
        }
        WebhookAction::Bounced => {
            apply_subscriber_status(state, &subscriber, "bounced");
            if let Some(campaign_id) = &event.campaign_id {
                if campaign_in_list(campaign_id, &subscriber.list_id) {
                    if let Some(mut campaign) = campaigns_store().get_mut(campaign_id) {
                        campaign.stats.bounced += 1;
                    }
                }
            }
            "bounced".to_string()
        }
        WebhookAction::Complained => {
            apply_subscriber_status(state, &subscriber, "complained");
            if let Some(campaign_id) = &event.campaign_id {
                if campaign_in_list(campaign_id, &subscriber.list_id) {
                    if let Some(mut campaign) = campaigns_store().get_mut(campaign_id) {
                        campaign.stats.complaints += 1;
                    }
                }
            }
            "complained".to_string()
        }
        WebhookAction::Unsubscribed => {
            apply_subscriber_status(state, &subscriber, "unsubscribed");
            if let Some(campaign_id) = &event.campaign_id {
                if campaign_in_list(campaign_id, &subscriber.list_id) {
                    if let Some(mut campaign) = campaigns_store().get_mut(campaign_id) {
                        campaign.stats.unsubscribed += 1;
                    }
                }
            }
            "unsubscribed".to_string()
        }
        WebhookAction::Opened | WebhookAction::Clicked => "tracked".to_string(),
        WebhookAction::LogOnly => "logged".to_string(),
    }
}

pub(crate) fn apply_subscriber_status(state: &NewsletterState, subscriber: &Subscriber, status: &str) {
    if let Some(mut stored) = subscribers_store().get_mut(&subscriber.id) {
        stored.status = status.to_string();
        if status == "unsubscribed" {
            stored.unsubscribed_at = Some(now_string());
        }
        stored.updated_at = now_string();
    }
    if status == "unsubscribed" {
        state.emit(
            EVENT_NEWSLETTER_UNSUBSCRIBED,
            json!({
                "subscriber_id": subscriber.id,
                "list_id": subscriber.list_id,
                "email": subscriber.email,
                "via": "webhook",
            }),
        );
    }
}

pub(crate) async fn provider_webhook(
    state: web::Data<Arc<NewsletterState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<HashMap<String, String>>,
    body: web::Bytes,
) -> HttpResponse {
    let debug = state.is_debug();
    let provider = path.into_inner();
    if !matches!(
        provider.as_str(),
        "mailchimp" | "brevo" | "sendgrid" | "mailgun" | "postmark" | "ses" | "resend" | "sparkpost"
    ) {
        return api_json::error_response(
            &UnifiedError::bad_request(format!(
                "Unknown webhook provider \"{provider}\" (SMTP has no webhooks)"
            )),
            debug,
        );
    }
    // Verify the signature through the Email Providers module before
    // any processing. The raw body is passed through unchanged.
    let headers_object: HashMap<String, String> = req
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let body_text = String::from_utf8_lossy(&body).into_owned();
    let verified = match state.hooks() {
        Some(hooks) => {
            let answer = hooks.emit(
                EVENT_EMAIL_PROVIDER_WEBHOOK_VERIFY,
                json!({
                    "provider": provider,
                    "connection_id": query.get("connection_id"),
                    "headers": headers_object,
                    "body": body_text,
                }),
            );
            match answer.results.first() {
                Some(result) if result.get("error").is_none() => {
                    result.get("valid").and_then(|v| v.as_bool()).unwrap_or(false)
                }
                Some(result) => {
                    let reason = result
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("webhook verification failed");
                    return api_json::error_response(
                        &UnifiedError::unauthorized(format!("Webhook rejected: {reason}")),
                        debug,
                    );
                }
                None => {
                    return api_json::error_response(
                        &UnifiedError::new(
                            "Email provider service did not answer the email_provider.webhook_verify event",
                            503,
                        ),
                        debug,
                    )
                }
            }
        }
        None => {
            return api_json::error_response(
                &UnifiedError::new("Webhook verification is unavailable (no hook bus)", 503),
                debug,
            )
        }
    };
    if !verified {
        return api_json::error_response(
            &UnifiedError::unauthorized("Webhook signature verification failed"),
            debug,
        );
    }
    // Parse the body: JSON first, form-encoded second (Mailgun and
    // Mandrill post form bodies).
    let parsed: Value = serde_json::from_slice(&body).unwrap_or_else(|_| {
        let fields: HashMap<String, String> = url::form_urlencoded::parse(&body)
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        serde_json::to_value(fields).unwrap_or(Value::Null)
    });
    let events = normalize_webhook_events(&provider, &parsed);
    let mut outcomes: HashMap<String, u64> = HashMap::new();
    for event in &events {
        let outcome = apply_webhook_event(&state, event);
        *outcomes.entry(outcome).or_insert(0) += 1;
    }
    api_json::success_response(
        json!({
            "provider": provider,
            "received": events.len(),
            "outcomes": outcomes,
        }),
        200,
    )
}

// ---------------------------------------------------------------------------
// Overall stats.
// ---------------------------------------------------------------------------

pub(crate) async fn overall_stats(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_VIEW) {
        return api_json::error_response(&error, debug);
    }
    let lists = lists_store().len() as u64;
    let subscribers = subscribers_store().len() as u64;
    let confirmed = subscribers_store()
        .iter()
        .filter(|entry| entry.value().status == "confirmed")
        .count() as u64;
    let campaigns = campaigns_store().len() as u64;
    let (sent, delivered, opened, clicked, bounced, unsubscribed, complaints) =
        campaigns_store().iter().fold(
            (0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64),
            |acc, entry| {
                let stats = &entry.value().stats;
                (
                    acc.0 + stats.sent,
                    acc.1 + stats.delivered,
                    acc.2 + stats.opened,
                    acc.3 + stats.clicked,
                    acc.4 + stats.bounced,
                    acc.5 + stats.unsubscribed,
                    acc.6 + stats.complaints,
                )
            },
        );
    api_json::success_response(
        json!({
            "lists": lists,
            "subscribers": subscribers,
            "confirmed_subscribers": confirmed,
            "campaigns": campaigns,
            "sent": sent,
            "delivered": delivered,
            "opened": opened,
            "clicked": clicked,
            "bounced": bounced,
            "unsubscribed": unsubscribed,
            "complaints": complaints,
        }),
        200,
    )
}

#[cfg(test)]
mod tests {
    use super::super::types::{Campaign, CampaignStats, NewsletterList};
    use super::*;
    use super::super::types::{Campaign, CampaignStats, NewsletterList};
    use std::collections::HashMap;
    use uuid::Uuid;

    fn seed_list() -> String {
        let id = format!("list-{}", Uuid::now_v7());
        lists_store().insert(
            id.clone(),
            NewsletterList {
                id: id.clone(),
                name: "List".to_string(),
                description: String::new(),
                default_connection_id: None,
                double_opt_in: true,
                sender_name: None,
                sender_email: None,
                base_url: None,
                created_at: String::new(),
                updated_at: String::new(),
            },
        );
        id
    }

    fn seed_campaign(list_id: &str) -> String {
        let id = format!("camp-{}", Uuid::now_v7());
        campaigns_store().insert(
            id.clone(),
            Campaign {
                id: id.clone(),
                list_id: list_id.to_string(),
                name: "Campaign".to_string(),
                template_id: "tpl".to_string(),
                subject: String::new(),
                preview_text: None,
                connection_id: None,
                status: "completed".to_string(),
                scheduled_at: None,
                sent_at: None,
                stats: CampaignStats::default(),
                created_at: String::new(),
                updated_at: String::new(),
            },
        );
        id
    }

    fn seed_subscriber(list_id: &str, email: &str) -> String {
        let id = format!("sub-{}", Uuid::now_v7());
        subscribers_store().insert(
            id.clone(),
            Subscriber {
                id: id.clone(),
                list_id: list_id.to_string(),
                email: email.to_string(),
                name: None,
                status: "confirmed".to_string(),
                metadata: HashMap::new(),
                confirmed_at: None,
                unsubscribed_at: None,
                created_at: String::new(),
                updated_at: String::new(),
            },
        );
        id
    }

    fn status_of(subscriber_id: &str) -> String {
        subscribers_store()
            .get(subscriber_id)
            .map(|entry| entry.value().status.clone())
            .unwrap_or_default()
    }

    fn bounced_of(campaign_id: &str) -> u64 {
        campaigns_store()
            .get(campaign_id)
            .map(|entry| entry.value().stats.bounced)
            .unwrap_or_default()
    }

    #[test]
    fn webhook_bounce_scoped_by_campaign_list_does_not_touch_other_lists() {
        let state = NewsletterState::new();
        let email = format!("shared-{}@example.com", Uuid::now_v7());
        let list_a = seed_list();
        let list_b = seed_list();
        let campaign_a = seed_campaign(&list_a);
        let campaign_b = seed_campaign(&list_b);
        let sub_a = seed_subscriber(&list_a, &email);
        let sub_b = seed_subscriber(&list_b, &email);
        let event = NormalizedEvent {
            email: email.clone(),
            action: WebhookAction::Bounced,
            campaign_id: Some(campaign_a.clone()),
            subscriber_id: None,
        };
        assert_eq!(apply_webhook_event(&state, &event), "bounced");
        assert_eq!(status_of(&sub_a), "bounced");
        assert_eq!(status_of(&sub_b), "confirmed", "list B must be untouched");
        assert_eq!(bounced_of(&campaign_a), 1);
        assert_eq!(bounced_of(&campaign_b), 0, "wrong campaign stats");
    }

    #[test]
    fn webhook_event_without_scope_is_ambiguous_for_a_shared_email() {
        let state = NewsletterState::new();
        let email = format!("shared-{}@example.com", Uuid::now_v7());
        let list_a = seed_list();
        let list_b = seed_list();
        let sub_a = seed_subscriber(&list_a, &email);
        let sub_b = seed_subscriber(&list_b, &email);
        let event = NormalizedEvent {
            email: email.clone(),
            action: WebhookAction::Bounced,
            campaign_id: None,
            subscriber_id: None,
        };
        assert_eq!(apply_webhook_event(&state, &event), "ambiguous_subscriber");
        assert_eq!(status_of(&sub_a), "confirmed");
        assert_eq!(status_of(&sub_b), "confirmed");
    }

    #[test]
    fn webhook_event_with_subscriber_id_scopes_to_that_subscriber() {
        let state = NewsletterState::new();
        let email = format!("shared-{}@example.com", Uuid::now_v7());
        let list_a = seed_list();
        let list_b = seed_list();
        let sub_a = seed_subscriber(&list_a, &email);
        let sub_b = seed_subscriber(&list_b, &email);
        let event = NormalizedEvent {
            email: email.clone(),
            action: WebhookAction::Complained,
            campaign_id: None,
            subscriber_id: Some(sub_b.clone()),
        };
        assert_eq!(apply_webhook_event(&state, &event), "complained");
        assert_eq!(status_of(&sub_b), "complained");
        assert_eq!(status_of(&sub_a), "confirmed");
    }

    #[test]
    fn webhook_open_with_mismatched_campaign_and_subscriber_is_unknown() {
        let state = NewsletterState::new();
        let list_a = seed_list();
        let list_b = seed_list();
        let campaign_a = seed_campaign(&list_a);
        let sub_b = seed_subscriber(&list_b, &format!("solo-{}@example.com", Uuid::now_v7()));
        let event = NormalizedEvent {
            email: String::new(),
            action: WebhookAction::Opened,
            campaign_id: Some(campaign_a),
            subscriber_id: Some(sub_b),
        };
        assert_eq!(apply_webhook_event(&state, &event), "unknown_subscriber");
    }
}
