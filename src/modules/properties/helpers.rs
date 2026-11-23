// Properties — Helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the events module.
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
/// (same fail-closed shape as the events module's capability check).
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

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 3).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`).
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in, as a `Uuid`: the tenant
/// context's company first, the verified caller's server-resolved
/// company otherwise. Fails closed (403) when both are present and
/// disagree — a request must not carry one company in its tenant
/// context and another in its verified identity. The
/// `X-Company-Id` header is NEVER trusted here: only the verified
/// caller (company resolved from the database at authentication
/// time) and the middleware-resolved tenant context are sources
/// (header trust removal is task 317's job elsewhere). A company
/// value that does not parse as a UUID names no tenant here and is
/// ignored.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<Uuid>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty())
        .and_then(|raw| Uuid::parse_str(&raw).ok());
    let tenant_company = tenant
        .company_id
        .as_deref()
        .and_then(|raw| Uuid::parse_str(raw).ok());
    match (tenant_company, verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company)),
        (None, verified) => Ok(verified),
    }
}

/// Verify a request body's company id against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company stores
/// a global row.
pub(crate) fn require_company_match_uuid(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &Option<Uuid>,
) -> UnifiedResult<Option<Uuid>> {
    let caller = caller_company(tenant, req)?;
    require_company_match_uuid_for(&caller, body_company_id)
}

/// `require_company_match_uuid` against an already-resolved caller
/// scope, for handlers that computed `caller_company` first.
pub(crate) fn require_company_match_uuid_for(
    caller: &Option<Uuid>,
    body_company_id: &Option<Uuid>,
) -> UnifiedResult<Option<Uuid>> {
    match (body_company_id, caller) {
        (Some(body), Some(caller_company)) if body != caller_company => Err(
            UnifiedError::forbidden("company_id does not match the caller's company scope"),
        ),
        (Some(body), Some(_)) => Ok(Some(*body)),
        (Some(_body), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, caller) => Ok(*caller),
    }
}

/// Verify an UPDATE body's newly named company against the caller
/// scope and return the company id to store. Unlike
/// `require_company_match_uuid_for` (create: an omitted company
/// adopts the caller scope), an explicit clear (`None`) stays a
/// clear and is never re-stamped with the caller scope.
pub(crate) fn require_company_change_uuid_for(
    caller: &Option<Uuid>,
    new_company_id: &Option<Uuid>,
) -> UnifiedResult<Option<Uuid>> {
    match (new_company_id, caller) {
        (Some(body), Some(caller_company)) if body != caller_company => Err(
            UnifiedError::forbidden("company_id does not match the caller's company scope"),
        ),
        (Some(body), Some(_)) => Ok(Some(*body)),
        (Some(_body), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, _) => Ok(None),
    }
}

/// True when a stored record belongs to the caller's company
/// scope. With no caller scope (an unresolved caller) only
/// global records match: unscoped records are visible, every
/// company-owned record is denied (the shared
/// `CallerScope::Unresolved` rule). With a scope, only records
/// carrying exactly that company match; unscoped (legacy) records
/// are hidden from scoped callers.
pub(crate) fn in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<Uuid>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => record_company_id.as_ref() == Some(company),
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope` as a filter instead.
pub(crate) fn require_record_in_company_scope(
    record_company_id: &Option<Uuid>,
    caller_company: &Option<Uuid>,
) -> UnifiedResult<()> {
    if in_company_scope(record_company_id, caller_company) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "The record belongs to a different company",
        ))
    }
}

/// True when the named property is in the caller scope. A missing
/// property is out of scope (fail-closed).
pub(crate) fn property_in_scope(
    state: &PropertiesState,
    property_id: Uuid,
    scope: &Option<Uuid>,
) -> bool {
    state
        .properties
        .get(&property_id)
        .is_some_and(|entry| in_company_scope(&entry.value().company_id, scope))
}

