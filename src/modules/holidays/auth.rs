//! Caller identity, capability checks and small HTTP helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, ProductCategory, RouteInfo, RouteVersionSupport};
use unified_core::tenancy::{schema_name_for_str, TenantContext as TenancyContext};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use reqwest::header::RETRY_AFTER;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::*;

// Helpers: tenant, caller, capabilities, envelope.

/// Resolve the tenant context for a request. THE single convergence
/// point for batch-17 schema-per-tenant plumbing (see the module
/// header): this function returns the tenancy `TenantContext`
/// (tenant id as Uuid + schema name derived ONLY by
/// `tenancy::schema_name()`), and the Diesel persistence wiring -
/// when it lands - takes the Uuid from here into
/// `tenancy::with_tenant_connection`. Fail closed:
/// `from_http_request` answers 401 when the request did not resolve
/// to a tenant and 403 when the resolved tenant id is not a valid
/// UUID; no resolved tenant means the request is refused here,
/// never served from `public`.
pub(crate) fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<TenancyContext> {
    TenancyContext::from_http_request(req)
}

/// The server-verified caller (see `unified_core::caller::Caller`):
/// the user id identifies the actor, the server-resolved role is the
/// role slug or id the capability check runs against.
/// from the resolved tenant context, never from a client header.
/// `schema_name` is carried (not just derived on demand) so the
/// future Diesel persistence layer takes it from exactly one place.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
    pub(crate) tenant_id: String,
    /// Derived ONLY by `tenancy::schema_name()` (see
    /// `resolve_tenant`). Unread until the Diesel persistence layer
    /// lands and takes it into `tenancy::with_tenant_connection`.
    #[allow(dead_code)]
    schema_name: String,
    pub(crate) company_id: String,
}

/// Tenant-scoped caller: resolves the tenant first, then the actor
/// headers. Used by every tenant-owned endpoint (configs, runs,
/// holidays, overrides).
pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let tenant = resolve_tenant(req)?;
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller {
        actor,
        role,
        tenant_id: tenant.tenant_id.to_string(),
        schema_name: tenant.schema_name.clone(),
        company_id: tenant.company_id.clone().unwrap_or_default(),
    })
}

/// Platform caller: actor headers only, no tenant context. Used ONLY
/// by the provider registry and country list endpoints, which are
/// platform reference data (not tenant business data). A platform
/// caller can never read, create, or touch tenant-owned records.
pub(crate) struct PlatformCaller {
    actor: String,
    pub(crate) role: String,
}

pub(crate) fn platform_caller(req: &HttpRequest) -> UnifiedResult<PlatformCaller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(PlatformCaller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
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

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

/// Fail-closed ownership rule (same as todos.rs): a tenant-owned
/// record that belongs to another tenant answers 404, never 403, so
/// existence never leaks across tenants.
pub(crate) fn not_found_for_tenant(what: &str) -> UnifiedError {
    UnifiedError::not_found(format!("{what} not found"))
}
