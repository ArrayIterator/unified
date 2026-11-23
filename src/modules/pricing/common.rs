// pricing — common.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::middleware::TenantContext;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::money::*;
use super::recommendations::*;
use super::discounts::*;
use super::proration::*;
use super::reconciliation::*;
use super::types::*;
use super::store::*;
use super::service::*;
use super::handlers_recommendations::*;
use super::handlers_rules::*;
use super::handlers_coupons::*;
use super::handlers_prices::*;
use super::handlers_bundles::*;
use super::handlers_reconciliation::*;
use super::module::*;

/// The server-verified caller (see `unified_core::caller::Caller`):
/// the user id identifies the actor, the server-resolved role is the
/// role slug or id the capability check runs against.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
    /// Tenant id (uuidv7) from the middleware-attached context.
    /// Every handler scopes its reads and writes to this tenant.
    pub(crate) tenant_id: String,
}

/// Resolve the tenant context for a request. This is the single
/// choke point for tenant scoping: it reads ONLY the
/// middleware-attached `TenantContext` (resolved through the
/// `tenant.resolve_requested` hook), never a client header, and
/// never a hard-coded id. No tenant context means fail closed
/// (403): pricing data is tenant business data and must never
/// fall back to the `public` schema.
///
/// CONVERGENCE POINT (batch 17): when the schema-per-tenant
/// plumbing lands, this is the one function the integrator swaps
/// to also return the batch-17 schema name / search_path handle.
/// No handler changes: every handler already takes its tenant
/// from the `Caller` this function feeds.
pub(crate) fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<TenantContext> {
    TenantContext::from_request(req).ok_or_else(|| {
        UnifiedError::forbidden(
            "No tenant context resolved for this request: pricing data is tenant-scoped and the request host did not resolve to a tenant",
        )
    })
}

/// Validate a tenant id carried in a hook payload: a uuidv7 in
/// canonical form (36 characters, hyphens at 8/13/18/23, version
/// nibble `7`). The id itself is never hard-coded; this only
/// checks the shape so a malformed payload fails closed instead
/// of scoping reads to a nonsense tenant.
pub fn validate_tenant_id(value: &str) -> UnifiedResult<String> {
    let tenant_id = value.trim().to_string();
    let chars: Vec<char> = tenant_id.chars().collect();
    let well_formed = chars.len() == 36
        && chars[8] == '-'
        && chars[13] == '-'
        && chars[18] == '-'
        && chars[23] == '-'
        && chars[14] == '7'
        && chars
            .iter()
            .enumerate()
            .all(|(index, c)| {
                index == 8 || index == 13 || index == 18 || index == 23 || c.is_ascii_hexdigit()
            });
    if well_formed {
        Ok(tenant_id)
    } else {
        Err(UnifiedError::bad_request(
            "tenant_id must be a uuidv7 in canonical form",
        ))
    }
}

/// Read the tenant id from a hook payload, failing closed when it
/// is absent or malformed.
pub(crate) fn hook_tenant_id(ctx: &HookContext) -> UnifiedResult<String> {
    match ctx.get("tenant_id").and_then(|value| value.as_str()) {
        Some(value) => validate_tenant_id(value),
        None => Err(UnifiedError::bad_request(
            "Hook payload must carry tenant_id: pricing data is tenant-scoped",
        )),
    }
}

/// Ownership check for a single record: a record owned by another
/// tenant answers 404 (not found), never leaks its existence.
pub(crate) fn require_same_tenant(record_tenant_id: &str, tenant_id: &str) -> UnifiedResult<()> {
    if record_tenant_id == tenant_id {
        Ok(())
    } else {
        Err(UnifiedError::not_found("Record not found"))
    }
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let tenant = resolve_tenant(req)?;
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller {
        actor,
        role,
        tenant_id: tenant.tenant_id,
    })
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

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.chars().count() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn require_currency_known(state: &PricingState, currency: &str) -> UnifiedResult<()> {
    validate_currency(currency)?;
    match state.currencies.get(currency) {
        Some(entry) if entry.active => Ok(()),
        _ => Err(UnifiedError::bad_request(format!(
            "Currency {currency} is not an active pricing currency"
        ))),
    }
}

pub(crate) fn normalize_code(code: &str) -> UnifiedResult<String> {
    let normalized = code.trim().to_uppercase();
    if normalized.is_empty() || normalized.chars().count() > 64 {
        return Err(UnifiedError::bad_request(
            "Coupon code must be 1 to 64 characters",
        ));
    }
    if !normalized
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(UnifiedError::bad_request(
            "Coupon code may only contain letters, digits, hyphens, and underscores",
        ));
    }
    Ok(normalized)
}

/// Read a JSON value that may be an integer, a decimal string, or
/// a JSON number, and parse it as scaled minor units without ever
/// using float arithmetic. JSON numbers are read through their
/// decimal string form (`Value::to_string`), so a billing float
/// such as 19.99 becomes the string "19.99" and is parsed with
/// integer math. Values with more than 4 fractional digits are
/// rejected rather than silently rounded.
pub(crate) fn value_to_scaled(value: &Value, currency: &str, field: &str) -> UnifiedResult<i64> {
    match value {
        Value::String(text) => parse_signed_scaled_amount(text, currency),
        Value::Number(number) => {
            let text = number.to_string();
            parse_signed_scaled_amount(&text, currency)
        }
        Value::Null => Err(UnifiedError::bad_request(format!(
            "{field} must not be null"
        ))),
        _ => Err(UnifiedError::bad_request(format!(
            "{field} must be a number or a decimal string"
        ))),
    }
}