/// Load a property by id and fail closed (403) when it is outside
/// the caller scope. Plans, blocks, history, and purchases scope
/// through their parent property with this.
pub(crate) fn require_property_by_id_in_scope(
    state: &PropertiesState,
    property_id: Uuid,
    scope: &Option<Uuid>,
) -> UnifiedResult<Property> {
    let property = state
        .properties
        .get(&property_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Property not found: {property_id}")))?;
    require_record_in_company_scope(&property.company_id, scope)?;
    Ok(property)
}

/// Load a property by its path id and fail closed (403) when it is
/// outside the caller scope.
pub(crate) fn require_property_in_scope(
    state: &PropertiesState,
    raw_id: &str,
    scope: &Option<Uuid>,
) -> UnifiedResult<Property> {
    let property = load_property(state, raw_id)?;
    require_record_in_company_scope(&property.company_id, scope)?;
    Ok(property)
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
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

/// A slug is lowercase letters, digits, and single hyphens only,
/// never leading or trailing with a hyphen, at most 128 chars.
pub(crate) fn valid_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > MAX_SLUG_LEN {
        return false;
    }
    if slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub(crate) fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in title.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("property");
    }
    out.truncate(MAX_SLUG_LEN);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Money: a price in minor units, non-negative. The request JSON
/// already converted major units to minor units through
/// `serde_major` (half away from zero to minor units), so this only
/// rejects negative amounts; the old 4-decimal float rounding no
/// longer applies because amounts are integers now.
pub(crate) fn valid_price(price: i64) -> UnifiedResult<i64> {
    if price < 0 {
        return Err(UnifiedError::bad_request("price must be >= 0"));
    }
    Ok(price)
}

/// Round a physical measurement (area in square metres, length in
/// metres) to 4 decimal places. This is NOT money: block areas and
/// lengths stay `f64`, only prices moved to minor units.
pub(crate) fn round_measure(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
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

pub(crate) fn valid_latitude(latitude: f64) -> UnifiedResult<f64> {
    if !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude) {
        return Err(UnifiedError::bad_request("latitude must be between -90 and 90"));
    }
    Ok(latitude)
}

pub(crate) fn valid_longitude(longitude: f64) -> UnifiedResult<f64> {
    if !longitude.is_finite() || !(-180.0..=180.0).contains(&longitude) {
        return Err(UnifiedError::bad_request(
            "longitude must be between -180 and 180",
        ));
    }
    Ok(longitude)
}

/// Validate a click target against the click behaviour and the
/// link kind. `popup` ignores any target. `product` / `order`
/// targets must be UUIDs; `page` targets must be a relative path
/// ("/...") or an absolute http(s) URL whose host is on the
/// configured link allowlist.
pub(crate) fn validate_click_target(
    behavior: ClickBehavior,
    link_kind: Option<ClickLinkKind>,
    link_target: Option<&str>,
    link_allowlist_hosts: &[String],
) -> UnifiedResult<(Option<ClickLinkKind>, Option<String>)> {
    match behavior {
        ClickBehavior::Popup => Ok((None, None)),
        ClickBehavior::Link => {
            let kind = link_kind.ok_or_else(|| {
                UnifiedError::bad_request("click_link_kind is required when click_behavior is \"link\"")
            })?;
            let target = link_target
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    UnifiedError::bad_request("click_link_target is required when click_behavior is \"link\"")
                })?;
            match kind {
                ClickLinkKind::Product | ClickLinkKind::Order => {
                    Uuid::parse_str(target).map_err(|_| {
                        UnifiedError::bad_request(
                            "click_link_target must be a UUID for product / order links",
                        )
                    })?;
                    Ok((Some(kind), Some(target.to_string())))
                }
                ClickLinkKind::Page => {
                    if target.starts_with('/') && !target.starts_with("//") {
                        return Ok((Some(kind), Some(target.to_string())));
                    }
                    let parsed = reqwest::Url::parse(target).map_err(|_| {
                        UnifiedError::bad_request(
                            "click_link_target must be a relative path or an absolute URL",
                        )
                    })?;
                    if parsed.scheme() != "http" && parsed.scheme() != "https" {
                        return Err(UnifiedError::bad_request(
                            "click_link_target absolute URLs must use http or https",
                        ));
                    }
                    let host = parsed.host_str().unwrap_or("");
                    if !link_allowlist_hosts.iter().any(|allowed| allowed == host) {
                        return Err(UnifiedError::bad_request(format!(
                            "click_link_target host \"{host}\" is not on the link allowlist"
                        )));
                    }
                    Ok((Some(kind), Some(target.to_string())))
                }
            }
        }
    }
}

pub(crate) fn load_property(state: &PropertiesState, raw_id: &str) -> UnifiedResult<Property> {
    let id = parse_uuid(raw_id, "property")?;
    state
        .properties
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Property not found: {id}")))
}

pub(crate) fn load_property_by_slug(state: &PropertiesState, slug: &str) -> UnifiedResult<Property> {
    state
        .properties
        .iter()
        .map(|entry| entry.value().clone())
        .find(|property| property.slug == slug)
        .ok_or_else(|| UnifiedError::not_found(format!("Property not found: {slug}")))
}

pub(crate) fn load_plan(state: &PropertiesState, property_id: Uuid, raw_id: &str) -> UnifiedResult<PropertyPlan> {
    let id = parse_uuid(raw_id, "plan")?;
    state
        .plans
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|plan| plan.property_id == property_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Plan not found: {id}")))
}

