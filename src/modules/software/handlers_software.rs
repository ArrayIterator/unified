//! HTTP handlers for software records, history and statistics.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use semver::Version;
use serde_json::{json, Value};
use std::collections::{HashMap};
use std::sync::{Arc};
use uuid::Uuid;
use super::helpers::{
    caller, caller_company, require_company_for_write, file_company, in_company_scope, release_company,
    require_company_match, require_company_match_for, require_record_in_company_scope,
    require_software_in_scope, require_tenant, require_permission, version_company,
};
use super::types::Release;
use super::types::{AttachTag, CreateSoftware, LatestRelease, PERM_SOFTWARE_CREATE, PERM_SOFTWARE_MANAGE, PERM_SOFTWARE_READ, SOFTWARE_STATUSES, Software, SoftwareDetail, SoftwareReleaseHistory, SoftwareVersion, Tag, UpdateSoftware};
use super::state::SoftwareState;
use super::validation::{matches_search, new_id, now, paginate, respond_created, slugify, validate_kind, validate_status_transition, validate_url};
use super::handlers_versions::release_files;

// ---------------------------------------------------------------------------
// Software record handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_software(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    let sort = params.allowed_sort(&["name", "slug", "status", "created_at"], "created_at");
    let tag_filter = params.filters.get("tag").cloned();
    let mut items: Vec<Software> = state
        .software
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|software| {
            in_company_scope(&software.vendor_company_id, &scope) && matches_search(
                &format!("{} {} {}", software.name, software.slug, software.description),
                &params,
            ) && params.filters.get("kind").is_none_or(|kind| {
                software.kind == *kind
            }) && params.filters.get("status").is_none_or(|status| {
                software.status == *status
            }) && params.filters.get("group_id").is_none_or(|group_id| {
                software.group_id.as_deref() == Some(group_id.as_str())
            }) && params.filters.get("category_id").is_none_or(|category_id| {
                software.category_id.as_deref() == Some(category_id.as_str())
            }) && params.filters.get("vendor_company_id").is_none_or(|vendor| {
                software.vendor_company_id.as_deref() == Some(vendor.as_str())
            }) && tag_filter.as_ref().is_none_or(|tag_id| {
                state
                    .software_tags
                    .get(&software.id)
                    .is_some_and(|tags| tags.contains(tag_id.as_str()))
            })
        })
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "slug" => items.sort_by(|a, b| a.slug.cmp(&b.slug)),
        "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) fn validate_software_refs(
    state: &SoftwareState,
    vendor_company_id: Option<&str>,
    group_id: Option<&str>,
    category_id: Option<&str>,
) -> UnifiedResult<()> {
    if let Some(vendor) = vendor_company_id {
        if vendor.trim().is_empty() {
            return Err(UnifiedError::bad_request("Vendor company id must not be empty"));
        }
        if vendor.parse::<Uuid>().is_err() {
            return Err(UnifiedError::bad_request(
                "Vendor company id must be a UUID",
            ));
        }
        if !state.vendor_exists(vendor) {
            return Err(UnifiedError::not_found("Vendor company not found"));
        }
    }
    if let Some(group_id) = group_id {
        match state.taxonomy.get(group_id) {
            Some(node) if node.kind == "group" => {}
            _ => return Err(UnifiedError::not_found("Group not found")),
        }
    }
    if let Some(category_id) = category_id {
        match state.taxonomy.get(category_id) {
            Some(node) if node.kind == "category" => {}
            _ => return Err(UnifiedError::not_found("Category not found")),
        }
    }
    Ok(())
}

