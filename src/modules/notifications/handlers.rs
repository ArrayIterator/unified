// notifications module -- HTTP handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::*;

/// Send one digest email for a user: collect their unread
/// notifications, render them through the `digest` template when one
/// exists (otherwise a plain summary), and dispatch through the Email
/// Providers module (`email.send_requested`), falling back to the Mail
/// Manager (`mail.send_requested`) when no provider handler answers.
/// Returns the digest summary. Reading state is never changed: a
/// digest does not mark notifications read.
pub(crate) fn send_digest_for_user(
    state: &Arc<NotificationsState>,
    user_id: &str,
    email: Option<&str>,
) -> UnifiedResult<Value> {
    if user_id.trim().is_empty() {
        return Err(UnifiedError::bad_request("user_id must not be empty"));
    }
    let mut unread: Vec<Notification> = state
        .notifications
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|notification| {
            NotificationsState::is_visible_to(notification, user_id) && !notification.read
        })
        .collect();
    unread.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    unread.truncate(50);
    if unread.is_empty() {
        return Ok(json!({ "user_id": user_id, "sent": false, "reason": "no unread notifications" }));
    }
    let items: Vec<Value> = unread
        .iter()
        .map(|notification| {
            json!({
                "id": notification.id,
                "title": notification.title,
                "body": notification.body,
                "kind": notification.kind,
                "created_at": notification.created_at.to_rfc3339(),
            })
        })
        .collect();
    let mut variables: HashMap<String, Value> = HashMap::new();
    variables.insert("user_id".to_string(), Value::String(user_id.to_string()));
    variables.insert(
        "count".to_string(),
        Value::from(unread.len() as u64),
    );
    variables.insert("notifications".to_string(), Value::Array(items.clone()));
    let (title, body, connection_id, email_template_id) = match state.template_for("digest", "en") {
        Some(template) => (
            render_template_text(&template.title_template, &variables),
            render_template_text(&template.body_template, &variables),
            template.email_connection_id.clone(),
            template.email_template_id.clone(),
        ),
        None => {
            let lines: Vec<String> = unread
                .iter()
                .map(|notification| format!("- {}: {}", notification.title, notification.body))
                .collect();
            (
                format!("Your notification digest ({} unread)", unread.len()),
                format!(
                    "You have {} unread notification(s):\n{}",
                    unread.len(),
                    lines.join("\n")
                ),
                None,
                None,
            )
        }
    };
    // Digest delivery respects the user's channels: email only when
    // the user has the email channel enabled.
    let preferences = state.preferences_for(user_id);
    if !preferences.channels.iter().any(|channel| channel == "email") {
        return Ok(json!({
            "user_id": user_id,
            "sent": false,
            "reason": "email channel is disabled for this user",
            "unread": unread.len(),
        }));
    }
    let Some(to) = email.map(str::to_string) else {
        return Ok(json!({
            "user_id": user_id,
            "sent": false,
            "reason": "no recipient email address was supplied",
            "unread": unread.len(),
        }));
    };
    // Send through the shared chain (Email Providers first, Mail
    // Manager fallback) and record the attempt, so a digest that
    // cannot be delivered is visible instead of silent.
    let outcome = send_email(
        &state,
        &to,
        &title,
        &body,
        email_template_id.as_deref(),
        &variables,
        connection_id,
        json!({ "digest": "true", "user_id": user_id }),
    );
    if outcome.queued {
        state.record_delivery(
            None,
            "email",
            "queued",
            Some(outcome.via.to_string()),
            outcome.job_id,
            Some(to.clone()),
            Some(title.clone()),
            email_template_id.clone(),
            None,
            1,
        );
        state.emit(
            "notification.digest.sent",
            json!({
                "user_id": user_id,
                "unread": unread.len(),
                "via": outcome.via,
                "sent_at": now().to_rfc3339(),
            }),
        );
        Ok(json!({
            "user_id": user_id,
            "sent": true,
            "via": outcome.via,
            "unread": unread.len(),
            "notifications": items,
        }))
    } else {
        let reason = outcome
            .error
            .unwrap_or_else(|| "digest email was not queued".to_string());
        let attempt = state.record_delivery(
            None,
            "email",
            "failed",
            Some(outcome.via.to_string()),
            None,
            Some(to.clone()),
            Some(title.clone()),
            email_template_id.clone(),
            Some(reason.clone()),
            1,
        );
        state.emit(
            "notification.delivery.failed",
            json!({
                "attempt_id": attempt.id,
                "notification_id": Value::Null,
                "channel": "email",
                "digest": true,
                "user_id": user_id,
                "error": reason,
                "attempts": 1,
                "retryable": true,
            }),
        );
        Ok(json!({
            "user_id": user_id,
            "sent": false,
            "reason": reason,
            "unread": unread.len(),
            "notifications": items,
        }))
    }
}

