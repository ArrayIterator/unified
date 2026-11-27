//! Small shared helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// Helpers

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the ai module.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
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

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
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
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

/// Money: an amount already in minor units (request JSON converted
/// major units through `serde_major` before this runs), non-negative.
pub(crate) fn valid_price(price: i64) -> UnifiedResult<i64> {
    if price < 0 {
        return Err(UnifiedError::bad_request("price must be >= 0"));
    }
    Ok(price)
}

/// True for zero-decimal currencies (no minor units), matching the
/// billing and store modules' list.
pub(crate) fn is_zero_decimal_currency(currency: &str) -> bool {
    unified_core::db::money::is_zero_decimal_currency(currency)
}

/// Convert integer minor units to major units, as JSON and hook
/// payloads carry them. This is the boundary conversion out of the
/// integer representation; business logic keeps minor units.
pub(crate) fn minor_to_major(amount_minor: i64, currency: &str) -> f64 {
    if is_zero_decimal_currency(currency) {
        amount_minor as f64
    } else {
        let major = amount_minor as f64 / 100.0;
        (major * 100.0).round() / 100.0
    }
}

/// Currency: 3-letter ISO code, uppercased.
pub(crate) fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let upper = currency.trim().to_uppercase();
    if upper.len() != 3 || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code such as \"USD\"",
        ));
    }
    Ok(upper)
}

pub(crate) fn load_asset(state: &AssetsState, raw_id: &str) -> UnifiedResult<Asset> {
    let id = parse_uuid(raw_id, "asset")?;
    state
        .assets
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Asset not found: {id}")))
}

pub(crate) fn load_stock(state: &AssetsState, raw_id: &str) -> UnifiedResult<AssetStock> {
    let id = parse_uuid(raw_id, "stock item")?;
    state
        .stock
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Stock item not found: {id}")))
}

pub(crate) fn load_stock_take(state: &AssetsState, raw_id: &str) -> UnifiedResult<AssetStockTake> {
    let id = parse_uuid(raw_id, "stock take")?;
    state
        .stock_takes
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Stock take not found: {id}")))
}

pub(crate) fn load_stock_take_line(
    state: &AssetsState,
    stock_take_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<AssetStockTakeLine> {
    let id = parse_uuid(raw_id, "stock take line")?;
    state
        .stock_take_lines
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|line| line.stock_take_id == stock_take_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Stock take line not found: {id}")))
}

/// The active (unreturned) assignment of an asset, if any.
pub(crate) fn active_assignment(state: &AssetsState, asset_id: Uuid) -> Option<AssetAssignment> {
    state
        .assignments
        .iter()
        .map(|entry| entry.value().clone())
        .find(|assignment| assignment.asset_id == asset_id && assignment.status == "active")
}

/// True when no other asset already uses this serial number.
/// Serials compare case-insensitively after trimming, so `ABC`
/// and `abc` are the same serial number.
pub(crate) fn serial_is_free(state: &AssetsState, serial: &str, except: Option<Uuid>) -> bool {
    let wanted = serial.trim().to_lowercase();
    !state.assets.iter().any(|entry| {
        let asset = entry.value();
        asset
            .serial_number
            .as_deref()
            .map(|value| value.trim().to_lowercase())
            == Some(wanted.clone())
            && Some(asset.id) != except
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_price_accepts_minor_units_and_rejects_negatives() {
        assert_eq!(valid_price(1000).unwrap(), 1000);
        assert_eq!(valid_price(0).unwrap(), 0);
        assert!(valid_price(-1).is_err());
    }

    #[test]
    fn minor_to_major_converts_cents_and_zero_decimal_currencies() {
        assert_eq!(minor_to_major(1025, "USD"), 10.25);
        assert_eq!(minor_to_major(1025, "JPY"), 1025.0);
        assert_eq!(minor_to_major(0, "USD"), 0.0);
    }

    #[test]
    fn valid_currency_uppercases() {
        assert_eq!(valid_currency("usd").unwrap(), "USD");
        assert_eq!(valid_currency(" idr ").unwrap(), "IDR");
    }

    #[test]
    fn valid_currency_rejects_bad() {
        assert!(valid_currency("US").is_err());
        assert!(valid_currency("USDD").is_err());
        assert!(valid_currency("U1D").is_err());
    }

    #[test]
    fn require_text_counts_characters_not_bytes() {
        // 200 multibyte characters are 400 bytes; the limit is 200
        // characters, so this must pass.
        let name = "é".repeat(200);
        assert_eq!(require_text(&name, "name", 200).unwrap(), name);
        assert!(require_text(&"é".repeat(201), "name", 200).is_err());
    }

    fn test_asset(serial: Option<&str>) -> Asset {
        Asset {
            id: Uuid::new_v4(),
            name: "Test asset".to_string(),
            description: None,
            asset_type: AssetType::Hardware,
            serial_number: serial.map(str::to_string),
            purchase_date: None,
            purchase_price: 0,
            currency: "USD".to_string(),
            vendor: None,
            location: None,
            status: AssetStatus::InStock,
            warranty_end: None,
            license_expires_at: None,
            license_seats: None,
            depreciation_method: None,
            useful_life_years: None,
            salvage_value: 0,
            depreciation_status: "none".to_string(),
            depreciation_schedule: None,
            depreciation_error: None,
            created_by: "tester".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn serial_is_free_is_case_insensitive_and_trims() {
        let state = AssetsState::default();
        let asset = test_asset(Some("ABC-123"));
        state.assets.insert(asset.id, asset.clone());
        assert!(!serial_is_free(&state, "abc-123", None));
        assert!(!serial_is_free(&state, "  ABC-123  ", None));
        assert!(!serial_is_free(&state, "AbC-123", None));
        assert!(serial_is_free(&state, "ABC-124", None));
        assert!(serial_is_free(&state, "abc-123", Some(asset.id)));
    }
}

// Tenant edge guard (C3 pilot 4, group C).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Asset (assets, stock, stock takes) records carry no company field, so the guard is a
/// fail-closed gate only: real per-company partitioning needs a
/// `tenant_id` on the records (C11 follow-up), and no per-record
/// company check is applied here.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::HttpMessage;
    use actix_web::test::TestRequest;

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach.
    fn tenant_request() -> HttpRequest {
        let req = TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "acme".to_string(),
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

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request();
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
    }
}
