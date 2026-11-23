//! Caller, permission, loading, and availability helpers.

use super::state::RentalState;
use super::types::{
    is_available, overlapping_quantity, status_holds_units, LateFeeRule, RentalDelivery,
    RentalListing, RentalOrder, RentalRate, MAX_CURRENCY_LEN, MAX_DESCRIPTION_LEN, MAX_NOTES_LEN,
    MAX_PERIOD_LABEL_LEN, MAX_PERIOD_UNIT_LEN, MAX_SLUG_LEN, MAX_TITLE_LEN, PERM_RENTAL_MANAGE,
};
use actix_web::{HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use uuid::Uuid;

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the forums module.
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

pub(crate) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// True when the caller holds the manage capability.
pub(crate) fn can_manage(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_RENTAL_MANAGE)
}

/// Resolve the tenant id for this request from the middleware's
/// resolved tenant context. Fails closed: a request with no
/// resolved tenant is rejected, and there is never a fallback to
/// the control plane.
pub(crate) fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<String> {
    unified_core::tenancy::TenantContext::from_http_request(req)
        .map(|context| context.tenant_id.to_string())
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 5, task 319).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// The records in this module carry a required `tenant_id` (not a
/// `company_id`): partitioning is by tenant, through the pre-existing
/// `load_*` helpers (cross-tenant row reads as 404) and the list
/// filters. `caller_company` below is therefore the mismatch-only
/// company check (pilot 4 group D idiom), not a strict
/// `require_company` gate: a tenant/caller company mismatch is 403,
/// but a write is never refused only because no company resolves —
/// the write is still fully tenant-partitioned by `tenant_id`.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources (header trust was
/// removed by task 317; this helper must never reintroduce it).
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified),
    }
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn require_currency(value: &str) -> UnifiedResult<String> {
    let code = value.trim().to_uppercase();
    if code.len() != MAX_CURRENCY_LEN || !code.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter code",
        ));
    }
    Ok(code)
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

pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
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

pub(crate) fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in name.to_lowercase().chars() {
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
        out.push_str("rental");
    }
    out.truncate(MAX_SLUG_LEN);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Resolve a slug from an optional client slug or a title: a
/// client slug is lowercased and validated, otherwise the title
/// is slugified. The returned slug is never empty.
pub(crate) fn resolve_slug(input: Option<&String>, title: &str) -> UnifiedResult<String> {
    match input {
        Some(raw) => {
            let slug = raw.trim().to_lowercase();
            if !valid_slug(&slug) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and single hyphens only (no leading/trailing hyphen, max 128 characters)",
                ));
            }
            Ok(slug)
        }
        None => Ok(slugify(title)),
    }
}

/// Validate a late-fee rule supplied as data: the basis must be
/// one of the documented values, the amount and grace must not
/// be negative, and a cap must not be negative.
pub(crate) fn validate_late_fee_rule(rule: &LateFeeRule) -> UnifiedResult<()> {
    match rule.basis.as_str() {
        "per_day" | "per_hour" | "flat" => {}
        other => {
            return Err(UnifiedError::bad_request(format!(
                "late_fee_rule.basis must be per_day, per_hour, or flat, got \"{other}\""
            )));
        }
    }
    if rule.amount_minor < 0 {
        return Err(UnifiedError::bad_request(
            "late_fee_rule.amount_minor must not be negative",
        ));
    }
    if rule.grace_hours < 0 {
        return Err(UnifiedError::bad_request(
            "late_fee_rule.grace_hours must not be negative",
        ));
    }
    if let Some(cap) = rule.cap_minor {
        if cap < 0 {
            return Err(UnifiedError::bad_request(
                "late_fee_rule.cap_minor must not be negative",
            ));
        }
    }
    Ok(())
}

/// Validate a period supplied as data: the unit must be one of
/// the documented calendar units and the length must be
/// positive. The units are data values, not a code list — an
/// unknown unit is rejected here so no rate is ever priced with
/// a guessed duration.
pub(crate) fn validate_period(unit: &str, length: i64) -> UnifiedResult<()> {
    match unit {
        "hour" | "day" | "week" | "month" => {}
        other => {
            return Err(UnifiedError::bad_request(format!(
                "period_unit must be hour, day, week, or month, got \"{other}\""
            )));
        }
    }
    if length <= 0 {
        return Err(UnifiedError::bad_request("period_length must be positive"));
    }
    Ok(())
}

pub(crate) fn validate_period_label(label: &str) -> UnifiedResult<String> {
    require_text(label, "period_label", MAX_PERIOD_LABEL_LEN)
}

pub(crate) fn validate_period_unit(unit: &str) -> UnifiedResult<String> {
    require_text(unit, "period_unit", MAX_PERIOD_UNIT_LEN)
}