// Handlers

/// Ownership check for single-notification actions. The acting
/// user is the authenticated caller (`X-User-Id` header, see
/// `authenticate`); broadcast notifications (`user_id` "all") may be
/// acted on by any authenticated user, and an admin (holder of the
/// `notifications.manage` capability) may act on any notification.
/// Anything else is forbidden: a caller-supplied `user_id` is never
/// trusted as identity.
pub(crate) fn check_notification_access(
    notification: &Notification,
    actor: &str,
    is_admin: bool,
) -> UnifiedResult<()> {
    if notification.user_id == "all" || notification.user_id == actor || is_admin {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Notification belongs to another user",
        ))
    }
}

/// Require the admin capability for operator-only resources
/// (templates, audiences). Returns the authenticated caller on
/// success; any other caller gets a 403, unauthenticated callers a
/// 401 from `authenticate`.
pub(crate) fn require_admin(
    ctx: &AppContext,
    req: &HttpRequest,
) -> UnifiedResult<Caller> {
    let (caller, is_admin) = authenticate(ctx, req)?;
    if is_admin {
        Ok(caller)
    } else {
        Err(UnifiedError::forbidden(
            "This resource requires the notifications.manage capability",
        ))
    }
}

pub(crate) async fn list_notifications(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    let (caller, is_admin) = match authenticate(&ctx, &req) {
        Ok(pair) => pair,
        Err(error) => return json::error_response(&error, debug),
    };
    let params = query.into_inner();
    let user_id = match resolve_actor_user_id(
        params.filters.get("user_id").map(String::as_str),
        &caller.actor,
        is_admin,
    ) {
        Ok(user_id) => user_id,
        Err(error) => return json::error_response(&error, debug),
    };
    // The list is always scoped to one user (the caller, or the
    // user an admin explicitly asked for): without a scope an
    // unauthenticated caller used to receive every user's
    // notifications. Broadcast notifications are visible to all.
    let mut items: Vec<Notification> = state
        .notifications
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|notification| {
            matches_search(
                &format!("{} {}", notification.title, notification.body),
                &params,
            ) && NotificationsState::is_visible_to(notification, &user_id)
                && params.filters.get("kind").is_none_or(|kind| notification.kind == *kind)
                && params.filters.get("category").is_none_or(|category| notification.category == *category)
                && params.filters.get("module").is_none_or(|module| notification.module == *module)
                && params.filters.get("priority").is_none_or(|priority| notification.priority == *priority)
                && params.filters.get("audience").is_none_or(|audience| notification.audience == *audience)
                && params.filters.get("read").is_none_or(|read| {
                    notification.read == (*read == "true" || *read == "1")
                })
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_notification(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    body: web::Json<CreateNotification>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    // A caller may only create notifications for themselves; naming
    // another user (or a list containing one) needs the admin
    // capability. The stored `user_id` is never taken as identity.
    let result: UnifiedResult<Notification> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let (caller, is_admin) = authenticate(&ctx, &req)?;
        if !is_admin {
            let mut targets = std::iter::once(input.user_id.as_str())
                .chain(input.user_ids.iter().map(String::as_str));
            if targets.any(|target| target.trim() != caller.actor) {
                return Err(UnifiedError::forbidden(
                    "Cannot create notifications for another user",
                ));
            }
        }
        state.create_notification(input)
    })();
    respond_created(result, debug)
}

