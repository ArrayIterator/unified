//! HTTP handlers for announcements: CRUD, publish, reads, feed.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use serde_json::{json, Value};
use std::sync::{Arc};
use uuid::Uuid;
use super::types::{AUDIENCE_ALL, Announcement, CreateAnnouncementRequest, MarkReadRequest, ReadReceipt, STATUS_ARCHIVED, STATUS_DRAFT, STATUS_PUBLISHED, UpdateAnnouncementRequest, default_announcement_channels};
use super::validation::{
    PERM_ANNOUNCEMENTS_MANAGE, PERM_ANNOUNCEMENTS_VIEW, audience_matches, authorize, caller,
    caller_company, matches_search, paginate, parse_uuid, require_company, require_tenant,
    resolve_identity, sort_announcements, valid_announcement_status, valid_audience,
    validate_audience, validate_channels, validate_title_body, validate_user_id, validate_window,
};
use super::state::AnnouncementsState;

// ---------------------------------------------------------------------------
// Announcement handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_announcements(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnnouncementsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_ANNOUNCEMENTS_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let mut items: Vec<Announcement> = state
        .announcements
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|announcement| {
            matches_search(
                &format!("{} {}", announcement.title, announcement.body),
                &params,
            ) && params.filters.get("status").map_or(true, |status| {
                announcement.status == *status
            }) && params.filters.get("audience").map_or(true, |audience| {
                announcement.audience == *audience
            }) && params.filters.get("pinned").map_or(true, |pinned| {
                announcement.pinned == (*pinned == "true" || *pinned == "1")
            })
        })
        .collect();
    items = sort_announcements(items, &params);
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

/// PUBLIC: published, currently visible announcements only.
/// Caller identity comes from the authenticated identity headers
/// (`X-User-Id`, `X-Role`, `X-Department`, `X-Segment`); query
/// parameters never identify the caller. A caller without identity
/// headers sees broadcast announcements only. Public access is gated
/// by the API Manager's `ApiAccessRule` for this path (default deny
/// without a rule).
pub(crate) async fn list_public_announcements(
    state: web::Data<Arc<AnnouncementsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let now = Utc::now();
    let identity = resolve_identity(&req);
    let user_id = identity.user_id;
    let roles = identity.roles;
    let departments = identity.departments;
    let segments = identity.segments;
    let mut items: Vec<Announcement> = state
        .announcements
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|announcement| {
            AnnouncementsState::is_visible_now(announcement, now)
                && matches_search(
                    &format!("{} {}", announcement.title, announcement.body),
                    &params,
                )
                && (if user_id.trim().is_empty() {
                    announcement.audience == AUDIENCE_ALL
                } else {
                    audience_matches(announcement, &user_id, &roles, &departments, &segments)
                })
                && params.filters.get("audience").map_or(true, |audience| {
                    announcement.audience == *audience
                })
        })
        .collect();
    // Pinned first, then newest publish time.
    items.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_announcement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnnouncementsState>>,
    req: HttpRequest,
    body: web::Json<CreateAnnouncementRequest>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_company(&_tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_ANNOUNCEMENTS_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<Announcement> = (|| {
        let input = body.into_inner();
        validate_title_body(&input.title, &input.body)?;
        let audience = input.audience.clone().unwrap_or_else(|| AUDIENCE_ALL.to_string());
        let audience_value = input
            .audience_value
            .clone()
            .filter(|value| !value.trim().is_empty());
        validate_audience(
            &audience,
            audience_value.as_deref(),
            &input.audience_user_ids,
            &input.audience_emails,
        )?;
        let channels = input.channels.clone().unwrap_or_else(default_announcement_channels);
        validate_channels(&channels)?;
        validate_window(input.publish_at, input.expires_at)?;
        let now = Utc::now();
        let announcement = Announcement {
            id: Uuid::now_v7(),
            title: input.title.trim().to_string(),
            body: input.body.trim().to_string(),
            audience,
            audience_value,
            audience_user_ids: input.audience_user_ids.clone(),
            audience_emails: input.audience_emails.clone(),
            channels,
            email_template_id: input
                .email_template_id
                .clone()
                .filter(|value| !value.trim().is_empty()),
            status: STATUS_DRAFT.to_string(),
            publish_at: input.publish_at,
            expires_at: input.expires_at,
            pinned: input.pinned,
            created_at: now,
            updated_at: now,
            published_at: None,
        };
        state
            .announcements
            .insert(announcement.id, announcement.clone());
        Ok(announcement)
    })();
    match result {
        Ok(announcement) => json::success_response(announcement, 201),
        Err(error) => json::error_response(&error, ctx.is_debug()),
    }
}