pub(crate) async fn create_software(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    body: web::Json<CreateSoftware>,
) -> HttpResponse {
    let result: UnifiedResult<SoftwareDetail> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_CREATE)?;
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Name must not be empty"));
        }
        validate_kind(&input.kind)?;
        let slug = match &input.slug {
            Some(slug) if !slug.trim().is_empty() => slugify(slug),
            _ => slugify(&input.name),
        };
        if slug.is_empty() {
            return Err(UnifiedError::bad_request(
                "Could not derive a slug from the software name",
            ));
        }
        if state
            .software
            .iter()
            .any(|entry| entry.value().slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "Software with slug \"{slug}\" already exists"
            )));
        }
        let vendor_company_id = require_company_match(&_tenant, &req, &input.vendor_company_id)?;
        validate_software_refs(
            &state,
            vendor_company_id.as_deref(),
            input.group_id.as_deref(),
            input.category_id.as_deref(),
        )?;
        for url in [
            ("Homepage URL", &input.homepage_url),
            ("Repository URL", &input.repository_url),
            ("Documentation URL", &input.documentation_url),
        ] {
            if let Some(value) = url.1 {
                validate_url(value, url.0)?;
            }
        }
        for screenshot in &input.screenshots {
            validate_url(screenshot, "Screenshot URL")?;
        }
        for tag_id in &input.tag_ids {
            if !state.tags.contains_key(tag_id.as_str()) {
                return Err(UnifiedError::not_found(format!(
                    "Tag \"{tag_id}\" not found"
                )));
            }
        }
        let software = Software {
            id: new_id(),
            slug,
            name: input.name.trim().to_string(),
            kind: input.kind,
            description: input.description.unwrap_or_default(),
            vendor_company_id,
            group_id: input.group_id,
            category_id: input.category_id,
            status: "draft".to_string(),
            homepage_url: input.homepage_url,
            repository_url: input.repository_url,
            documentation_url: input.documentation_url,
            screenshots: input.screenshots,
            created_at: now(),
            updated_at: now(),
        };
        state
            .software
            .insert(software.id.clone(), software.clone());
        if !input.tag_ids.is_empty() {
            state.software_tags.insert(
                software.id.clone(),
                input.tag_ids.iter().cloned().collect(),
            );
        }
        let detail = state.software_detail(&software);
        state.emit(
            "software.created",
            serde_json::to_value(&detail).unwrap_or(Value::Null),
        );
        Ok(detail)
    })();
    respond_created(result, ctx.is_debug())
}