pub(crate) async fn get_notification(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Notification> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let (caller, is_admin) = authenticate(&ctx, &req)?;
        let notification = state
            .notifications
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))?;
        check_notification_access(&notification, &caller.actor, is_admin)?;
        Ok(notification)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_notification(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let (caller, is_admin) = authenticate(&ctx, &req)?;
        let existing = state
            .notifications
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))?;
        check_notification_access(&existing, &caller.actor, is_admin)?;
        state
            .notifications
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn mark_read(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Notification> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let (caller, is_admin) = authenticate(&ctx, &req)?;
        let existing = state
            .notifications
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))?;
        check_notification_access(&existing, &caller.actor, is_admin)?;
        let mut notification = state
            .notifications
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))?;
        notification.read = true;
        Ok(notification.clone())
    })();
    if let Ok(notification) = &result {
        state.emit(
            "notification.read",
            json!({
                "notification_id": notification.id,
                "user_id": notification.user_id,
                "read": true,
                "read_at": now().to_rfc3339(),
            }),
        );
    }
    json::respond(result, debug)
}

pub(crate) async fn mark_unread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Notification> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let (caller, is_admin) = authenticate(&ctx, &req)?;
        let existing = state
            .notifications
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))?;
        check_notification_access(&existing, &caller.actor, is_admin)?;
        let mut notification = state
            .notifications
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))?;
        notification.read = false;
        Ok(notification.clone())
    })();
    if let Ok(notification) = &result {
        state.emit(
            "notification.read",
            json!({
                "notification_id": notification.id,
                "user_id": notification.user_id,
                "read": false,
                "read_at": now().to_rfc3339(),
            }),
        );
    }
    json::respond(result, debug)
}

pub(crate) async fn mark_all_read(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    body: web::Json<ReadAllRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let (caller, is_admin) = authenticate(&ctx, &req)?;
        let input = body.into_inner();
        // The body user id is the target, never the identity: it must
        // be the caller, unless the caller is an admin.
        let user_id = resolve_actor_user_id(Some(&input.user_id), &caller.actor, is_admin)?;
        let mut marked = 0u64;
        for mut entry in state.notifications.iter_mut() {
            if NotificationsState::is_visible_to(entry.value(), &user_id) && !entry.value().read {
                entry.value_mut().read = true;
                marked += 1;
            }
        }
        Ok(json!({ "user_id": user_id, "marked_read": marked }))
    })();
    if let Ok(value) = &result {
        state.emit(
            "notification.read",
            json!({
                "user_id": value.get("user_id"),
                "marked_read": value.get("marked_read"),
                "read_all": true,
                "read_at": now().to_rfc3339(),
            }),
        );
    }
    json::respond(result, debug)
}

pub(crate) async fn unread_count(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    let (caller, is_admin) = match authenticate(&ctx, &req) {
        Ok(pair) => pair,
        Err(error) => return json::error_response(&error, debug),
    };
    // Scoped to the caller; an admin may ask for another user's
    // count with `?user_id=...`. The query value is never identity.
    let user_id = match resolve_actor_user_id(
        query.filters.get("user_id").map(String::as_str),
        &caller.actor,
        is_admin,
    ) {
        Ok(user_id) => user_id,
        Err(error) => return json::error_response(&error, debug),
    };
    // Broadcast notifications (user_id "all") count for every user.
    let visible: Vec<Notification> = state
        .notifications
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|notification| NotificationsState::is_visible_to(notification, &user_id))
        .collect();
    let unread = visible.iter().filter(|notification| !notification.read).count() as u64;
    let total = visible.len() as u64;
    let mut by_category: HashMap<String, u64> = HashMap::new();
    for notification in visible.iter().filter(|notification| !notification.read) {
        *by_category.entry(notification.category.clone()).or_insert(0) += 1;
    }
    json::success_response(
        json!({ "user_id": user_id, "unread": unread, "total": total, "by_category": by_category }),
        200,
    )
}

