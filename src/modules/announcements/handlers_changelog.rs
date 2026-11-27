//! HTTP handlers for changelog entries.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use serde_json::{json, Value};
use std::sync::{Arc};
use uuid::Uuid;
use super::types::{CATEGORY_FEATURE, ChangelogEntry, CreateChangelogRequest, UpdateChangelogRequest};
use super::validation::{
    PERM_ANNOUNCEMENTS_MANAGE, authorize, caller_company, matches_search, paginate, parse_uuid,
    require_company, require_tenant, valid_category, validate_date, validate_title_body,
    validate_version,
};
use super::state::AnnouncementsState;

// Changelog handlers

/// PUBLIC: changelog entries, newest first.
/// No auth is required; public access is gated by the API Manager's
/// `ApiAccessRule` for this path (default deny without a rule).
pub(crate) async fn list_changelog(
    state: web::Data<Arc<AnnouncementsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<ChangelogEntry> = state
        .changelog
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            matches_search(&format!("{} {} {}", entry.version, entry.title, entry.body), &params)
                && params.filters.get("category").map_or(true, |category| {
                    entry.category == *category
                })
                && params.filters.get("version").map_or(true, |version| {
                    entry.version == *version
                })
        })
        .collect();
    items.sort_by(|a, b| {
        b.date
            .cmp(&a.date)
            .then_with(|| b.version.cmp(&a.version))
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_changelog(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnnouncementsState>>,
    req: HttpRequest,
    body: web::Json<CreateChangelogRequest>,
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
    let result: UnifiedResult<ChangelogEntry> = (|| {
        let input = body.into_inner();
        validate_version(&input.version)?;
        validate_date(&input.date)?;
        let category = input.category.clone().unwrap_or_else(|| CATEGORY_FEATURE.to_string());
        if !valid_category(&category) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid category \"{category}\": expected feature, fix, or security"
            )));
        }
        validate_title_body(&input.title, &input.body)?;
        if let Some(link) = &input.link {
            if link.trim().is_empty() {
                return Err(UnifiedError::bad_request("link must not be empty"));
            }
            if !(link.starts_with("https://") || link.starts_with("http://")) {
                return Err(UnifiedError::bad_request(
                    "link must be an http(s) URL",
                ));
            }
        }
        let now = Utc::now();
        let entry = ChangelogEntry {
            id: Uuid::now_v7(),
            version: input.version.trim().to_string(),
            date: input.date.trim().to_string(),
            category,
            title: input.title.trim().to_string(),
            body: input.body.trim().to_string(),
            link: input.link.map(|link| link.trim().to_string()),
            created_at: now,
            updated_at: now,
        };
        state.changelog.insert(entry.id, entry.clone());
        state.emit(
            "changelog.created",
            json!({
                "entry_id": entry.id,
                "version": entry.version,
                "category": entry.category,
                "title": entry.title,
            }),
        );
        Ok(entry)
    })();
    match result {
        Ok(entry) => json::success_response(entry, 201),
        Err(error) => json::error_response(&error, ctx.is_debug()),
    }
}

/// PUBLIC: one changelog entry.
/// No auth is required; public access is gated by the API Manager's
/// `ApiAccessRule` for this path (default deny without a rule).
pub(crate) async fn get_changelog(
    state: web::Data<Arc<AnnouncementsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ChangelogEntry> = (|| {
        let id = parse_uuid(path.as_str(), "changelog entry")?;
        state
            .changelog
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Changelog entry not found"))
    })();
    json::respond(result, false)
}

pub(crate) async fn update_changelog(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnnouncementsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateChangelogRequest>,
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
    let result: UnifiedResult<ChangelogEntry> = (|| {
        let id = parse_uuid(path.as_str(), "changelog entry")?;
        let input = body.into_inner();
        let mut entry = state
            .changelog
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Changelog entry not found"))?;
        if let Some(version) = input.version {
            validate_version(&version)?;
            entry.version = version.trim().to_string();
        }
        if let Some(date) = input.date {
            validate_date(&date)?;
            entry.date = date.trim().to_string();
        }
        if let Some(category) = input.category {
            if !valid_category(&category) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid category \"{category}\""
                )));
            }
            entry.category = category;
        }
        if let Some(title) = input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request("title must not be empty"));
            }
            entry.title = title.trim().to_string();
        }
        if let Some(body) = input.body {
            if body.trim().is_empty() {
                return Err(UnifiedError::bad_request("body must not be empty"));
            }
            entry.body = body.trim().to_string();
        }
        if let Some(link) = input.link {
            if !(link.starts_with("https://") || link.starts_with("http://")) {
                return Err(UnifiedError::bad_request(
                    "link must be an http(s) URL",
                ));
            }
            entry.link = Some(link.trim().to_string());
        }
        entry.updated_at = Utc::now();
        Ok(entry.clone())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_changelog(
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
        let id = parse_uuid(path.as_str(), "changelog entry")?;
        if !state.changelog.contains_key(&id) {
            return Err(UnifiedError::not_found("Changelog entry not found"));
        }
        state.changelog.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, ctx.is_debug())
}