pub(crate) async fn get_software(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let result: UnifiedResult<SoftwareDetail> = (|| {
        let software = require_software_in_scope(&state, path.as_str(), &scope)?;
        Ok(state.software_detail(&software))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_software(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSoftware>,
) -> HttpResponse {
    let result: UnifiedResult<SoftwareDetail> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        let input = body.into_inner();
        // Slug uniqueness is checked before taking the write lock:
        // iterating the map while holding a RefMut on one of its
        // entries would deadlock on the shard lock.
        let new_slug = input
            .slug
            .as_ref()
            .map(|slug| slugify(slug));
        if let Some(slug) = &new_slug {
            if slug.is_empty() {
                return Err(UnifiedError::bad_request("Slug must not be empty"));
            }
            if state
                .software
                .iter()
                .any(|entry| entry.value().slug == *slug && entry.key() != path.as_str())
            {
                return Err(UnifiedError::conflict(format!(
                    "Software with slug \"{slug}\" already exists"
                )));
            }
        }
        let mut software = state
            .software
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Software not found"))?;
        require_record_writable_in_company_scope(&software.vendor_company_id, &scope)?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Name must not be empty"));
            }
            software.name = name.trim().to_string();
        }
        if let Some(slug) = new_slug {
            software.slug = slug;
        }
        if let Some(kind) = input.kind {
            validate_kind(&kind)?;
            software.kind = kind;
        }
        if let Some(description) = input.description {
            software.description = description;
        }
        if let Some(vendor) = input.vendor_company_id {
            // The body may only name the caller's own company; an
            // unresolved caller may not name one. `None` in the
            // body means "no change" (this shape cannot express an
            // explicit clear), so the stored vendor is untouched.
            require_company_match_for(&scope, &Some(vendor))?;
            validate_software_refs(&state, Some(&vendor), None, None)?;
            software.vendor_company_id = Some(vendor);
        }
        if let Some(group_id) = input.group_id {
            validate_software_refs(&state, None, Some(&group_id), None)?;
            software.group_id = Some(group_id);
        }
        if let Some(category_id) = input.category_id {
            validate_software_refs(&state, None, None, Some(&category_id))?;
            software.category_id = Some(category_id);
        }
        for (field, value) in [
            ("Homepage URL", &input.homepage_url),
            ("Repository URL", &input.repository_url),
            ("Documentation URL", &input.documentation_url),
        ] {
            if let Some(url) = value {
                validate_url(url, field)?;
            }
        }
        if let Some(url) = input.homepage_url {
            software.homepage_url = Some(url);
        }
        if let Some(url) = input.repository_url {
            software.repository_url = Some(url);
        }
        if let Some(url) = input.documentation_url {
            software.documentation_url = Some(url);
        }
        if let Some(screenshots) = input.screenshots {
            for screenshot in &screenshots {
                validate_url(screenshot, "Screenshot URL")?;
            }
            software.screenshots = screenshots;
        }
        if let Some(status) = input.status {
            if !SOFTWARE_STATUSES.contains(&status.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "Status must be one of: {}",
                    SOFTWARE_STATUSES.join(", ")
                )));
            }
            validate_status_transition(&software.status, &status)?;
            software.status = status;
        }
        software.updated_at = now();
        let detail = state.software_detail(&software.clone());
        drop(software);
        state.emit(
            "software.updated",
            serde_json::to_value(&detail).unwrap_or(Value::Null),
        );
        Ok(detail)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_software(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        require_software_in_scope(&state, path.as_str(), &scope)?;
        let in_use = state
            .versions
            .iter()
            .any(|entry| entry.value().software_id.as_deref() == Some(path.as_str()));
        if in_use {
            return Err(UnifiedError::conflict(
                "Software has versions; delete the versions first",
            ));
        }
        let removed = state
            .software
            .remove(path.as_str())
            .map(|(_, software)| software);
        match removed {
            Some(software) => {
                state.software_tags.remove(path.as_str());
                state.emit(
                    "software.deleted",
                    serde_json::to_value(&software).unwrap_or(Value::Null),
                );
                Ok(json!({ "deleted": true, "id": path.as_str() }))
            }
            None => Err(UnifiedError::not_found("Software not found")),
        }
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn publish_software(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<SoftwareDetail> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        let mut software = state
            .software
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Software not found"))?;
        require_record_writable_in_company_scope(&software.vendor_company_id, &scope)?;
        validate_status_transition(&software.status, "published")?;
        software.status = "published".to_string();
        software.updated_at = now();
        let detail = state.software_detail(&software.clone());
        drop(software);
        state.emit(
            "software.published",
            serde_json::to_value(&detail).unwrap_or(Value::Null),
        );
        Ok(detail)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn unpublish_software(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<SoftwareDetail> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        let mut software = state
            .software
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Software not found"))?;
        require_record_writable_in_company_scope(&software.vendor_company_id, &scope)?;
        validate_status_transition(&software.status, "draft")?;
        software.status = "draft".to_string();
        software.updated_at = now();
        let detail = state.software_detail(&software.clone());
        drop(software);
        state.emit(
            "software.updated",
            serde_json::to_value(&detail).unwrap_or(Value::Null),
        );
        Ok(detail)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn list_software_versions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    if require_software_in_scope(&state, path.as_str(), &scope).is_err() {
        return json::error_response(&UnifiedError::not_found("Software not found"), ctx.is_debug());
    }
    let mut items: Vec<SoftwareVersion> = state
        .versions
        .iter()
        .filter(|entry| entry.value().software_id.as_deref() == Some(path.as_str()))
        .map(|entry| entry.value().clone())
        .filter(|version| {
            matches_search(
                &format!("{} {} {}", version.product, version.version, version.changelog),
                &params,
            )
        })
        .collect();
    items.sort_by(|a, b| {
        let parsed_a = Version::parse(&a.version);
        let parsed_b = Version::parse(&b.version);
        match (parsed_a, parsed_b) {
            (Ok(left), Ok(right)) => left.cmp(&right),
            _ => a.version.cmp(&b.version),
        }
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn software_release_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    if require_software_in_scope(&state, path.as_str(), &scope).is_err() {
        return json::error_response(&UnifiedError::not_found("Software not found"), ctx.is_debug());
    }
    let mut items: Vec<SoftwareReleaseHistory> = state
        .releases
        .iter()
        .filter_map(|entry| {
            let release = entry.value().clone();
            let version = state.versions.get(&release.version_id)?;
            if version.software_id.as_deref() != Some(path.as_str()) {
                return None;
            }
            let version = version.value().clone();
            if !params
                .filters
                .get("channel")
                .is_none_or(|channel| release.channel == *channel)
                || !params
                    .filters
                    .get("status")
                    .is_none_or(|status| release.status == *status)
            {
                return None;
            }
            Some(SoftwareReleaseHistory {
                release,
                version: version.version,
                changelog: version.changelog,
                license_required: version.license_required,
            })
        })
        .collect();
    items.sort_by(|a, b| {
        b.release
            .published_at
            .cmp(&a.release.published_at)
            .then_with(|| b.release.created_at.cmp(&a.release.created_at))
    });
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

/// Latest published releases.
///
/// PUBLIC endpoint: no caller/permission gate by design — only
/// `published` releases are listed, and access is gated by the API
/// Manager's `ApiAccessRule` for this path (default deny).
pub(crate) async fn latest_releases(
    state: web::Data<Arc<SoftwareState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<LatestRelease> = state
        .releases
        .iter()
        .filter(|entry| entry.value().status == "published")
        .filter_map(|entry| {
            let release = entry.value().clone();
            let version = state.versions.get(&release.version_id)?.value().clone();
            let (software_name, software_slug) = match &version.software_id {
                Some(id) => state.software.get(id).map(|entry| {
                    (Some(entry.name.clone()), Some(entry.slug.clone()))
                }).unwrap_or((None, None)),
                None => (None, None),
            };
            if !params
                .filters
                .get("channel")
                .is_none_or(|channel| release.channel == *channel)
                || !params.filters.get("software_id").is_none_or(|software_id| {
                    version.software_id.as_deref() == Some(software_id.as_str())
                })
            {
                return None;
            }
            Some(LatestRelease {
                release,
                version: version.version,
                product: version.product,
                software_id: version.software_id,
                software_name,
                software_slug,
            })
        })
        .collect();
    items.sort_by(|a, b| {
        b.release
            .published_at
            .cmp(&a.release.published_at)
            .then_with(|| b.release.created_at.cmp(&a.release.created_at))
    });
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

// ---------------------------------------------------------------------------
// Software tag assignment handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_software_tags(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let params = query.into_inner();
    if require_software_in_scope(&state, path.as_str(), &scope).is_err() {
        return json::error_response(&UnifiedError::not_found("Software not found"), ctx.is_debug());
    }
    let tag_ids = state
        .software_tags
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .unwrap_or_default();
    let mut items: Vec<Tag> = tag_ids
        .iter()
        .filter_map(|id| state.tags.get(id).map(|entry| entry.value().clone()))
        .filter(|tag| matches_search(&format!("{} {}", tag.name, tag.slug), &params))
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn attach_software_tag(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AttachTag>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_CREATE)?;
        require_software_in_scope(&state, path.as_str(), &scope)?;
        if !state.tags.contains_key(body.tag_id.as_str()) {
            return Err(UnifiedError::not_found("Tag not found"));
        }
        let mut tags = state
            .software_tags
            .entry(path.as_str().to_string())
            .or_default();
        if !tags.insert(body.tag_id.clone()) {
            return Err(UnifiedError::conflict("Tag is already attached to this software"));
        }
        state.emit(
            "software.tag.attached",
            json!({ "software_id": path.as_str(), "tag_id": body.tag_id }),
        );
        Ok(json!({ "software_id": path.as_str(), "tag_id": body.tag_id }))
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, ctx.is_debug()),
    }
}

pub(crate) async fn detach_software_tag(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (software_id, tag_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SOFTWARE_MANAGE)?;
        require_software_in_scope(&state, software_id.as_str(), &scope)?;
        let mut tags = state
            .software_tags
            .get_mut(software_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Tag is not attached to this software"))?;
        if !tags.remove(tag_id.as_str()) {
            return Err(UnifiedError::not_found("Tag is not attached to this software"));
        }
        state.emit(
            "software.tag.detached",
            json!({ "software_id": software_id, "tag_id": tag_id }),
        );
        Ok(json!({ "detached": true, "software_id": software_id, "tag_id": tag_id }))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn get_stats(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SoftwareState>>,
    req: HttpRequest,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SOFTWARE_READ) {
        return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
    }
    let in_scope_software = |vendor: &Option<String>| in_company_scope(vendor, &scope);
    let in_scope_version =
        |version: &SoftwareVersion| in_scope_software(&version_company(&state, version));
    let in_scope_release =
        |release: &Release| in_scope_software(&release_company(&state, release));
    let total_downloads: u64 = state
        .releases
        .iter()
        .filter(|entry| in_scope_release(entry.value()))
        .map(|entry| entry.value().download_count)
        .sum();
    let downloads_by_release: Vec<Value> = state
        .releases
        .iter()
        .filter(|entry| in_scope_release(entry.value()))
        .map(|entry| {
            json!({
                "release_id": entry.value().id,
                "version_id": entry.value().version_id,
                "download_count": entry.value().download_count,
            })
        })
        .collect();
    let mut by_status: HashMap<String, u64> = HashMap::new();
    for entry in state
        .software
        .iter()
        .filter(|entry| in_scope_software(&entry.value().vendor_company_id))
    {
        *by_status.entry(entry.value().status.clone()).or_default() += 1;
    }
    let mut by_kind: HashMap<String, u64> = HashMap::new();
    for entry in state
        .software
        .iter()
        .filter(|entry| in_scope_software(&entry.value().vendor_company_id))
    {
        *by_kind.entry(entry.value().kind.clone()).or_default() += 1;
    }
    json::success_response(
        json!({
            "total_software": state
                .software
                .iter()
                .filter(|entry| in_scope_software(&entry.value().vendor_company_id))
                .count(),
            "software_by_status": by_status,
            "software_by_kind": by_kind,
            "total_versions": state
                .versions
                .iter()
                .filter(|entry| in_scope_version(entry.value()))
                .count(),
            "total_releases": state
                .releases
                .iter()
                .filter(|entry| in_scope_release(entry.value()))
                .count(),
            "total_release_files": state
                .release_files
                .iter()
                .filter(|entry| in_scope_software(&file_company(&state, entry.value())))
                .count(),
            "total_groups": state.taxonomy.iter().filter(|entry| entry.value().kind == "group").count(),
            "total_categories": state.taxonomy.iter().filter(|entry| entry.value().kind == "category").count(),
            "total_tags": state.tags.len(),
            "total_downloads": total_downloads,
            "downloads_by_release": downloads_by_release,
        }),
        200,
    )
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::context::AppContext;
    use unified_core::hooks::{HookBus, HookContext};
    use unified_core::modules::ModuleRegistry;
    use actix_web::test::TestRequest;
    use std::sync::Arc;

    /// App context whose `role.check` hook allows exactly the given
    /// permissions (all other permissions are denied).
    fn test_ctx(allowed: &[&str]) -> web::Data<Arc<AppContext>> {
        let hooks = Arc::new(HookBus::new());
        let allowed: Vec<String> = allowed.iter().map(|permission| permission.to_string()).collect();
        hooks.register(
            "role.check",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                let permission = ctx
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.set(
                    "allowed",
                    serde_json::json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        let ctx = AppContext::new(
            AppConfig::default(),
            hooks,
            Arc::new(ModuleRegistry::new()),
        );
        web::Data::new(Arc::new(ctx))
    }

    fn authed_request() -> HttpRequest {
        verified_request("tester", "admin")
    }

    #[tokio::test]
    async fn list_software_rejects_a_request_without_a_verified_caller() {
        let ctx = test_ctx(&["software:read"]);
        let state = web::Data::new(Arc::new(SoftwareState::new()));
        let req = TestRequest::default().to_http_request();
        let response = list_software(
            ctx,
            state,
            req,
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status().as_u16(), 401);
    }

    #[tokio::test]
    async fn list_software_allows_a_caller_with_the_read_permission() {
        let ctx = test_ctx(&["software:read"]);
        let state = web::Data::new(Arc::new(SoftwareState::new()));
        let response = list_software(
            ctx,
            state,
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status().as_u16(), 200);
    }

    #[tokio::test]
    async fn list_software_forbids_a_caller_without_the_read_permission() {
        let ctx = test_ctx(&["software:manage"]);
        let state = web::Data::new(Arc::new(SoftwareState::new()));
        let response = list_software(
            ctx,
            state,
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status().as_u16(), 403);
    }
}
