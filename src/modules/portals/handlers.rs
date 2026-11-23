//! Portals - portal handlers.
//!
//! Part of the `portals` module split; see `src/modules/portals.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use actix_web::web;
use chrono::Utc;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use super::constants::MAX_COMPANY_LEN;
use super::constants::MAX_NAME_LEN;
use super::constants::MAX_STORAGE_KEY_LEN;
use super::constants::PERM_PORTALS_MANAGE;
use super::constants::PERM_PORTALS_VIEW;
use super::helpers::apply_custom_domain;
use super::helpers::apply_domain_verification;
use super::helpers::caller;
use super::helpers::caller_company_scope;
use super::helpers::created;
use super::helpers::default_color;
use super::helpers::default_sections;
use super::helpers::domain_in_use;
use super::helpers::err;
use super::helpers::load_portal;
use super::helpers::ok;
use super::helpers::paginate;
use super::helpers::in_company_scope_str;
use super::helpers::require_company_match_str;
use super::helpers::require_hex_color;
use super::helpers::require_permission;
use super::helpers::require_record_in_company_scope_str;
use super::helpers::require_record_writable_in_company_scope_str;
use super::helpers::require_text;
use super::helpers::require_tenant;
use super::helpers::slug_in_use;
use super::helpers::slugify;
use super::helpers::valid_domain;
use super::helpers::valid_slug;
use super::helpers::validate_sections;
use super::state::PortalsState;
use super::types::CreatePortalRequest;
use super::types::Portal;
use super::types::PortalView;
use super::types::UpdateDomainStatusRequest;
use super::types::UpdatePortalRequest;

