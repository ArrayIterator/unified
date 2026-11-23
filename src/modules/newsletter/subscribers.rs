// Newsletter module - subscriber handlers.
//
// Split from `src/modules/newsletter.rs`; the module root re-exports the public API of this module.

use unified_core::error::UnifiedError;
use unified_core::json as api_json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use super::constants::{CONFIRM_TOKEN_TTL_SECS, EVENT_NEWSLETTER_CONFIRMED, EVENT_NEWSLETTER_SUBSCRIBED, EVENT_NEWSLETTER_UNSUBSCRIBED, PERM_NEWSLETTER_MANAGE, PERM_NEWSLETTER_VIEW};
use super::helpers::{confirm_url, find_subscriber, matches_search, now_string, paginate, request_send, validate_email_address};
use super::state::{
    NewsletterState, authorize, caller_company, lists_store, require_tenant, subscribers_store,
};
use super::types::{NewsletterList, Subscriber};

// ---------------------------------------------------------------------------
// Subscriber handlers.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct SubscribeInput {
    pub(crate) email: String,
    pub(crate) name: Option<String>,
    pub(crate) metadata: Option<HashMap<String, Value>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SubscriberUpdateInput {
    pub(crate) name: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) metadata: Option<HashMap<String, Value>>,
}

pub(crate) async fn list_list_subscribers(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
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
    let list_id = path.into_inner();
    if !lists_store().contains_key(&list_id) {
        return api_json::error_response(&UnifiedError::not_found("Newsletter list not found"), debug);
    }
    let params = query.into_inner();
    let mut items: Vec<Subscriber> = subscribers_store()
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|subscriber| {
            subscriber.list_id == list_id
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| subscriber.status == *status)
                && matches_search(&subscriber.email, &params)
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    api_json::success_response(page.to_json(), 200)
}

pub(crate) async fn subscribe(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
    body: web::Json<SubscribeInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let list_id = path.into_inner();
    let input = body.into_inner();
    let Some(list) = lists_store().get(&list_id).map(|entry| entry.value().clone()) else {
        return api_json::error_response(&UnifiedError::not_found("Newsletter list not found"), debug);
    };
    if let Err(error) = validate_email_address(&input.email) {
        return api_json::error_response(&error, debug);
    }
    let email = input.email.trim().to_lowercase();
    let now = now_string();
    if let Some(existing) = find_subscriber(&list_id, &email) {
        // Re-subscribing an unsubscribed or bounced address starts the
        // flow again instead of silently reviving the old record.
        if existing.status == "confirmed" {
            return api_json::error_response(
                &UnifiedError::conflict("This email address is already subscribed to the list"),
                debug,
            );
        }
        let mut updated = existing.clone();
        updated.status = if list.double_opt_in { "pending".to_string() } else { "confirmed".to_string() };
        updated.name = input.name.or(updated.name);
        updated.updated_at = now.clone();
        if updated.status == "confirmed" {
            updated.confirmed_at = Some(now.clone());
        }
        subscribers_store().insert(updated.id.clone(), updated.clone());
        return subscribe_response(&state, &list, updated);
    }
    let subscriber = Subscriber {
        id: Uuid::now_v7().to_string(),
        list_id: list.id.clone(),
        email,
        name: input.name,
        status: if list.double_opt_in { "pending".to_string() } else { "confirmed".to_string() },
        metadata: input.metadata.unwrap_or_default(),
        confirmed_at: if list.double_opt_in { None } else { Some(now.clone()) },
        unsubscribed_at: None,
        created_at: now.clone(),
        updated_at: now,
    };
    subscribers_store().insert(subscriber.id.clone(), subscriber.clone());
    state.emit(
        EVENT_NEWSLETTER_SUBSCRIBED,
        json!({
            "subscriber_id": subscriber.id,
            "list_id": list.id,
            "email": subscriber.email,
            "status": subscriber.status,
        }),
    );
    subscribe_response(&state, &list, subscriber)
}