pub(crate) fn validate_title(title: &str) -> UnifiedResult<String> {
    require_text(title, "title", MAX_TITLE_LEN)
}

pub(crate) fn validate_description(raw: &Option<String>) -> UnifiedResult<Option<String>> {
    match raw {
        Some(text) => Ok(Some(require_text(
            text,
            "description",
            MAX_DESCRIPTION_LEN,
        )?)),
        None => Ok(None),
    }
}

pub(crate) fn validate_notes(raw: &Option<String>) -> UnifiedResult<Option<String>> {
    match raw {
        Some(text) => Ok(Some(require_text(text, "condition_notes", MAX_NOTES_LEN)?)),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Tenant-scoped loaders
// ---------------------------------------------------------------------------
//
// Every loader checks the row's `tenant_id` against the caller's
// tenant. A row from another tenant reads as 404, never 403, so
// its existence does not leak across tenants.

fn tenant_match(row_tenant_id: &str, tenant_id: &str, id: Uuid, what: &str) -> UnifiedResult<()> {
    if row_tenant_id != tenant_id {
        return Err(UnifiedError::not_found(format!("{what} not found: {id}")));
    }
    Ok(())
}

pub(crate) fn load_listing(
    state: &RentalState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<RentalListing> {
    let id = parse_uuid(raw_id, "listing")?;
    let listing = state
        .listings
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Listing not found: {id}")))?;
    tenant_match(&listing.tenant_id, tenant_id, id, "Listing")?;
    Ok(listing)
}

pub(crate) fn load_listing_by_slug(
    state: &RentalState,
    tenant_id: &str,
    slug: &str,
) -> UnifiedResult<RentalListing> {
    state
        .listings
        .iter()
        .map(|entry| entry.value().clone())
        .find(|listing| listing.tenant_id == tenant_id && listing.slug == slug)
        .ok_or_else(|| UnifiedError::not_found(format!("Listing not found: {slug}")))
}

pub(crate) fn load_rate(
    state: &RentalState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<RentalRate> {
    let id = parse_uuid(raw_id, "rate")?;
    let rate = state
        .rates
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Rate not found: {id}")))?;
    tenant_match(&rate.tenant_id, tenant_id, id, "Rate")?;
    Ok(rate)
}

pub(crate) fn load_order(
    state: &RentalState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<RentalOrder> {
    let id = parse_uuid(raw_id, "order")?;
    let order = state
        .orders
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Order not found: {id}")))?;
    tenant_match(&order.tenant_id, tenant_id, id, "Order")?;
    Ok(order)
}

pub(crate) fn load_delivery(
    state: &RentalState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<RentalDelivery> {
    let id = parse_uuid(raw_id, "delivery")?;
    let delivery = state
        .deliveries
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Delivery not found: {id}")))?;
    tenant_match(&delivery.tenant_id, tenant_id, id, "Delivery")?;
    Ok(delivery)
}

// ---------------------------------------------------------------------------
// Availability
// ---------------------------------------------------------------------------

/// Booked periods for a listing: every order that still holds
/// units, as `(starts_at, ends_at, quantity)` triples. The
/// caller may exclude one order (the order being re-priced or
/// re-checked) so it does not count against itself.
pub(crate) fn booked_periods(
    state: &RentalState,
    tenant_id: &str,
    listing_id: Uuid,
    exclude_order_id: Option<Uuid>,
) -> Vec<(DateTime<Utc>, DateTime<Utc>, i64)> {
    state
        .orders
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|order| {
            order.tenant_id == tenant_id
                && order.listing_id == listing_id
                && status_holds_units(order.status)
                && Some(order.id) != exclude_order_id
        })
        .map(|order| (order.starts_at, order.ends_at, order.quantity))
        .collect()
}

/// Availability snapshot for a listing over a range: how many
/// units are already booked, and whether `requested` more fit.
pub(crate) fn availability(
    state: &RentalState,
    tenant_id: &str,
    listing: &RentalListing,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    requested: i64,
) -> Value {
    let booked = booked_periods(state, tenant_id, listing.id, None);
    let booked_quantity = overlapping_quantity(&booked, start, end);
    let available = is_available(listing.quantity, booked_quantity, requested);
    json!({
        "listing_id": listing.id.to_string(),
        "starts_at": start.to_rfc3339(),
        "ends_at": end.to_rfc3339(),
        "total_quantity": listing.quantity,
        "booked_quantity": booked_quantity,
        "available_quantity": (listing.quantity - booked_quantity).max(0),
        "requested_quantity": requested,
        "available": available,
    })
}

/// Reject a booking when the requested units do not fit in the
/// listing's availability calendar for the range.
pub(crate) fn check_availability(
    state: &RentalState,
    tenant_id: &str,
    listing: &RentalListing,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    quantity: i64,
) -> UnifiedResult<()> {
    let booked = booked_periods(state, tenant_id, listing.id, None);
    let booked_quantity = overlapping_quantity(&booked, start, end);
    if !is_available(listing.quantity, booked_quantity, quantity) {
        return Err(UnifiedError::conflict(format!(
            "Listing is not available for the requested range: {booked_quantity} of {} unit(s) already booked",
            listing.quantity
        )));
    }
    Ok(())
}

/// Order view with its computed totals and its deliveries and
/// damage reports attached, so a single read shows the whole
/// rental story.
pub(crate) fn order_view(state: &RentalState, order: &RentalOrder) -> Value {
    let mut value = serde_json::to_value(order).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let mut deliveries: Vec<RentalDelivery> = state
            .deliveries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|delivery| delivery.order_id == order.id)
            .collect();
        deliveries.sort_by_key(|delivery| delivery.created_at);
        map.insert(
            "deliveries".to_string(),
            serde_json::to_value(deliveries).unwrap_or(Value::Null),
        );
        let mut reports: Vec<Value> = state
            .damage_reports
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|report| report.order_id == order.id)
            .map(|report| serde_json::to_value(report).unwrap_or(Value::Null))
            .collect();
        reports.sort_by(|a, b| {
            a.get("created_at")
                .and_then(Value::as_str)
                .cmp(&b.get("created_at").and_then(Value::as_str))
        });
        map.insert("damage_reports".to_string(), Value::Array(reports));
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn pagination_params(page: Option<u32>, per_page: Option<u32>) -> PaginationParams {
        PaginationParams {
            page,
            per_page,
            sort: None,
            order: None,
            search: None,
            filters: HashMap::new(),
        }
    }

    #[test]
    fn valid_slug_rules() {
        assert!(valid_slug("camera-kit"));
        assert!(valid_slug("a"));
        assert!(!valid_slug("Camera"));
        assert!(!valid_slug("a--b"));
        assert!(!valid_slug("-a"));
        assert!(!valid_slug("a-"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("a_b"));
    }

    #[test]
    fn slugify_normalizes_titles() {
        assert_eq!(slugify("Camera Kit Pro!"), "camera-kit-pro");
        assert_eq!(slugify(""), "rental");
        assert_eq!(slugify("!!!"), "rental");
    }

    #[test]
    fn currency_must_be_three_letters() {
        assert_eq!(require_currency("usd"), Ok("USD".to_string()));
        assert!(require_currency("US").is_err());
        assert!(require_currency("USDD").is_err());
        assert!(require_currency("U1D").is_err());
    }

    #[test]
    fn late_fee_rule_validation() {
        let good = LateFeeRule {
            basis: "per_day".to_string(),
            amount_minor: 1_000,
            grace_hours: 2,
            cap_minor: Some(10_000),
        };
        assert!(validate_late_fee_rule(&good).is_ok());
        let bad_basis = LateFeeRule {
            basis: "per_moon".to_string(),
            ..good.clone()
        };
        assert!(validate_late_fee_rule(&bad_basis).is_err());
        let negative = LateFeeRule {
            amount_minor: -1,
            ..good.clone()
        };
        assert!(validate_late_fee_rule(&negative).is_err());
        let negative_cap = LateFeeRule {
            cap_minor: Some(-5),
            ..good
        };
        assert!(validate_late_fee_rule(&negative_cap).is_err());
    }

    #[test]
    fn period_validation() {
        assert!(validate_period("day", 1).is_ok());
        assert!(validate_period("month", 3).is_ok());
        assert!(validate_period("fortnight", 1).is_err());
        assert!(validate_period("day", 0).is_err());
    }

    #[test]
    fn paginate_bounds() {
        let items: Vec<i64> = (1..=5).collect();
        let first = paginate(items.clone(), &pagination_params(Some(1), Some(2)));
        assert_eq!(first.items, vec![1, 2]);
        assert_eq!(first.pagination.total, 5);
        let past_end = paginate(items.clone(), &pagination_params(Some(4), Some(2)));
        assert!(past_end.items.is_empty());
        assert_eq!(past_end.pagination.total, 5);
    }
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::test::TestRequest;

    fn tenant_request(company_id: &str, caller_company_id: Option<&str>) -> HttpRequest {
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
        if let Some(company) = caller_company_id {
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
        let req = tenant_request("acme", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }

    #[test]
    fn caller_company_prefers_verified_caller_when_tenant_has_none() {
        let req = tenant_request("", Some("acme"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request("acme", Some("other"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_ignores_the_x_company_id_header() {
        // A forged header naming another company must not move
        // the scope: only the tenant context and the verified
        // caller count.
        let req = TestRequest::default()
            .insert_header(("x-company-id", "other"))
            .to_http_request();
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
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid("alice"), Vec::new()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }
}