pub(crate) async fn get_announcement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnnouncementsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_ANNOUNCEMENTS_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<Announcement> = (|| {
        let id = parse_uuid(path.as_str(), "announcement")?;
        state
            .announcements
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Announcement not found"))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_announcement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnnouncementsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateAnnouncementRequest>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_company(&_tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_ANNOUNCEMENTS_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<Announcement> = (|| {
        let id = parse_uuid(path.as_str(), "announcement")?;
        let input = body.into_inner();
        let mut announcement = state
            .announcements
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Announcement not found"))?;
        if let Some(title) = input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request("title must not be empty"));
            }
            announcement.title = title.trim().to_string();
        }
        if let Some(body) = input.body {
            if body.trim().is_empty() {
                return Err(UnifiedError::bad_request("body must not be empty"));
            }
            announcement.body = body.trim().to_string();
        }
        if let Some(audience) = input.audience {
            if !valid_audience(&audience) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid audience \"{audience}\""
                )));
            }
            announcement.audience = audience;
        }
        if let Some(audience_value) = input.audience_value {
            announcement.audience_value = if audience_value.trim().is_empty() {
                None
            } else {
                Some(audience_value.trim().to_string())
            };
        }
        if let Some(user_ids) = input.audience_user_ids {
            announcement.audience_user_ids = user_ids;
        }
        if let Some(emails) = input.audience_emails {
            announcement.audience_emails = emails;
        }
        if let Some(channels) = input.channels {
            validate_channels(&channels)?;
            announcement.channels = channels;
        }
        if let Some(email_template_id) = input.email_template_id {
            announcement.email_template_id = if email_template_id.trim().is_empty() {
                None
            } else {
                Some(email_template_id)
            };
        }
        if input.publish_at.is_some() {
            announcement.publish_at = input.publish_at;
        }
        if input.expires_at.is_some() {
            announcement.expires_at = input.expires_at;
        }
        validate_window(announcement.publish_at, announcement.expires_at)?;
        if let Some(pinned) = input.pinned {
            announcement.pinned = pinned;
        }
        validate_audience(
            &announcement.audience,
            announcement.audience_value.as_deref(),
            &announcement.audience_user_ids,
            &announcement.audience_emails,
        )?;
        if let Some(status) = input.status {
            if !valid_announcement_status(&status) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid status \"{status}\": expected draft, published, or archived"
                )));
            }
            announcement.status = status;
        }
        announcement.updated_at = Utc::now();
        let snapshot = announcement.clone();
        drop(announcement);
        // Publishing only fans out when the publish time has arrived;
        // a future publish_at stays scheduled until publish-due runs.
        state.maybe_publish(&snapshot);
        let snapshot = state
            .announcements
            .get(&id)
            .map(|entry| entry.value().clone())
            .unwrap_or(snapshot);
        Ok(snapshot)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_announcement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnnouncementsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_company(&_tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_ANNOUNCEMENTS_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<Value> = (|| {
        let id = parse_uuid(path.as_str(), "announcement")?;
        if !state.announcements.contains_key(&id) {
            return Err(UnifiedError::not_found("Announcement not found"));
        }
        state.announcements.remove(&id);
        state.reads.retain(|_, receipt| receipt.announcement_id != id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn publish_announcement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnnouncementsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_company(&_tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_ANNOUNCEMENTS_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<Announcement> = (|| {
        let id = parse_uuid(path.as_str(), "announcement")?;
        let mut announcement = state
            .announcements
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Announcement not found"))?;
        if announcement.status == STATUS_PUBLISHED {
            return Err(UnifiedError::conflict(
                "Announcement is already published",
            ));
        }
        if announcement.status == STATUS_ARCHIVED {
            return Err(UnifiedError::conflict(
                "An archived announcement cannot be published; create a new one instead",
            ));
        }
        let now = Utc::now();
        announcement.status = STATUS_PUBLISHED.to_string();
        announcement.updated_at = now;
        let snapshot = announcement.clone();
        drop(announcement);
        // Only fans out when the publish time has arrived. A future
        // publish_at leaves the announcement scheduled
        // (`published_at` stays empty) until publish-due runs.
        state.maybe_publish(&snapshot);
        let snapshot = state
            .announcements
            .get(&id)
            .map(|entry| entry.value().clone())
            .unwrap_or(snapshot);
        Ok(snapshot)
    })();
    json::respond(result, ctx.is_debug())
}

/// Publish every announcement whose publish time has arrived.
/// Driven by the scheduler through the `announcement.publish_due`
/// hook (see the scheduler arm in the report), or by an operator.
pub(crate) async fn publish_due(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnnouncementsState>>,
    req: HttpRequest,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_company(&_tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_ANNOUNCEMENTS_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let published = state.publish_due();
    let ids: Vec<Uuid> = published.iter().map(|announcement| announcement.id).collect();
    json::success_response(
        json!({ "published": published.len(), "announcement_ids": ids }),
        200,
    )
}

pub(crate) async fn archive_announcement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnnouncementsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_company(&_tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_ANNOUNCEMENTS_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<Announcement> = (|| {
        let id = parse_uuid(path.as_str(), "announcement")?;
        let mut announcement = state
            .announcements
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Announcement not found"))?;
        if announcement.status == STATUS_ARCHIVED {
            return Err(UnifiedError::conflict("Announcement is already archived"));
        }
        announcement.status = STATUS_ARCHIVED.to_string();
        announcement.updated_at = Utc::now();
        Ok(announcement.clone())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn mark_announcement_read(
    state: web::Data<Arc<AnnouncementsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MarkReadRequest>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _company = match require_company(&_tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, false),
    };
    // A caller may only mark their own announcements read: the
    // receipt is recorded for the authenticated caller, and a body
    // `user_id` naming anyone else is refused.
    let found = match caller(&req) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<ReadReceipt> = (|| {
        let id = parse_uuid(path.as_str(), "announcement")?;
        let user_id = body.user_id.trim().to_string();
        validate_user_id(&user_id)?;
        if user_id != found.actor {
            return Err(UnifiedError::forbidden(
                "user_id must match the authenticated caller",
            ));
        }
        if !state.announcements.contains_key(&id) {
            return Err(UnifiedError::not_found("Announcement not found"));
        }
        let key = format!("{id}:{user_id}");
        if let Some(existing) = state.reads.get(&key) {
            return Ok(existing.value().clone());
        }
        let receipt = ReadReceipt {
            announcement_id: id,
            user_id: user_id.clone(),
            read_at: Utc::now(),
        };
        state.reads.insert(key, receipt.clone());
        Ok(receipt)
    })();
    json::respond(result, false)
}

pub(crate) async fn list_announcement_reads(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnnouncementsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_ANNOUNCEMENTS_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<ReadReceipt>> = (|| {
        let id = parse_uuid(path.as_str(), "announcement")?;
        if !state.announcements.contains_key(&id) {
            return Err(UnifiedError::not_found("Announcement not found"));
        }
        let mut items: Vec<ReadReceipt> = state
            .reads
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|receipt| receipt.announcement_id == id)
            .collect();
        items.sort_by(|a, b| b.read_at.cmp(&a.read_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, ctx.is_debug())
}

/// Published, visible announcements the user has not read yet.
/// The user is the authenticated caller (`X-User-Id` / `X-Role`
/// headers); query parameters never identify the caller.
pub(crate) async fn list_unread(
    state: web::Data<Arc<AnnouncementsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let found = match caller(&req) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let user_id = found.actor;
    let now = Utc::now();
    let roles = vec![found.role];
    let identity = resolve_identity(&req);
    let departments = identity.departments;
    let segments = identity.segments;
    let mut items: Vec<Announcement> = state
        .announcements
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|announcement| {
            AnnouncementsState::is_visible_now(announcement, now)
                && audience_matches(announcement, &user_id, &roles, &departments, &segments)
                && !state
                    .reads
                    .contains_key(&format!("{}:{user_id}", announcement.id))
        })
        .collect();
    items.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

/// User-dashboard feed: the audience-filtered, currently visible
/// announcements for one user plus the unread count. The dashboards
/// module does not read announcements itself, so this endpoint is the
/// feed it (or any client) renders. The user is the authenticated
/// caller (`X-User-Id` / `X-Role` headers); query parameters never
/// identify the caller.
pub(crate) async fn dashboard_feed(
    state: web::Data<Arc<AnnouncementsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let found = match caller(&req) {
        Ok(found) => found,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let user_id = found.actor;
    let roles = vec![found.role];
    let identity = resolve_identity(&req);
    let departments = identity.departments;
    let segments = identity.segments;
    let now = Utc::now();
    let mut items: Vec<Announcement> = state
        .announcements
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|announcement| {
            AnnouncementsState::is_visible_now(announcement, now)
                && audience_matches(announcement, &user_id, &roles, &departments, &segments)
        })
        .collect();
    items.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
    let unread = items
        .iter()
        .filter(|announcement| {
            !state
                .reads
                .contains_key(&format!("{}:{user_id}", announcement.id))
        })
        .count() as u64;
    let page = paginate(items, &params);
    json::success_response(
        json!({
            "user_id": user_id,
            "unread_announcements": unread,
            "announcements": page.items,
            "pagination": page.pagination,
        }),
        200,
    )
}