/// Shared subscribe response: for double opt-in, build the signed
/// confirmation link and email it through the list's provider
/// connection (or the global default). The link is also returned so
/// an administrator can deliver it another way when no provider is
/// configured.
pub(crate) fn subscribe_response(
    state: &Arc<NewsletterState>,
    list: &NewsletterList,
    subscriber: Subscriber,
) -> HttpResponse {
    if !list.double_opt_in {
        state.emit(
            EVENT_NEWSLETTER_CONFIRMED,
            json!({
                "subscriber_id": subscriber.id,
                "list_id": list.id,
                "email": subscriber.email,
            }),
        );
        return api_json::success_response(
            json!({ "subscriber": subscriber, "confirmation_email_sent": false, "confirm_url": null }),
            201,
        );
    }
    let token = state.sign_token(&subscriber.id, "confirm", CONFIRM_TOKEN_TTL_SECS);
    let url = confirm_url(list, &token);
    let sent = request_send(
        &state,
        json!({
            "connection_id": list.default_connection_id,
            "to": subscriber.email,
            "from": list.sender_email,
            "from_name": list.sender_name,
            "subject": format!("Please confirm your subscription to {}", list.name),
            "body_text": format!(
                "Please confirm your subscription to {} by opening this link (valid for 48 hours): {url}\n\nIf you did not subscribe, ignore this message.",
                list.name
            ),
            "body_html": format!(
                "<p>Please confirm your subscription to {} by clicking the link below (valid for 48 hours):</p><p><a href=\"{url}\">Confirm subscription</a></p><p>If you did not subscribe, ignore this message.</p>",
                list.name, url = url
            ),
        }),
    );
    api_json::success_response(
        json!({
            "subscriber": subscriber,
            "confirmation_email_sent": sent,
            "confirm_url": url,
        }),
        201,
    )
}

pub(crate) async fn get_subscriber(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
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
    match subscribers_store().get(path.as_str()).map(|entry| entry.value().clone()) {
        Some(subscriber) => api_json::success_response(subscriber, 200),
        None => api_json::error_response(&UnifiedError::not_found("Subscriber not found"), debug),
    }
}

pub(crate) async fn update_subscriber(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
    body: web::Json<SubscriberUpdateInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let input = body.into_inner();
    if let Some(status) = &input.status {
        if !matches!(status.as_str(), "pending" | "confirmed" | "unsubscribed" | "bounced" | "complained") {
            return api_json::error_response(
                &UnifiedError::bad_request("status must be one of: pending, confirmed, unsubscribed, bounced, complained"),
                debug,
            );
        }
    }
    let mut store = subscribers_store();
    let Some(mut subscriber) = store.get_mut(path.as_str()) else {
        return api_json::error_response(&UnifiedError::not_found("Subscriber not found"), debug);
    };
    if let Some(name) = input.name {
        subscriber.name = if name.trim().is_empty() { None } else { Some(name) };
    }
    if let Some(status) = input.status {
        subscriber.status = status;
        if subscriber.status == "confirmed" && subscriber.confirmed_at.is_none() {
            subscriber.confirmed_at = Some(now_string());
        }
        if subscriber.status == "unsubscribed" {
            subscriber.unsubscribed_at = Some(now_string());
        }
    }
    if let Some(metadata) = input.metadata {
        subscriber.metadata = metadata;
    }
    subscriber.updated_at = now_string();
    let subscriber = subscriber.clone();
    drop(store);
    api_json::success_response(subscriber, 200)
}

pub(crate) async fn delete_subscriber(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    match subscribers_store().remove(path.as_str()) {
        Some(_) => api_json::success_response(
            json!({ "deleted": true, "id": path.as_str() }),
            200,
        ),
        None => api_json::error_response(&UnifiedError::not_found("Subscriber not found"), debug),
    }
}

pub(crate) async fn confirm_subscriber(
    state: web::Data<Arc<NewsletterState>>,
    query: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    let debug = state.is_debug();
    let Some(token) = query.get("token") else {
        return api_json::error_response(
            &UnifiedError::bad_request("Query parameter token is required"),
            debug,
        );
    };
    confirm_with_token(&state, token)
}

#[derive(Debug, Deserialize)]
pub(crate) struct ConfirmInput {
    pub(crate) token: String,
}

pub(crate) async fn confirm_subscriber_post(
    state: web::Data<Arc<NewsletterState>>,
    body: web::Json<ConfirmInput>,
) -> HttpResponse {
    confirm_with_token(&state, &body.into_inner().token)
}

