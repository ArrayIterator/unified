// cloud module - hook-collected catalogue endpoints.
//
// Thin read endpoints over the collectors in `super::hooks`: the
// "cloud panel" connector definitions and the SSL provider
// definitions contributed through the hook contracts. The in-tree
// `panels` and `certificates` modules answer those hooks today;
// external "cloud panel" modules answer the same way when adopted.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::json;
use actix_web::{web, HttpRequest, HttpResponse};
use std::sync::Arc;

use super::helpers::{require_cloud_entitlement, PERM_CLOUD_VIEW};
use super::hooks::{
    collect_cloud_panel_definitions, collect_cloud_ssl_provider_definitions,
    CloudPanelDefinition, CloudSslProviderDefinition,
};
use super::providers::handlers::{caller, require_permission, require_tenant};

pub(crate) async fn list_cloud_panels(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<CloudPanelDefinition>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CLOUD_VIEW)?;
        require_cloud_entitlement(&ctx, &req)?;
        Ok(collect_cloud_panel_definitions(&ctx))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_cloud_ssl_providers(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<CloudSslProviderDefinition>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CLOUD_VIEW)?;
        require_cloud_entitlement(&ctx, &req)?;
        Ok(collect_cloud_ssl_provider_definitions(&ctx))
    })();
    json::respond(result, debug)
}