pub(crate) async fn get_preferences(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    // Preferences are per-user data: the caller reads their own,
    // unless they are an admin naming another user.
    let (caller, is_admin) = match authenticate(&ctx, &req) {
        Ok(pair) => pair,
        Err(error) => return json::error_response(&error, debug),
    };
    let user_id = match resolve_actor_user_id(
        query.filters.get("user_id").map(String::as_str),
        &caller.actor,
        is_admin,
    ) {
        Ok(user_id) => user_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let preferences = state.preferences_for(&user_id);
    json::success_response(preferences, 200)
}

pub(crate) async fn update_preferences(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdatePreferences>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<NotificationPreferences> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let (caller, is_admin) = authenticate(&ctx, &req)?;
        let user_id = resolve_actor_user_id(
            query.filters.get("user_id").map(String::as_str),
            &caller.actor,
            is_admin,
        )?;
        let input = body.into_inner();
        let channels = input.channels;
        validate_channels(&channels)?;
        let current = state.preferences_for(&user_id);
        let categories = match input.categories {
            Some(categories) => {
                if categories.is_empty() {
                    return Err(UnifiedError::bad_request(
                        "At least one category must be enabled",
                    ));
                }
                for category in &categories {
                    if !NOTIFICATION_CATEGORIES.contains(&category.as_str()) {
                        return Err(UnifiedError::bad_request(format!(
                            "Categories must be a subset of: {}",
                            NOTIFICATION_CATEGORIES.join(", ")
                        )));
                    }
                }
                categories
            }
            None => current.categories,
        };
        let channel_by_category = match input.channel_by_category {
            Some(overrides) => {
                for (category, category_channels) in &overrides {
                    if !NOTIFICATION_CATEGORIES.contains(&category.as_str()) {
                        return Err(UnifiedError::bad_request(format!(
                            "channel_by_category keys must be a subset of: {}",
                            NOTIFICATION_CATEGORIES.join(", ")
                        )));
                    }
                    validate_channels(category_channels)?;
                }
                overrides
            }
            None => current.channel_by_category,
        };
        let digest = match input.digest {
            Some(digest) => {
                if !DIGEST_OPTIONS.contains(&digest.as_str()) {
                    return Err(UnifiedError::bad_request(format!(
                        "digest must be one of: {}",
                        DIGEST_OPTIONS.join(", ")
                    )));
                }
                digest
            }
            None => current.digest,
        };
        let preferences = NotificationPreferences {
            user_id: user_id.clone(),
            channels,
            categories,
            channel_by_category,
            digest,
            updated_at: now(),
        };
        state
            .preferences
            .insert(user_id.clone(), preferences.clone());
        state.emit(
            "notification.preferences.updated",
            json!({
                "user_id": user_id,
                "channels": preferences.channels,
                "categories": preferences.categories,
                "channel_by_category": preferences.channel_by_category,
                "digest": preferences.digest,
                "updated_at": preferences.updated_at.to_rfc3339(),
            }),
        );
        // A daily or weekly digest is delivered by the scheduler, not
        // by this module: signal the scheduler that this user now wants
        // a digest so it can pick the cadence up on its next run.
        // `immediate` and `off` need no scheduler job.
        if preferences.digest == "daily" || preferences.digest == "weekly" {
            state.emit(
                "notification.digest.requested",
                json!({
                    "user_id": user_id,
                    "digest": preferences.digest,
                }),
            );
        }
        Ok(preferences)
    })();
    json::respond(result, debug)
}

pub(crate) async fn send_digest(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    body: web::Json<DigestSendRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let state: Arc<NotificationsState> = state.get_ref().clone();
    let input = body.into_inner();
    // A digest is per-user mail: the caller sends their own, unless
    // they are an admin sending for another user.
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let (caller, is_admin) = authenticate(&ctx, &req)?;
        let user_id = resolve_actor_user_id(Some(&input.user_id), &caller.actor, is_admin)?;
        send_digest_for_user(&state, &user_id, input.email.as_deref())
    })();
    json::respond(result, debug)
}

// Delivery and audience handlers.