// ---------------------------------------------------------------------------
// Portal handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_portals(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_VIEW)?;
        let scope = caller_company_scope(&_tenant, &req)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Portal> = state
            .portals
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|portal| {
                // Portals always carry a company: a scoped caller
                // sees only their own company's portals, an
                // unresolved caller sees none.
                if !in_company_scope_str(&portal.company_id, &scope) {
                    return false;
                }
                if let Some(company) = &company_filter {
                    if &portal.company_id != company {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if portal.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {} {}", portal.name, portal.slug, portal.company_id)
                        .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "slug", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.cmp(&b.name),
                "slug" => a.slug.cmp(&b.slug),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_portal(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    body: web::Json<CreatePortalRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Portal> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_MANAGE)?;
        let scope = caller_company_scope(&_tenant, &req)?;
        let input = body.into_inner();
        let company_id = require_text(&input.company_id, "company_id", MAX_COMPANY_LEN)?;
        // The portal always carries a company: the body company
        // must equal the caller scope (an unresolved caller may
        // not create a portal for any company).
        let company_id = require_company_match_str(&_tenant, &req, &company_id)?;
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let slug = match &input.slug {
            Some(raw) => {
                let slug = raw.trim().to_lowercase();
                if !valid_slug(&slug) {
                    return Err(UnifiedError::bad_request(
                        "slug must be lowercase letters, digits, and single hyphens only (no leading/trailing hyphen, max 128 characters)",
                    ));
                }
                slug
            }
            None => slugify(&name),
        };
        if slug_in_use(&state, &slug, None) {
            return Err(UnifiedError::conflict(format!(
                "slug is already in use: {slug}"
            )));
        }
        let custom_domain = match &input.custom_domain {
            Some(raw) => {
                let domain = raw.trim().to_lowercase();
                if !valid_domain(&domain) {
                    return Err(UnifiedError::bad_request(format!(
                        "Invalid custom domain: {domain}"
                    )));
                }
                if domain_in_use(&state, &domain, None) {
                    return Err(UnifiedError::conflict(format!(
                        "custom domain is already in use: {domain}"
                    )));
                }
                Some(domain)
            }
            None => None,
        };
        let logo_storage_target = match &input.logo_storage_target {
            Some(raw) => Some(require_text(
                raw,
                "logo_storage_target",
                MAX_STORAGE_KEY_LEN,
            )?),
            None => None,
        };
        let logo_storage_key = match &input.logo_storage_key {
            Some(raw) => Some(require_text(raw, "logo_storage_key", MAX_STORAGE_KEY_LEN)?),
            None => None,
        };
        if logo_storage_key.is_some() != logo_storage_target.is_some() {
            return Err(UnifiedError::bad_request(
                "logo_storage_target and logo_storage_key must be given together (storage module reference)",
            ));
        }
        let primary_color = match &input.primary_color {
            Some(raw) => require_hex_color(raw, "primary_color")?,
            None => default_color(true),
        };
        let secondary_color = match &input.secondary_color {
            Some(raw) => require_hex_color(raw, "secondary_color")?,
            None => default_color(false),
        };
        let accent_color = match &input.accent_color {
            Some(raw) => require_hex_color(raw, "accent_color")?,
            None => "#C9A227".to_string(),
        };
        let background_color = match &input.background_color {
            Some(raw) => require_hex_color(raw, "background_color")?,
            None => "#FFFFFF".to_string(),
        };
        let text_color = match &input.text_color {
            Some(raw) => require_hex_color(raw, "text_color")?,
            None => "#1A1A1A".to_string(),
        };
        let sections = if input.sections.is_empty() {
            default_sections()
        } else {
            validate_sections(&input.sections)?;
            input.sections.clone()
        };
        let now = Utc::now();
        let portal = Portal {
            id: Uuid::now_v7(),
            company_id: company_id.clone(),
            name,
            slug: slug.clone(),
            custom_domain: custom_domain.clone(),
            domain_status: if custom_domain.is_some() {
                "requested".to_string()
            } else {
                "none".to_string()
            },
            logo_storage_target,
            logo_storage_key,
            primary_color,
            secondary_color,
            accent_color,
            background_color,
            text_color,
            sections,
            status: input.status.unwrap_or_default(),
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.portals.insert(portal.id, portal.clone());
        if let Some(domain) = &custom_domain {
            state.emit(
                "portals.domain.requested",
                json!({
                    "portal_id": portal.id.to_string(),
                    "slug": portal.slug,
                    "company_id": company_id,
                    "domain": domain,
                }),
            );
        }
        state.emit(
            "portals.created",
            json!({
                "portal_id": portal.id.to_string(),
                "slug": portal.slug,
                "company_id": portal.company_id,
                "status": portal.status.as_str(),
            }),
        );
        Ok(portal)
    })();
    match result {
        Ok(portal) => created(portal),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_portal(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Portal> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_VIEW)?;
        let scope = caller_company_scope(&_tenant, &req)?;
        let portal = load_portal(&state, &path.into_inner())?;
        require_record_in_company_scope_str(&portal.company_id, &scope)?;
        Ok(portal)
    })();
    match result {
        Ok(portal) => ok(portal),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_portal(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePortalRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Portal> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_MANAGE)?;
        let scope = caller_company_scope(&_tenant, &req)?;
        let mut portal = load_portal(&state, &path.into_inner())?;
        require_record_writable_in_company_scope_str(&portal.company_id, &scope)?;
        let input = body.into_inner();
        let mut domain_changed = false;
        if let Some(name) = &input.name {
            portal.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(slug) = &input.slug {
            let slug = slug.trim().to_lowercase();
            if !valid_slug(&slug) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and single hyphens only",
                ));
            }
            if slug_in_use(&state, &slug, Some(portal.id)) {
                return Err(UnifiedError::conflict(format!(
                    "slug is already in use: {slug}"
                )));
            }
            portal.slug = slug;
        }
        if let Some(domain) = &input.custom_domain {
            match domain {
                Some(raw) => {
                    let domain = raw.trim().to_lowercase();
                    if !valid_domain(&domain) {
                        return Err(UnifiedError::bad_request(format!(
                            "Invalid custom domain: {domain}"
                        )));
                    }
                    if domain_in_use(&state, &domain, Some(portal.id)) {
                        return Err(UnifiedError::conflict(format!(
                            "custom domain is already in use: {domain}"
                        )));
                    }
                    // Only a real change resets domain_status to
                    // "requested" (and re-emits the verification
                    // request below); re-saving the same domain
                    // keeps a "verified" status untouched.
                    domain_changed = apply_custom_domain(&mut portal, Some(domain));
                }
                None => {
                    domain_changed = apply_custom_domain(&mut portal, None);
                }
            }
        }
        if let Some(target) = &input.logo_storage_target {
            portal.logo_storage_target = match target {
                Some(raw) => Some(require_text(
                    raw,
                    "logo_storage_target",
                    MAX_STORAGE_KEY_LEN,
                )?),
                None => None,
            };
        }
        if let Some(key) = &input.logo_storage_key {
            portal.logo_storage_key = match key {
                Some(raw) => Some(require_text(raw, "logo_storage_key", MAX_STORAGE_KEY_LEN)?),
                None => None,
            };
        }
        if portal.logo_storage_key.is_some() != portal.logo_storage_target.is_some() {
            return Err(UnifiedError::bad_request(
                "logo_storage_target and logo_storage_key must be given together",
            ));
        }
        if let Some(color) = &input.primary_color {
            portal.primary_color = require_hex_color(color, "primary_color")?;
        }
        if let Some(color) = &input.secondary_color {
            portal.secondary_color = require_hex_color(color, "secondary_color")?;
        }
        if let Some(color) = &input.accent_color {
            portal.accent_color = require_hex_color(color, "accent_color")?;
        }
        if let Some(color) = &input.background_color {
            portal.background_color = require_hex_color(color, "background_color")?;
        }
        if let Some(color) = &input.text_color {
            portal.text_color = require_hex_color(color, "text_color")?;
        }
        if let Some(sections) = &input.sections {
            validate_sections(sections)?;
            portal.sections = sections.clone();
        }
        if let Some(status) = input.status {
            portal.status = status;
        }
        portal.updated_at = Utc::now();
        state.portals.insert(portal.id, portal.clone());
        if domain_changed {
            if let Some(domain) = &portal.custom_domain {
                state.emit(
                    "portals.domain.requested",
                    json!({
                        "portal_id": portal.id.to_string(),
                        "slug": portal.slug,
                        "company_id": portal.company_id,
                        "domain": domain,
                    }),
                );
            }
        }
        state.emit(
            "portals.updated",
            json!({
                "portal_id": portal.id.to_string(),
                "slug": portal.slug,
                "status": portal.status.as_str(),
            }),
        );
        Ok(portal)
    })();
    match result {
        Ok(portal) => ok(portal),
        Err(error) => err(error, debug),
    }
}

