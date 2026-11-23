// cloud module — shared helpers: the package entitlement gate and
// the cloud permission names.
//
// The gate mirrors `site_builder::helpers::require_site_builder_entitlement`:
// permission first, then entitlement (see `site_builder::helpers::authorize`
// for the same order). The control plane (no company scope) is never
// package-gated; a tenant without the cloud package gets a 403.

use actix_web::HttpRequest;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};

/// Module id gated by the cloud package.
pub const CLOUD_MODULE_ID: &str = "cloud";

/// Capability required to read cloud resources (plans, accounts,
/// providers, apps, virtualization).
pub const PERM_CLOUD_VIEW: &str = "cloud.view";

/// Capability required to create, change, or delete cloud resources.
pub const PERM_CLOUD_MANAGE: &str = "cloud.manage";

/// Require the cloud package for a tenant-scoped request.
///
/// The tenant/company scope is resolved from the request itself: a
/// mismatch between the tenant context company and the verified caller
/// company is forbidden; no company scope (the control plane) is never
/// gated; otherwise the licensing verdict for module id `cloud`
/// decides (tenant without the package is not entitled).
pub fn require_cloud_entitlement(ctx: &AppContext, req: &HttpRequest) -> UnifiedResult<()> {
    let tenant = unified_core::tenancy::TenantContext::from_http_request(req).ok();
    let tenant_company = tenant.as_ref().and_then(|item| item.company_id.clone());
    let verified_company = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    let company = match (&tenant_company, &verified_company) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            return Err(UnifiedError::forbidden(
                "Tenant context company and verified caller company disagree",
            ));
        }
        (Some(tenant_company), _) => Some(tenant_company.clone()),
        (None, verified_company) => verified_company.clone(),
    };
    let Some(company) = company else {
        // No company scope: the control plane, never package-gated.
        return Ok(());
    };
    crate::modules::licensing::check_module_entitlement(ctx, &company, CLOUD_MODULE_ID)
}