pub(crate) async fn list_deliveries(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    let (caller, is_admin) = match authenticate(&ctx, &req) {
        Ok(pair) => pair,
        Err(error) => return json::error_response(&error, debug),
    };
    let params = query.into_inner();
    // Delivery attempts carry recipient addresses and subjects, so a
    // non-admin caller only sees attempts for notifications visible
    // to them. Attempts with no notification (digest mail) have no
    // owner to scope to and are admin-only. An admin naming a
    // notification_id for another user's notification is forbidden.
    if let Some(notification_id) = params.filters.get("notification_id") {
        if let Some(notification) = state
            .notifications
            .get(notification_id.as_str())
            .map(|entry| entry.value().clone())
        {
            if let Err(error) =
                check_notification_access(&notification, &caller.actor, is_admin)
            {
                return json::error_response(&error, debug);
            }
        }
    }
    let mut items: Vec<DeliveryAttempt> = state
        .deliveries
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|attempt| {
            (is_admin || attempt.notification_id.as_deref().is_some_and(|id| {
                state
                    .notifications
                    .get(id)
                    .is_some_and(|entry| {
                        NotificationsState::is_visible_to(entry.value(), &caller.actor)
                    })
            })) && params
                .filters
                .get("notification_id")
                .is_none_or(|id| attempt.notification_id.as_deref() == Some(id.as_str()))
                && params
                    .filters
                    .get("channel")
                    .is_none_or(|channel| attempt.channel == *channel)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| attempt.status == *status)
                && params.filters.get("job_id").is_none_or(|job_id| {
                    attempt.job_id.as_deref() == Some(job_id.as_str())
                })
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn list_notification_deliveries(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<DeliveryAttempt>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let (caller, is_admin) = authenticate(&ctx, &req)?;
        let notification = state
            .notifications
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))?;
        check_notification_access(&notification, &caller.actor, is_admin)?;
        let mut items: Vec<DeliveryAttempt> = state
            .deliveries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|attempt| attempt.notification_id.as_deref() == Some(path.as_str()))
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

/// Retry one failed delivery attempt (email or webhook) under the
/// retry policy: at most `MAX_DELIVERY_ATTEMPTS` attempts, with the
/// backoff recorded as `next_retry_at`. The retry re-sends through
/// the same channel with the stored recipient, subject and template.

pub(crate) async fn retry_delivery(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let state: Arc<NotificationsState> = state.get_ref().clone();
    let (notification_id, attempt_id) = path.into_inner();
    let result: UnifiedResult<DeliveryAttempt> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let (caller, is_admin) = authenticate(&ctx, &req)?;
        let notification = state
            .notifications
            .get(notification_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))?;
        check_notification_access(&notification, &caller.actor, is_admin)?;
        // Snapshot first and drop the guard before sending: the send
        // emits hook events, and no hook handler may run while this
        // module holds a write guard on the deliveries map.
        let snapshot = state
            .deliveries
            .get(attempt_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Delivery attempt not found"))?;
        if snapshot.notification_id.as_deref() != Some(notification_id.as_str()) {
            return Err(UnifiedError::not_found("Delivery attempt not found"));
        }
        if snapshot.attempts >= snapshot.max_attempts {
            return Err(UnifiedError::unprocessable(
                "Delivery has reached the maximum number of attempts",
            ));
        }
        match snapshot.channel.as_str() {
            "email" => {
                let to = snapshot
                    .to
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request("No recipient email address was recorded for this attempt")
                    })?;
                let variables: HashMap<String, Value> = HashMap::new();
                let outcome = send_email(
                    &state,
                    &to,
                    &notification.title,
                    &notification.body,
                    snapshot.email_template_id.as_deref(),
                    &variables,
                    None,
                    json!({ "notification_id": notification.id, "event": notification.kind, "retry": true }),
                );
                let mut attempt = state
                    .deliveries
                    .get_mut(attempt_id.as_str())
                    .ok_or_else(|| UnifiedError::not_found("Delivery attempt not found"))?;
                attempt.attempts += 1;
                if outcome.queued {
                    attempt.status = "queued".to_string();
                    attempt.job_id = outcome.job_id;
                    attempt.provider = Some(outcome.via.to_string());
                    attempt.error = None;
                    attempt.next_retry_at = None;
                    attempt.updated_at = now();
                    Ok(attempt.clone())
                } else {
                    let reason = outcome
                        .error
                        .unwrap_or_else(|| "email send was not queued".to_string());
                    attempt.status = "failed".to_string();
                    attempt.error = Some(reason.clone());
                    attempt.next_retry_at = next_retry_at(attempt.attempts);
                    let failed = attempt.clone();
                    drop(attempt);
                    state.emit(
                        "notification.delivery.failed",
                        json!({
                            "attempt_id": failed.id,
                            "notification_id": notification.id,
                            "channel": "email",
                            "error": reason,
                            "attempts": failed.attempts,
                            "retryable": failed.attempts < failed.max_attempts,
                        }),
                    );
                    Ok(failed)
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
                        "retry": true,
                    }),
                );
                let mut attempt = state
                    .deliveries
                    .get_mut(attempt_id.as_str())
                    .ok_or_else(|| UnifiedError::not_found("Delivery attempt not found"))?;
                attempt.attempts += 1;
                attempt.status = "queued".to_string();
                attempt.next_retry_at = None;
                attempt.updated_at = now();
                Ok(attempt.clone())
            }
            other => Err(UnifiedError::unprocessable(format!(
                "Channel {other} is already delivered and cannot be retried"
            ))),
        }
    })();
    json::respond(result, debug)
}