/// Write back a custom-domain verification outcome: `verified` or
/// `rejected`, for a portal whose `domain_status` is `requested`.
///
/// This is the write-back path for the domains module's
/// verification result. The domains module has no listener on
/// `portals.domain.requested`, so without this endpoint a custom
/// domain would stay `requested` forever; the verifier (or an
/// operator acting on its result) calls this endpoint instead, and
/// the outcome is announced as `portals.domain.verified` or
/// `portals.domain.rejected`. The call requires the
/// `portals.manage` capability, so an unauthenticated caller cannot
/// flip a domain to `verified`.
pub(crate) async fn update_domain_status(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDomainStatusRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Portal> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_MANAGE)?;
        let scope = caller_company_scope(&_tenant, &req)?;
        let mut portal = load_portal(&state, &path.into_inner())?;
        require_record_writable_in_company_scope_str(&portal.company_id, &scope)?;
        let input = body.into_inner();
        apply_domain_verification(&mut portal, &input.status, input.domain.as_deref())?;
        let status = portal.domain_status.clone();
        state.portals.insert(portal.id, portal.clone());
        state.emit(
            if status == "verified" {
                "portals.domain.verified"
            } else {
                "portals.domain.rejected"
            },
            json!({
                "portal_id": portal.id.to_string(),
                "slug": portal.slug,
                "company_id": portal.company_id,
                "domain": portal.custom_domain,
                "domain_status": status,
                "reason": input.reason,
            }),
        );
        Ok(portal)
    })();
    match result {
        Ok(portal) => ok(portal),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_portal(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_MANAGE)?;
        let scope = caller_company_scope(&_tenant, &req)?;
        let portal = load_portal(&state, &path.into_inner())?;
        require_record_writable_in_company_scope_str(&portal.company_id, &scope)?;
        let id = portal.id;
        state.portals.remove(&id);
        let stale_menus: Vec<Uuid> = state
            .menus
            .iter()
            .filter(|entry| entry.value().portal_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in stale_menus {
            state.menus.remove(&key);
        }
        let stale_views: Vec<Uuid> = state
            .views
            .iter()
            .filter(|entry| entry.value().portal_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in stale_views {
            state.views.remove(&key);
        }
        state.emit(
            "portals.deleted",
            json!({ "portal_id": id.to_string(), "slug": portal.slug }),
        );
        Ok(json!({ "deleted": true, "portal_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn portal_analytics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_VIEW)?;
        let scope = caller_company_scope(&_tenant, &req)?;
        let portal = load_portal(&state, &path.into_inner())?;
        require_record_in_company_scope_str(&portal.company_id, &scope)?;
        let params = query.into_inner();
        let mut views: Vec<PortalView> = state
            .views
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|view| view.portal_id == portal.id)
            .collect();
        views.sort_by_key(|view| std::cmp::Reverse(view.viewed_at));
        let total_views = views.len() as u64;
        let mut unique_viewers: Vec<String> = views
            .iter()
            .filter_map(|view| view.viewer_user_id.clone())
            .collect();
        unique_viewers.sort();
        unique_viewers.dedup();
        let page = paginate(views, &params).to_json();
        Ok(json!({
            "portal_id": portal.id.to_string(),
            "slug": portal.slug,
            "total_views": total_views,
            "unique_viewers": unique_viewers.len() as u64,
            "views": page,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