pub(crate) fn load_block(
    state: &PropertiesState,
    property_id: Uuid,
    plan_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<PropertyBlock> {
    let id = parse_uuid(raw_id, "block")?;
    state
        .blocks
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|block| block.property_id == property_id && block.plan_id == plan_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Block not found: {id}")))
}

pub(crate) fn slug_in_use(state: &PropertiesState, slug: &str, except: Option<Uuid>) -> bool {
    state.properties.iter().any(|entry| {
        entry.value().slug == slug && Some(entry.value().id) != except
    })
}

pub(crate) fn validate_storage_reference(target: &str, key: &str) -> UnifiedResult<()> {
    if target.trim().is_empty() || target.len() > MAX_NAME_LEN {
        return Err(UnifiedError::bad_request(
            "cover_storage_target must not be empty and must be at most 200 characters",
        ));
    }
    if key.trim().is_empty() || key.len() > MAX_STORAGE_KEY_LEN {
        return Err(UnifiedError::bad_request(
            "cover_storage_key must not be empty and must be at most 512 characters",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_slug_accepts_lowercase_dashes_and_digits() {
        assert!(valid_slug("my-property-1"));
    }

    #[test]
    fn valid_slug_rejects_uppercase() {
        assert!(!valid_slug("My-Property"));
    }

    #[test]
    fn valid_slug_rejects_edge_and_double_dashes() {
        assert!(!valid_slug("-lead"));
        assert!(!valid_slug("trail-"));
        assert!(!valid_slug("a--b"));
        assert!(!valid_slug(""));
    }

    #[test]
    fn slugify_turns_title_into_slug() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("Block  A  --  1"), "block-a-1");
    }

    #[test]
    fn slugify_falls_back_for_empty_titles() {
        assert_eq!(slugify(""), "property");
        assert_eq!(slugify("  --  "), "property");
    }

    #[test]
    fn latitude_bounds_are_enforced() {
        assert!(valid_latitude(-90.0).is_ok());
        assert!(valid_latitude(90.0).is_ok());
        assert!(valid_latitude(91.0).is_err());
        assert!(valid_latitude(-91.0).is_err());
    }

    #[test]
    fn valid_price_accepts_minor_units_and_rejects_negatives() {
        assert_eq!(valid_price(0).unwrap(), 0);
        assert_eq!(valid_price(10050).unwrap(), 10050);
        assert!(valid_price(-1).is_err());
    }

    #[test]
    fn round_measure_keeps_four_decimals() {
        assert_eq!(round_measure(10.00006), 10.0001);
        assert_eq!(round_measure(10.00004), 10.0);
        assert_eq!(round_measure(2.5), 2.5);
    }
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::test::TestRequest;
    use actix_web::HttpMessage;

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach, plus a verified
    /// caller (optionally with a server-resolved company).
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        let req = TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: company_id.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let mut caller =
            unified_core::auth::Caller::verified(test_user_uuid("alice"), Vec::new());
        if let Some(company) = caller_company {
            caller = caller.with_company(company.to_string());
        }
        req.extensions_mut().insert(caller);
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

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let company = Uuid::new_v4();
        let req = tenant_request(&company.to_string(), None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some(company)
        );
    }

    #[test]
    fn caller_company_prefers_verified_caller_when_tenant_has_none() {
        let company = Uuid::new_v4();
        let req = tenant_request("", Some(&company.to_string()));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some(company)
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request(
            &Uuid::new_v4().to_string(),
            Some(&Uuid::new_v4().to_string()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_ignores_the_x_company_id_header() {
        // A forged header naming another company must not move
        // the scope: only the tenant context and the verified
        // caller count.
        let company = Uuid::new_v4();
        let other = Uuid::new_v4();
        let req = TestRequest::default()
            .insert_header(("x-company-id", other.to_string()))
            .to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: company.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid("alice"), Vec::new()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some(company)
        );
    }

    #[test]
    fn in_company_scope_hides_unscoped_records_from_scoped_callers() {
        let company = Uuid::new_v4();
        let other = Uuid::new_v4();
        let scope = Some(company);
        assert!(in_company_scope(&Some(company), &scope));
        assert!(!in_company_scope(&Some(other), &scope));
        assert!(!in_company_scope(&None, &scope));
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some(other), &None));
    }

    #[test]
    fn require_company_match_uuid_for_refuses_body_company_from_unresolved_callers() {
        let company = Uuid::new_v4();
        let other = Uuid::new_v4();
        let body = Some(company);
        assert!(require_company_match_uuid_for(&None, &body).is_err());
        assert!(require_company_match_uuid_for(&None, &None)
            .unwrap()
            .is_none());
        assert_eq!(
            require_company_match_uuid_for(&Some(company), &body).unwrap(),
            Some(company)
        );
        assert!(require_company_match_uuid_for(&Some(company), &Some(other)).is_err());
    }

    #[test]
    fn require_company_change_uuid_for_keeps_an_explicit_clear_a_clear() {
        let company = Uuid::new_v4();
        let other = Uuid::new_v4();
        let scope = Some(company);
        assert_eq!(
            require_company_change_uuid_for(&scope, &None).unwrap(),
            None
        );
        assert_eq!(
            require_company_change_uuid_for(&scope, &Some(company)).unwrap(),
            Some(company)
        );
        assert!(require_company_change_uuid_for(&scope, &Some(other)).is_err());
        assert!(require_company_change_uuid_for(&None, &Some(company)).is_err());
        assert_eq!(
            require_company_change_uuid_for(&None, &None).unwrap(),
            None
        );
    }
}