pub(crate) fn valid_audience_address(audience: &str) -> bool {
    audience.starts_with("role:")
        && audience.len() > "role:".len()
        || audience.starts_with("department:")
        && audience.len() > "department:".len()
        || audience.starts_with("segment:")
        && audience.len() > "segment:".len()
}

/// Audience memberships list member emails, so audience
/// administration is operator-only: the caller must hold the admin
/// capability.
pub(crate) async fn list_audiences(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = require_admin(&ctx, &req) {
        return json::error_response(&error, debug);
    }
    let params = query.into_inner();
    let mut items: Vec<AudienceMembership> = state
        .audiences
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|membership| {
            matches_search(
                &format!(
                    "{} {}",
                    membership.audience,
                    membership
                        .members
                        .iter()
                        .map(|member| member.user_id.clone())
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                &params,
            )
        })
        .collect();
    items.sort_by(|a, b| a.audience.cmp(&b.audience));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_audience(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    body: web::Json<CreateAudience>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = require_admin(&ctx, &req) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<AudienceMembership> = (|| {
        let input = body.into_inner();
        let audience = input.audience.trim().to_string();
        if !valid_audience_address(&audience) {
            return Err(UnifiedError::bad_request(
                "audience must be role:<id>, department:<name> or segment:<name>",
            ));
        }
        let mut members: Vec<AudienceMember> = Vec::new();
        for member in input.members {
            let user_id = member.user_id.trim().to_string();
            if user_id.is_empty() || members.iter().any(|item| item.user_id == user_id) {
                continue;
            }
            members.push(AudienceMember {
                user_id,
                email: member.email.filter(|value| !value.trim().is_empty()),
            });
        }
        for user_id in input.user_ids {
            let user_id = user_id.trim().to_string();
            if user_id.is_empty() || members.iter().any(|item| item.user_id == user_id) {
                continue;
            }
            members.push(AudienceMember {
                user_id,
                email: None,
            });
        }
        if members.is_empty() {
            return Err(UnifiedError::bad_request(
                "At least one member is required",
            ));
        }
        let membership = AudienceMembership {
            audience: audience.clone(),
            members,
            updated_at: now(),
        };
        state
            .audiences
            .insert(audience, membership.clone());
        Ok(membership)
    })();
    match result {
        Ok(membership) => json::success_response(membership, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_audience(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AudienceMembership> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        require_admin(&ctx, &req)?;
        state
            .audiences
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Audience not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_audience(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        require_admin(&ctx, &req)?;
        state
            .audiences
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "audience": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Audience not found"))
    })();
    json::respond(result, debug)
}

// Notification template handlers.

/// Templates are operator configuration: every template endpoint
/// requires the admin capability.
pub(crate) async fn list_templates(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = require_admin(&ctx, &req) {
        return json::error_response(&error, debug);
    }
    let params = query.into_inner();
    let mut items: Vec<NotificationTemplate> = state
        .templates
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|template| {
            matches_search(
                &format!("{} {} {}", template.event, template.title_template, template.body_template),
                &params,
            ) && params
                .filters
                .get("event")
                .is_none_or(|event| template.event == *event)
                && params
                    .filters
                    .get("locale")
                    .is_none_or(|locale| template.locale == *locale)
                && params
                    .filters
                    .get("channel")
                    .is_none_or(|channel| template.channels.iter().any(|c| c == channel))
        })
        .collect();
    items.sort_by(|a, b| a.event.cmp(&b.event).then(a.locale.cmp(&b.locale)));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    body: web::Json<CreateNotificationTemplate>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return json::error_response(&error, debug);
    }
    if let Err(error) = require_admin(&ctx, &req) {
        return json::error_response(&error, debug);
    }
    let input = body.into_inner();
    if input.event.trim().is_empty() {
        return json::error_response(
            &UnifiedError::bad_request("Template event must not be empty"),
            debug,
        );
    }
    if input.title_template.trim().is_empty() {
        return json::error_response(
            &UnifiedError::bad_request("title_template must not be empty"),
            debug,
        );
    }
    if input.body_template.trim().is_empty() {
        return json::error_response(
            &UnifiedError::bad_request("body_template must not be empty"),
            debug,
        );
    }
    if let Err(error) = validate_channels(&input.channels) {
        return json::error_response(&error, debug);
    }
    // No email_connection_id is not an error: the provider's global
    // default connection is used when a template has none.
    let now = now();
    let template = NotificationTemplate {
        id: new_id(),
        event: input.event.trim().to_string(),
        locale: input
            .locale
            .unwrap_or_else(|| "en".to_string())
            .trim()
            .to_string(),
        title_template: input.title_template,
        body_template: input.body_template,
        channels: input.channels,
        email_connection_id: input.email_connection_id.filter(|value| !value.trim().is_empty()),
        email_template_id: input.email_template_id.filter(|value| !value.trim().is_empty()),
        created_at: now,
        updated_at: now,
    };
    state
        .templates
        .insert(template.id.clone(), template.clone());
    state.emit(
        "notification.template.created",
        json!({
            "template_id": template.id,
            "event": template.event,
            "locale": template.locale,
        }),
    );
    respond_created(Ok(template), debug)
}

pub(crate) async fn get_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<NotificationTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        require_admin(&ctx, &req)?;
        state
            .templates
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Notification template not found"))
    })();
    json::respond(result, debug)
}

