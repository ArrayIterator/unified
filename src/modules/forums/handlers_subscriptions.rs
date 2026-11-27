//! HTTP handlers for thread and forum subscriptions.
//!
//! Subscribing is idempotent: subscribing twice returns the
//! existing row instead of creating a duplicate. Subscribers are
//! notified (through `notification.requested`) when new content
//! lands on the target they follow.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{caller, created, err, load_forum, load_thread, ok, paginate, require_tenant, resolve_tenant};
use super::state::ForumState;
use super::types::ForumSubscription;

// Thread subscriptions

pub(crate) async fn subscribe_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ForumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<(Value, bool)> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let thread = load_thread(&state, &tenant_id, &path.into_inner())?;
        if let Some(existing) = state
            .subscriptions
            .iter()
            .map(|entry| entry.value().clone())
            .find(|subscription| {
                subscription.tenant_id == tenant_id
                    && subscription.user_id == caller.actor
                    && subscription.target_type == "thread"
                    && subscription.target_id == thread.id
            })
        {
            return Ok((
                serde_json::to_value(&existing).unwrap_or(Value::Null),
                false,
            ));
        }
        let subscription = ForumSubscription {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            user_id: caller.actor.clone(),
            target_type: "thread".to_string(),
            target_id: thread.id,
            created_at: Utc::now(),
        };
        state
            .subscriptions
            .insert(subscription.id, subscription.clone());
        state.emit(
            "forums.subscription.created",
            json!({
                "subscription_id": subscription.id.to_string(),
                "target_type": "thread",
                "target_id": thread.id.to_string(),
                "user_id": caller.actor,
            }),
        );
        Ok((
            serde_json::to_value(&subscription).unwrap_or(Value::Null),
            true,
        ))
    })();
    match result {
        Ok((data, was_created)) => {
            if was_created {
                created(data)
            } else {
                ok(data)
            }
        }
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn unsubscribe_thread(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ForumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let thread = load_thread(&state, &tenant_id, &path.into_inner())?;
        let existing_id = state
            .subscriptions
            .iter()
            .find(|entry| {
                let subscription = entry.value();
                subscription.tenant_id == tenant_id
                    && subscription.user_id == caller.actor
                    && subscription.target_type == "thread"
                    && subscription.target_id == thread.id
            })
            .map(|entry| *entry.key());
        if let Some(id) = existing_id {
            state.subscriptions.remove(&id);
            state.emit(
                "forums.subscription.deleted",
                json!({
                    "subscription_id": id.to_string(),
                    "target_type": "thread",
                    "target_id": thread.id.to_string(),
                    "user_id": caller.actor,
                }),
            );
        }
        Ok(json!({
            "target_type": "thread",
            "target_id": thread.id.to_string(),
            "subscribed": false,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Forum subscriptions

pub(crate) async fn subscribe_forum(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ForumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<(Value, bool)> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let forum = load_forum(&state, &tenant_id, &path.into_inner())?;
        if let Some(existing) = state
            .subscriptions
            .iter()
            .map(|entry| entry.value().clone())
            .find(|subscription| {
                subscription.tenant_id == tenant_id
                    && subscription.user_id == caller.actor
                    && subscription.target_type == "forum"
                    && subscription.target_id == forum.id
            })
        {
            return Ok((
                serde_json::to_value(&existing).unwrap_or(Value::Null),
                false,
            ));
        }
        let subscription = ForumSubscription {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            user_id: caller.actor.clone(),
            target_type: "forum".to_string(),
            target_id: forum.id,
            created_at: Utc::now(),
        };
        state
            .subscriptions
            .insert(subscription.id, subscription.clone());
        state.emit(
            "forums.subscription.created",
            json!({
                "subscription_id": subscription.id.to_string(),
                "target_type": "forum",
                "target_id": forum.id.to_string(),
                "user_id": caller.actor,
            }),
        );
        Ok((
            serde_json::to_value(&subscription).unwrap_or(Value::Null),
            true,
        ))
    })();
    match result {
        Ok((data, was_created)) => {
            if was_created {
                created(data)
            } else {
                ok(data)
            }
        }
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn unsubscribe_forum(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ForumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let forum = load_forum(&state, &tenant_id, &path.into_inner())?;
        let existing_id = state
            .subscriptions
            .iter()
            .find(|entry| {
                let subscription = entry.value();
                subscription.tenant_id == tenant_id
                    && subscription.user_id == caller.actor
                    && subscription.target_type == "forum"
                    && subscription.target_id == forum.id
            })
            .map(|entry| *entry.key());
        if let Some(id) = existing_id {
            state.subscriptions.remove(&id);
            state.emit(
                "forums.subscription.deleted",
                json!({
                    "subscription_id": id.to_string(),
                    "target_type": "forum",
                    "target_id": forum.id.to_string(),
                    "user_id": caller.actor,
                }),
            );
        }
        Ok(json!({
            "target_type": "forum",
            "target_id": forum.id.to_string(),
            "subscribed": false,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Own subscriptions

pub(crate) async fn list_subscriptions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ForumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .subscriptions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|subscription| {
                subscription.tenant_id == tenant_id && subscription.user_id == caller.actor
            })
            .map(|subscription| {
                // Each row already carries its target_type and
                // target_id; they are surfaced here as the
                // enrichment the list contract promises.
                let mut value = serde_json::to_value(&subscription).unwrap_or(Value::Null);
                if let Value::Object(map) = &mut value {
                    map.insert(
                        "target_type".to_string(),
                        Value::String(subscription.target_type.clone()),
                    );
                    map.insert(
                        "target_id".to_string(),
                        Value::String(subscription.target_id.to_string()),
                    );
                }
                value
            })
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