pub(crate) fn confirm_with_token(state: &Arc<NewsletterState>, token: &str) -> HttpResponse {
    let debug = state.is_debug();
    let (subscriber_id, purpose) = match state.verify_token(token) {
        Ok(pair) => pair,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if purpose != "confirm" {
        return api_json::error_response(
            &UnifiedError::bad_request("This token is not a confirmation token"),
            debug,
        );
    }
    let mut store = subscribers_store();
    let Some(mut subscriber) = store.get_mut(&subscriber_id) else {
        return api_json::error_response(&UnifiedError::not_found("Subscriber not found"), debug);
    };
    if subscriber.status == "confirmed" {
        let subscriber = subscriber.clone();
        drop(store);
        return api_json::success_response(
            json!({ "subscriber": subscriber, "already_confirmed": true }),
            200,
        );
    }
    if subscriber.status == "unsubscribed" || subscriber.status == "bounced" || subscriber.status == "complained" {
        return api_json::error_response(
            &UnifiedError::conflict(format!(
                "Subscriber cannot be confirmed while status is \"{}\"",
                subscriber.status
            )),
            debug,
        );
    }
    subscriber.status = "confirmed".to_string();
    subscriber.confirmed_at = Some(now_string());
    subscriber.updated_at = now_string();
    let subscriber = subscriber.clone();
    drop(store);
    state.emit(
        EVENT_NEWSLETTER_CONFIRMED,
        json!({
            "subscriber_id": subscriber.id,
            "list_id": subscriber.list_id,
            "email": subscriber.email,
        }),
    );
    api_json::success_response(
        json!({ "subscriber": subscriber, "confirmed": true }),
        200,
    )
}

pub(crate) async fn resend_confirm(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    // Admin-only: this returns a live confirmation URL containing a
    // valid token for any subscriber id, so it must never be open.
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let Some(subscriber) = subscribers_store().get(path.as_str()).map(|entry| entry.value().clone()) else {
        return api_json::error_response(&UnifiedError::not_found("Subscriber not found"), debug);
    };
    if subscriber.status == "confirmed" {
        return api_json::error_response(
            &UnifiedError::conflict("Subscriber is already confirmed"),
            debug,
        );
    }
    let Some(list) = lists_store().get(&subscriber.list_id).map(|entry| entry.value().clone()) else {
        return api_json::error_response(&UnifiedError::not_found("Subscriber list not found"), debug);
    };
    let token = state.sign_token(&subscriber.id, "confirm", CONFIRM_TOKEN_TTL_SECS);
    let url = confirm_url(&list, &token);
    let sent = request_send(
        &state,
        json!({
            "connection_id": list.default_connection_id,
            "to": subscriber.email,
            "from": list.sender_email,
            "from_name": list.sender_name,
            "subject": format!("Please confirm your subscription to {}", list.name),
            "body_text": format!(
                "Please confirm your subscription to {} by opening this link (valid for 48 hours): {url}",
                list.name
            ),
            "body_html": format!(
                "<p>Please confirm your subscription to {} by clicking the link below (valid for 48 hours):</p><p><a href=\"{url}\">Confirm subscription</a></p>",
                list.name, url = url
            ),
        }),
    );
    api_json::success_response(
        json!({ "subscriber_id": subscriber.id, "confirmation_email_sent": sent, "confirm_url": url }),
        200,
    )
}

pub(crate) async fn unsubscribe_subscriber(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let mut store = subscribers_store();
    let Some(mut subscriber) = store.get_mut(path.as_str()) else {
        return api_json::error_response(&UnifiedError::not_found("Subscriber not found"), debug);
    };
    subscriber.status = "unsubscribed".to_string();
    subscriber.unsubscribed_at = Some(now_string());
    subscriber.updated_at = now_string();
    let subscriber = subscriber.clone();
    drop(store);
    state.emit(
        EVENT_NEWSLETTER_UNSUBSCRIBED,
        json!({
            "subscriber_id": subscriber.id,
            "list_id": subscriber.list_id,
            "email": subscriber.email,
            "via": "admin",
        }),
    );
    api_json::success_response(json!({ "subscriber": subscriber, "unsubscribed": true }), 200)
}

pub(crate) async fn unsubscribe_via_link(
    state: web::Data<Arc<NewsletterState>>,
    query: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    let debug = state.is_debug();
    let Some(token) = query.get("token") else {
        return api_json::error_response(
            &UnifiedError::bad_request("Query parameter token is required"),
            debug,
        );
    };
    let (subscriber_id, purpose) = match state.verify_token(token) {
        Ok(pair) => pair,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if purpose != "unsubscribe" {
        return api_json::error_response(
            &UnifiedError::bad_request("This token is not an unsubscribe token"),
            debug,
        );
    }
    let mut store = subscribers_store();
    let Some(mut subscriber) = store.get_mut(&subscriber_id) else {
        return api_json::error_response(&UnifiedError::not_found("Subscriber not found"), debug);
    };
    subscriber.status = "unsubscribed".to_string();
    subscriber.unsubscribed_at = Some(now_string());
    subscriber.updated_at = now_string();
    let subscriber = subscriber.clone();
    drop(store);
    state.emit(
        EVENT_NEWSLETTER_UNSUBSCRIBED,
        json!({
            "subscriber_id": subscriber.id,
            "list_id": subscriber.list_id,
            "email": subscriber.email,
            "via": "link",
        }),
    );
    api_json::success_response(json!({ "subscriber": subscriber, "unsubscribed": true }), 200)
}
