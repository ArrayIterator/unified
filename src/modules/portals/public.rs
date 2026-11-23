//! Portals - public portal and client-data handlers.
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
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;

use super::constants::PERM_PORTALS_VIEW;
use super::helpers::Caller;
use super::helpers::caller;
use super::helpers::err;
use super::helpers::load_portal_by_slug;
use super::helpers::ok;
use super::helpers::paginate;
use super::helpers::public_portal_view;
use super::helpers::record_view;
use super::helpers::require_caller_company;
use super::helpers::require_permission;
use super::helpers::require_tenant;
use super::helpers::section_kind;
use super::state::PortalsState;
use super::types::Portal;
use super::types::PortalRecord;
use super::types::PortalStatus;

// ---------------------------------------------------------------------------
// Public portal
// ---------------------------------------------------------------------------

pub(crate) async fn get_public_portal(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    // No capability check here on purpose: this is the public
    // portal render. The API Manager gate (ApiAccessRule with
    // public_access for this path, default deny) is what makes it
    // public — see the module header.
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let portal = load_portal_by_slug(&state, &path.into_inner())?;
        if portal.status != PortalStatus::Active {
            return Err(UnifiedError::not_found(format!(
                "Portal not found: {}",
                portal.slug
            )));
        }
        // The X-Role header is deliberately ignored here: this
        // endpoint is unauthenticated, so anyone could send
        // `X-Role: admin` and read role-gated menu labels and
        // targets. The public render therefore shows only menus
        // with an empty role list; role-filtered menus are shown
        // only on the authenticated client home route, where the
        // caller's role comes from the auth layer.
        let view = public_portal_view(&state, &portal, None);
        record_view(&state, &portal, &req);
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Client data (scoped server-side)
// ---------------------------------------------------------------------------

/// Load the portal for a client call and enforce scoping: the
/// portal must be active, must belong to the caller's company, and
/// the requested section must be visible on the portal. Returns
/// the portal, the caller, and the company id.
fn client_portal(
    state: &PortalsState,
    req: &HttpRequest,
    slug: &str,
    section: &str,
) -> UnifiedResult<(Portal, Caller, String)> {
    let tenant = require_tenant(req)?;
    let caller = caller(req)?;
    let company_id = require_caller_company(&tenant, req)?;
    let portal = load_portal_by_slug(state, slug)?;
    if portal.status != PortalStatus::Active {
        return Err(UnifiedError::not_found(format!(
            "Portal not found: {}",
            portal.slug
        )));
    }
    if portal.company_id != company_id {
        return Err(UnifiedError::forbidden(
            "This portal does not belong to the caller's company",
        ));
    }
    if !portal.sections.iter().any(|entry| entry == section) {
        return Err(UnifiedError::forbidden(format!(
            "Section \"{section}\" is not enabled on this portal"
        )));
    }
    Ok((portal, caller, company_id))
}

/// Shared list logic for one `my/*` kind: capability check, portal
/// scoping, then only the caller's own records of the caller's
/// company, newest first, paginated, filterable by status.
fn client_records(
    ctx: &AppContext,
    state: &PortalsState,
    req: &HttpRequest,
    slug: &str,
    kind: &str,
    params: &PaginationParams,
) -> UnifiedResult<Value> {
    let (portal, caller, company_id) = client_portal(state, req, slug, kind)?;
    require_permission(ctx, &caller.role, PERM_PORTALS_VIEW)?;
    let record_kind = section_kind(kind);
    let status_filter = params.filters.get("status").cloned();
    let mut items: Vec<PortalRecord> = state
        .records
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            record.kind == record_kind
                && record.company_id == company_id
                && record.user_id == caller.actor
                && match &status_filter {
                    Some(status) => &record.status == status,
                    None => true,
                }
        })
        .collect();
    items.sort_by_key(|record| std::cmp::Reverse(record.created_at));
    let page = paginate(items, params).to_json();
    Ok(json!({
        "portal_id": portal.id.to_string(),
        "slug": portal.slug,
        "kind": kind,
        "records": page,
    }))
}

pub(crate) async fn client_portal_home(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let slug = path.into_inner();
        let caller = caller(&req)?;
        let company_id = require_caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_VIEW)?;
        let portal = load_portal_by_slug(&state, &slug)?;
        if portal.status != PortalStatus::Active {
            return Err(UnifiedError::not_found(format!(
                "Portal not found: {}",
                portal.slug
            )));
        }
        if portal.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "This portal does not belong to the caller's company",
            ));
        }
        let mut counts: Vec<Value> = Vec::new();
        for section in &portal.sections {
            let record_kind = section_kind(section);
            let count = state
                .records
                .iter()
                .filter(|entry| {
                    entry.value().kind == record_kind
                        && entry.value().company_id == company_id
                        && entry.value().user_id == caller.actor
                })
                .count();
            counts.push(json!({ "section": section, "count": count }));
        }
        let mut view = public_portal_view(&state, &portal, Some(&caller.role));
        if let Value::Object(map) = &mut view {
            map.insert("record_counts".to_string(), Value::Array(counts));
        }
        record_view(&state, &portal, &req);
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn client_invoices(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_tenant(&req) {
        return err(error, debug);
    }
    let params = query.into_inner();
    let result = client_records(&ctx, &state, &req, &path.into_inner(), "invoices", &params);
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn client_tickets(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_tenant(&req) {
        return err(error, debug);
    }
    let params = query.into_inner();
    let result = client_records(&ctx, &state, &req, &path.into_inner(), "tickets", &params);
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn client_downloads(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_tenant(&req) {
        return err(error, debug);
    }
    let params = query.into_inner();
    let result = client_records(&ctx, &state, &req, &path.into_inner(), "downloads", &params);
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn client_subscriptions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_tenant(&req) {
        return err(error, debug);
    }
    let params = query.into_inner();
    let result = client_records(
        &ctx,
        &state,
        &req,
        &path.into_inner(),
        "subscriptions",
        &params,
    );
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn client_licenses(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_tenant(&req) {
        return err(error, debug);
    }
    let params = query.into_inner();
    let result = client_records(&ctx, &state, &req, &path.into_inner(), "licenses", &params);
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
