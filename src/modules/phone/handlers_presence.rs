//! HTTP handlers for presence: one user's status
//! (available / busy / do-not-disturb / offline), set by the
//! user themselves or read by anyone in the tenant.
//!
//! Presence is owned by this module. No `user.presence.*`
//! hook event exists anywhere in the tree (verified by grep
//! over `src/modules/`), and the chat module's presence is
//! per-room state, not a hook event — so there is nothing to
//! listen to. Other modules that need a user's availability
//! read this API or the `phone.presence.updated` event.

use super::helpers::{caller, err, ok, optional_text, paginate, require_user_id, resolve_tenant};
use super::state::PhoneState;
use super::types::{
    PresenceEntry, PresenceStatus, SetPresenceRequest, EVENT_PRESENCE_UPDATED, MAX_LABEL_LEN,
    PRESENCE_STALE_SECS,
};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;

/// Path carrying a user id.
#[derive(Debug, Deserialize)]
struct UserPath {
    user_id: String,
}

fn presence_key(tenant_id: &str, user_id: &str) -> String {
    format!("{tenant_id}:{user_id}")
}

/// The effective status of an entry: a stale entry (no
/// heartbeat within `PRESENCE_STALE_SECS`) reads as offline,
/// so presence never reports ghosts.
fn effective_status(entry: &PresenceEntry, now: chrono::DateTime<Utc>) -> PresenceStatus {
    if (now - entry.updated_at).num_seconds() > PRESENCE_STALE_SECS {
        PresenceStatus::Offline
    } else {
        entry.status
    }
}

fn presence_view(entry: &PresenceEntry, now: chrono::DateTime<Utc>) -> Value {
    json!({
        "tenant_id": entry.tenant_id,
        "user_id": entry.user_id,
        "status": effective_status(entry, now).as_str(),
        "declared_status": entry.status.as_str(),
        "note": entry.note,
        "updated_at": entry.updated_at.to_rfc3339(),
        "stale": (now - entry.updated_at).num_seconds() > PRESENCE_STALE_SECS,
    })
}

/// List presence of the tenant's users (paginated, filter by
/// status). Users with no row read as offline and are not
/// listed — presence only tracks users who have set it.
pub(crate) async fn list_presence(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let params = query.into_inner();
        let status_filter = params
            .filters
            .get("status")
            .and_then(|raw| PresenceStatus::from_str(raw));
        let now = Utc::now();
        let mut items: Vec<PresenceEntry> = state
            .presence
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        if let Some(status) = status_filter {
            items.retain(|entry| effective_status(entry, now) == status);
        }
        let views: Vec<Value> = items
            .iter()
            .map(|entry| presence_view(entry, now))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Get one user's presence. A user with no row reads as
/// offline.
pub(crate) async fn get_presence(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<UserPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let user_id = require_user_id(&path.user_id)?;
        let now = Utc::now();
        let key = presence_key(&tenant_id, &user_id);
        match state.presence.get(&key).map(|entry| entry.value().clone()) {
            Some(entry) => Ok(presence_view(&entry, now)),
            None => Ok(json!({
                "tenant_id": tenant_id,
                "user_id": user_id,
                "status": "offline",
                "declared_status": "offline",
                "note": Value::Null,
                "updated_at": Value::Null,
                "stale": true,
            })),
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Set the caller's own presence. A user may only set their
/// own row — presence is identity, not admin data.
pub(crate) async fn set_presence(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<SetPresenceRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let input = body.into_inner();
        let status = PresenceStatus::from_str(&input.status).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown presence status: {} (expected available, busy, do_not_disturb, or offline)",
                input.status
            ))
        })?;
        let note = optional_text(&input.note, "note", MAX_LABEL_LEN)?;
        let now = Utc::now();
        let entry = PresenceEntry {
            tenant_id: tenant_id.clone(),
            user_id: caller.actor.clone(),
            status,
            note,
            updated_at: now,
        };
        state
            .presence
            .insert(presence_key(&tenant_id, &caller.actor), entry.clone());
        state.emit(
            EVENT_PRESENCE_UPDATED,
            json!({
                "tenant_id": tenant_id,
                "user_id": caller.actor,
                "status": status.as_str(),
            }),
        );
        Ok(presence_view(&entry, now))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Presence heartbeat: refreshes the caller's
/// `updated_at` without changing the declared status, so a
/// connected client stays non-stale. Creates an `available`
/// row when the caller has no presence yet.
pub(crate) async fn heartbeat_presence(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let now = Utc::now();
        let key = presence_key(&tenant_id, &caller.actor);
        let entry = match state.presence.get(&key).map(|entry| entry.value().clone()) {
            Some(mut entry) => {
                entry.updated_at = now;
                entry
            }
            None => PresenceEntry {
                tenant_id: tenant_id.clone(),
                user_id: caller.actor.clone(),
                status: PresenceStatus::Available,
                note: None,
                updated_at: now,
            },
        };
        state.presence.insert(key, entry.clone());
        Ok(presence_view(&entry, now))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