/// Apply an update to a stored template, enforcing the same
/// non-empty rules `create_template` enforces: a template must never
/// be updated to render an empty title or body, and its event and
/// channels stay valid. Validates everything before mutating, so a
/// rejected update leaves the stored template untouched.
pub(crate) fn apply_template_update(
    template: &mut NotificationTemplate,
    input: UpdateNotificationTemplate,
) -> UnifiedResult<()> {
    if let Some(channels) = &input.channels {
        validate_channels(channels)?;
    }
    if let Some(event) = &input.event {
        if event.trim().is_empty() {
            return Err(UnifiedError::bad_request("Template event must not be empty"));
        }
    }
    if let Some(title_template) = &input.title_template {
        if title_template.trim().is_empty() {
            return Err(UnifiedError::bad_request("title_template must not be empty"));
        }
    }
    if let Some(body_template) = &input.body_template {
        if body_template.trim().is_empty() {
            return Err(UnifiedError::bad_request("body_template must not be empty"));
        }
    }
    if let Some(event) = input.event {
        template.event = event.trim().to_string();
    }
    if let Some(locale) = input.locale {
        template.locale = locale.trim().to_string();
    }
    if let Some(title_template) = input.title_template {
        template.title_template = title_template;
    }
    if let Some(body_template) = input.body_template {
        template.body_template = body_template;
    }
    if let Some(channels) = input.channels {
        template.channels = channels;
    }
    if let Some(email_connection_id) = input.email_connection_id {
        template.email_connection_id = if email_connection_id.trim().is_empty() {
            None
        } else {
            Some(email_connection_id)
        };
    }
    if let Some(email_template_id) = input.email_template_id {
        template.email_template_id = if email_template_id.trim().is_empty() {
            None
        } else {
            Some(email_template_id)
        };
    }
    template.updated_at = now();
    Ok(())
}

