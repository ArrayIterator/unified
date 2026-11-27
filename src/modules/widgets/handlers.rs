// Widgets - HTTP handlers: list types, get / save / reset layouts.
//
// Auth follows the documents / saved_views shape: the caller is the
// server-verified JWT identity and every route asks the roles module
// through the `role.check` hook (fail closed). Reads need
// `widgets.view`; layout changes need `widgets.manage`.
//
// Scoping: the tenant dashboard is tenant-scoped (the middleware
// tenant context + the verified caller's company, never a header);
// every other context is platform scope.

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::json;

use super::helpers::{caller, require_caller_company, require_permission, require_tenant, Caller};
use super::registry::collect_widget_types;
use super::service::{
    load_layout as load_layout_service, reset_layout as reset_layout_service,
    save_layout as save_layout_service,
};
use super::types::*;
use super::validation::validate_zone;

/// Authorize one request: caller identity + one capability. Returns
/// the caller on success so handlers can use the actor id.
fn authorize(ctx: &AppContext, req: &HttpRequest, permission: &str) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    require_permission(ctx, &found.role, permission)?;
    Ok(found)
}

/// Resolve the layout scope for a context: the tenant dashboard is
/// tenant-scoped (fail closed without a resolved tenant + company);
/// every other context is platform scope.
fn resolve_scope(
    req: &HttpRequest,
    context: &str,
) -> UnifiedResult<(String, String)> {
    if context == CONTEXT_TENANT_DASHBOARD {
        let tenant = require_tenant(req)?;
        let company = require_caller_company(&tenant, req)?;
        Ok((SCOPE_TENANT.to_string(), company))
    } else {
        Ok((SCOPE_PLATFORM.to_string(), SCOPE_ID_PLATFORM.to_string()))
    }
}

/// `GET /api/v1/widgets/types[?context=...]`: the widget catalogue
/// (built-ins plus hook-registered types), optionally filtered to
/// one zone. Requires `widgets.view`.
pub(crate) async fn list_widget_types(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<TypesQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<WidgetDefinition>> = (|| {
        authorize(&ctx, &req, CAP_WIDGETS_VIEW)?;
        let catalogue = collect_widget_types(&ctx);
        match query.context.as_deref() {
            None => Ok(catalogue),
            Some(context) => {
                let context = validate_zone(context)?;
                Ok(catalogue
                    .into_iter()
                    .filter(|definition| definition.for_context(context))
                    .collect())
            }
        }
    })();
    match result {
        Ok(definitions) => json::success_response(definitions, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `GET /api/v1/widgets/layouts/{context}`: the caller's layout for
/// a zone - the user's own row, then the shared default row for the
/// zone, then the built-in default. Requires `widgets.view`.
pub(crate) async fn get_layout(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let found = authorize(&ctx, &req, CAP_WIDGETS_VIEW)?;
        let context = validate_zone(&path.into_inner())?.to_string();
        let (scope_kind, scope_id) = resolve_scope(&req, &context)?;
        load_layout_service(&ctx, &context, &scope_kind, &scope_id, &found.actor).await
    })();
    match result.await {
        Ok(layout) => json::success_response(layout, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `PUT /api/v1/widgets/layouts/{context}`: save a layout (pin /
/// add / remove / reorder - the body array order IS the layout
/// order). By default the caller's OWN layout is saved; with
/// `"shared": true` the shared (team) default for the zone is saved
/// instead. Requires `widgets.manage`.
pub(crate) async fn save_layout(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SaveLayoutInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let found = authorize(&ctx, &req, CAP_WIDGETS_MANAGE)?;
        let context = validate_zone(&path.into_inner())?.to_string();
        let (scope_kind, scope_id) = resolve_scope(&req, &context)?;
        let input = body.into_inner();
        let owner_id = if input.shared {
            OWNER_DEFAULT.to_string()
        } else {
            found.actor
        };
        save_layout_service(&ctx, &context, &scope_kind, &scope_id, &owner_id, input.items).await
    })();
    match result.await {
        Ok(layout) => json::success_response(layout, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `DELETE /api/v1/widgets/layouts/{context}`: reset a layout to the
/// shared default (or the built-in default when no shared row
/// exists) by deleting the caller's own row; with `?shared=true`
/// the shared default row itself is deleted instead. Requires
/// `widgets.manage`.
pub(crate) async fn reset_layout_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ResetQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let found = authorize(&ctx, &req, CAP_WIDGETS_MANAGE)?;
        let context = validate_zone(&path.into_inner())?.to_string();
        let (scope_kind, scope_id) = resolve_scope(&req, &context)?;
        let owner_id = if query.shared.unwrap_or(false) {
            OWNER_DEFAULT.to_string()
        } else {
            found.actor
        };
        reset_layout_service(&ctx, &context, &scope_kind, &scope_id, &owner_id).await
    })();
    match result.await {
        Ok(layout) => json::success_response(layout, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// `?context=` filter for the types list.
#[derive(Debug, Deserialize)]
pub(crate) struct TypesQuery {
    pub context: Option<String>,
}

/// `?shared=true` for the reset: delete the shared default row
/// instead of the caller's own row.
#[derive(Debug, Deserialize)]
pub(crate) struct ResetQuery {
    pub shared: Option<bool>,
}
