//! Saas Packages - Caller resolution and capability checks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::middleware::TenantContext;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::tenancy as tenancy;
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai::{register_ai_capabilities, run_ai_capability, AiCapability};
use crate::modules::licensing::check_module_entitlement;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{constants::*, types::*, state::*, seed::*, helpers::*, provisioning::*, handlers_packages::*, handlers_purchases::*, handlers_ai::*, events::*, module::*};

// Viewer identity and capability checks (same trusted-parameter
// pattern as the e-learning module: X-User-Id plus X-Role headers,
// `role.check` hook, fail closed).

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the e-learning module.
pub struct Caller {
    pub actor: String,
    pub role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Tenant scoping (standing rule). When the tenant-resolution
/// middleware attached a `TenantContext` to the request, the
/// requested company must match the resolved tenant -- a mismatch
/// is a 403, fail closed. The resolved context is validated
/// through the tenancy contract (`tenancy::TenantContext
/// ::from_resolved`): a tenant id that is not a uuidv7 UUID is
/// rejected outright, and the tenant schema name is always derived
/// through `tenancy::schema_name`, never built inline. With no
/// context resolved (a control-plane request, for example a buyer
/// on the main host or an admin), the explicit company scoping in
/// each handler governs instead. That is not a silent fallback to
/// the public schema: this module executes no SQL at all, never
/// sets a search_path, and never reads a record without its
/// company id. The database-level enforcement (search_path via
/// `with_tenant_connection`) has nothing to wrap here because
/// there is no SQL in this file; it applies when the migration
/// persistence target is wired to Postgres (see the module header
/// OPEN notes).
pub(crate) fn require_tenant_scope(req: &HttpRequest, company_id: &Uuid) -> UnifiedResult<()> {
    let Some(resolved) = TenantContext::from_request(req) else {
        return Ok(());
    };
    let tenant = tenancy::TenantContext::from_resolved(&resolved)?;
    if tenant.tenant_id != *company_id {
        return Err(UnifiedError::forbidden(
            "Request tenant context does not match the requested company: tenant-scoped data is never served across tenants",
        ));
    }
    if let Some(company) = &tenant.company_id {
        if company != &company_id.to_string() {
            return Err(UnifiedError::forbidden(
                "Request tenant context does not match the requested company: tenant-scoped data is never served across tenants",
            ));
        }
    }
    Ok(())
}

/// Control-plane guard (standing rule, mirroring
/// `tenancy::ControlPlaneGuard`): package administration and
/// module settings live in the `public` schema, so a request that
/// resolved to a tenant host is rejected outright (403). The
/// storefront reads (listing / getting published packages) are
/// the exception: tenants must be able to browse the catalog.
pub(crate) fn require_control_plane(req: &HttpRequest) -> UnifiedResult<()> {
    if TenantContext::from_request(req).is_some() {
        return Err(UnifiedError::forbidden(
            "Tenant requests cannot access the control plane",
        )
        .with_metadata("kind", serde_json::json!("tenant_control_plane_blocked")));
    }
    Ok(())
}

/// The tenant schema name for a company id, built only through
/// the tenancy helper -- never inline, never hard-coded.
pub(crate) fn tenant_schema(company_id: &Uuid) -> String {
    tenancy::schema_name(*company_id)
}