pub(crate) async fn update_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateNotificationTemplate>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<NotificationTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        require_admin(&ctx, &req)?;
        let input = body.into_inner();
        let mut template = state
            .templates
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Notification template not found"))?;
        apply_template_update(&mut template, input)?;
        Ok(template.clone())
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        require_admin(&ctx, &req)?;
        state
            .templates
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Notification template not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn preview_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TemplateRenderInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        require_admin(&ctx, &req)?;
        let template = state
            .templates
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Notification template not found"))?;
        let variables = body.into_inner().variables;
        Ok(json!({
            "template_id": template.id,
            "event": template.event,
            "locale": template.locale,
            "title": render_template_text(&template.title_template, &variables),
            "body": render_template_text(&template.body_template, &variables),
            "channels": template.channels,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn test_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<NotificationsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TemplateTestInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let state: Arc<NotificationsState> = state.get_ref().clone();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        require_admin(&ctx, &req)?;
        let input = body.into_inner();
        let template = state
            .templates
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Notification template not found"))?;
        let mut variables = input.variables.clone();
        variables.insert(
            "user_id".to_string(),
            Value::String(input.user_id.clone()),
        );
        let title = render_template_text(&template.title_template, &variables);
        let body_text = render_template_text(&template.body_template, &variables);
        let notification = Notification {
            id: new_id(),
            user_id: input.user_id.clone(),
            title: title.clone(),
            body: body_text.clone(),
            kind: template.event.clone(),
            category: category_for_kind(&template.event).to_string(),
            module: module_for_kind(&template.event).to_string(),
            priority: "normal".to_string(),
            link: None,
            audience: "user".to_string(),
            read: false,
            created_at: now(),
        };
        // A test render is not stored; it only dispatches through the
        // template's channels so the operator can see the real output.
        dispatch_template_channels(&state, &template, &notification, input.email, &variables);
        Ok(json!({
            "template_id": template.id,
            "event": template.event,
            "title": title,
            "body": body_text,
            "channels": template.channels,
            "dispatched": true,
        }))
    })();
    json::respond(result, debug)
}

// Hook handlers

#[cfg(test)]
mod tests {
    use super::*;

    fn test_notification(user_id: &str) -> Notification {
        Notification {
            id: "n-1".to_string(),
            user_id: user_id.to_string(),
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

    fn test_template() -> NotificationTemplate {
        NotificationTemplate {
            id: "t-1".to_string(),
            event: "order.paid".to_string(),
            locale: "en".to_string(),
            title_template: "Order paid".to_string(),
            body_template: "Order {{id}} was paid.".to_string(),
            channels: vec!["in_app".to_string()],
            email_connection_id: None,
            email_template_id: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn empty_update() -> UpdateNotificationTemplate {
        UpdateNotificationTemplate {
            event: None,
            locale: None,
            title_template: None,
            body_template: None,
            channels: None,
            email_connection_id: None,
            email_template_id: None,
        }
    }

    #[test]
    fn access_allows_owner_and_broadcast_to_anyone() {
        let own = test_notification("user-1");
        assert!(check_notification_access(&own, "user-1", false).is_ok());
        let broadcast = test_notification("all");
        assert!(check_notification_access(&broadcast, "user-2", false).is_ok());
    }

    #[test]
    fn access_forbids_other_users_unless_admin() {
        let notification = test_notification("user-1");
        assert!(check_notification_access(&notification, "user-2", false).is_err());
        assert!(check_notification_access(&notification, "user-2", true).is_ok());
    }

    #[test]
    fn template_update_rejects_empty_title_and_body() {
        let mut template = test_template();
        let mut input = empty_update();
        input.title_template = Some("   ".to_string());
        assert!(apply_template_update(&mut template, input).is_err());
        assert_eq!(template.title_template, "Order paid");

        let mut input = empty_update();
        input.body_template = Some(String::new());
        assert!(apply_template_update(&mut template, input).is_err());
        assert_eq!(template.body_template, "Order {{id}} was paid.");
    }

    #[test]
    fn template_update_rejects_empty_event_and_unknown_channels() {
        let mut template = test_template();
        let mut input = empty_update();
        input.event = Some("  ".to_string());
        assert!(apply_template_update(&mut template, input).is_err());
        assert_eq!(template.event, "order.paid");

        let mut input = empty_update();
        input.channels = Some(vec!["sms".to_string()]);
        assert!(apply_template_update(&mut template, input).is_err());
        assert_eq!(template.channels, vec!["in_app".to_string()]);
    }

    #[test]
    fn template_update_applies_valid_changes() {
        let mut template = test_template();
        let mut input = empty_update();
        input.title_template = Some("Paid!".to_string());
        input.body_template = Some("Done.".to_string());
        input.locale = Some("id".to_string());
        assert!(apply_template_update(&mut template, input).is_ok());
        assert_eq!(template.title_template, "Paid!");
        assert_eq!(template.body_template, "Done.");
        assert_eq!(template.locale, "id");
        assert_eq!(template.event, "order.paid");
    }
}
