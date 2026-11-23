// Store module - shared helpers.
//
// Split from `src/modules/store.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse};
use serde::Serialize;
use serde_json::json;

use super::state::StoreState;
use super::types::{DigitalProduct, DownloadEntitlement, Order, Product};

// Batch 18: payment reference ids (END)
// ---------------------------------------------------------------------------

pub(crate) fn round_money(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Apply a ratio (a percentage, a day share) to a minor-unit
/// amount, rounding half away from zero to minor units. Rates are
/// `f64` fractions by design, so the multiplication itself stays
/// float; the result is rounded exactly once to minor units, which
/// is the same rounding `round_money` applied to the old float
/// math.
pub(crate) fn apply_ratio_to_minor(amount_minor: i64, ratio: f64) -> i64 {
    (amount_minor as f64 * ratio).round() as i64
}

/// Integer division rounded half away from zero, with `i128`
/// intermediates so proportional splits never overflow. A zero
/// divisor yields zero (callers only divide by positive sums).
pub(crate) fn div_rounded(dividend: i128, divisor: i128) -> i128 {
    if divisor == 0 {
        return 0;
    }
    let negative = (dividend < 0) != (divisor < 0);
    let numerator = dividend.unsigned_abs();
    let denominator = divisor.unsigned_abs();
    let quotient = (numerator + denominator / 2) / denominator;
    if negative {
        -(quotient as i128)
    } else {
        quotient as i128
    }
}

/// Saturating `i128` to `i64` conversion for money results.
pub(crate) fn saturating_i64(value: i128) -> i64 {
    if value > i64::MAX as i128 {
        i64::MAX
    } else if value < i64::MIN as i128 {
        i64::MIN
    } else {
        value as i64
    }
}

/// Line total for a unit price in minor units times a quantity.
/// Exact integer multiplication: unit prices are already minor
/// units, so no rounding is involved.
pub(crate) fn line_total_for(unit_price_minor: i64, quantity: u32) -> i64 {
    unit_price_minor.saturating_mul(i64::from(quantity))
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

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// Cart owner: `x-session-id` header, then `x-user-id`, then `guest`.
pub(crate) fn owner_of(req: &HttpRequest) -> String {
    let verified = unified_core::caller::caller_from(req);
    if verified.is_authenticated() {
        verified.user_id
    } else {
        "guest".to_string()
    }
}

/// Caller identities from the server-verified caller. A caller owns
/// a record when the verified user id matches the record owner. This
/// is an ownership check on verified identity, not authentication: it
/// stops cross-user mistakes. A caller with no verified identity
/// matches only `guest`-owned records, so anonymous calls can no
/// longer act on another user's order.
pub(crate) fn caller_identities(req: &HttpRequest) -> Vec<String> {
    let verified = unified_core::caller::caller_from(req);
    if verified.is_authenticated() {
        vec![verified.user_id]
    } else {
        Vec::new()
    }
}

/// True when the caller may act on a record owned by `owner`: no
/// identity headers (see `caller_identities`), or one of the caller
/// identities matches the owner. An anonymous `guest` owner matches a
/// caller with no identity headers.
pub(crate) fn caller_owns(req: &HttpRequest, owner: &str) -> bool {
    let identities = caller_identities(req);
    if identities.is_empty() {
        return owner == "guest";
    }
    identities.iter().any(|identity| identity == owner)
}

/// Reject a mutating order call from a caller who does not own the
/// order (see `caller_owns`).
pub(crate) fn require_order_owner(req: &HttpRequest, order: &Order) -> UnifiedResult<()> {
    if caller_owns(req, &order.user_id) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Order belongs to a different user",
        ))
    }
}

/// Reject a download-entitlement call from a caller who does not own
/// the entitlement (see `caller_owns`).
pub(crate) fn require_entitlement_owner(
    req: &HttpRequest,
    entitlement: &DownloadEntitlement,
) -> UnifiedResult<()> {
    if caller_owns(req, &entitlement.user_id) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Download entitlement belongs to a different user",
        ))
    }
}

/// True when a list endpoint may show a record owned by `owner` to
/// this caller: a caller holding the manage permission sees every
/// record, otherwise only their own records (an anonymous caller
/// only ever sees `guest`-owned records).
pub(crate) fn record_visible_to_caller(req: &HttpRequest, owner: &str, manage: bool) -> bool {
    if manage {
        return true;
    }
    caller_owns(req, owner)
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 5, group E).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before any capability or ownership check.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// -- a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
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

/// Writes require a resolved company: a mutating handler with no
/// resolvable company scope is refused (403). Reads proceed on the
/// tenant gate plus the mismatch check above.
pub(crate) fn require_caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?
        .ok_or_else(|| UnifiedError::forbidden("A write requires a resolved company scope"))
}

/// Verify a request body's company id against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company; omitting the company
/// stores a global row.
pub(crate) fn require_company_match(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    let caller = caller_company(tenant, req)?;
    require_company_match_for(&caller, body_company_id)
}

/// `require_company_match` against an already-resolved caller
/// scope, for handlers that computed `caller_company` first.
pub(crate) fn require_company_match_for(
    caller: &Option<String>,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    match (body_company_id, caller) {
        (Some(body), Some(caller_company)) if body != caller_company => Err(
            UnifiedError::forbidden("company_id does not match the caller's company scope"),
        ),
        (Some(body), Some(_)) => Ok(Some(body.clone())),
        (Some(body), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, caller) => Ok(caller.clone()),
    }
}

/// True when a record carrying `record_company_id` is visible to a
/// caller acting in `caller_company`: with a caller scope, only
/// records carrying exactly that company match -- unscoped
/// (legacy/global) records are HIDDEN from scoped callers; an
/// unresolved caller sees global records only. This is the pilot-3
/// strict idiom, deliberately fail-closed for legacy unscoped rows:
/// if product wants scoped callers to see global rows, that is a
/// conscious one-line change here, not an accident of this pilot.
pub(crate) fn in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => record_company_id.as_deref() == Some(company.as_str()),
    }
}

/// Refuse a single-record read or write whose record belongs to
/// another company (see `in_company_scope`).
pub(crate) fn require_record_in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    if in_company_scope(record_company_id, caller_company) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "The record belongs to a different company",
        ))
    }
}

/// Load a product and refuse it when it belongs to another company.
/// Used by the seller-side handlers whose records are scoped
/// through their parent product (tiers, catalog links, coupon
/// product lists, digital profiles).
pub(crate) fn require_product_in_scope(
    state: &StoreState,
    scope: &Option<String>,
    product_id: &str,
) -> UnifiedResult<Product> {
    let product = state
        .products
        .get(product_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Product {product_id} not found")))?;
    require_record_in_company_scope(&product.company_id, scope)?;
    Ok(product)
}

/// Load a digital product and refuse it when its royalty payee
/// company is another company.
pub(crate) fn require_digital_product_in_scope(
    state: &StoreState,
    scope: &Option<String>,
    digital_product_id: &str,
) -> UnifiedResult<DigitalProduct> {
    let digital = state
        .digital_products
        .get(digital_product_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Digital product not found"))?;
    require_record_in_company_scope(&digital.royalty_payee_company_id, scope)?;
    Ok(digital)
}

/// The company a download entitlement belongs to, through its
/// order (`None` when the order is gone).
pub(crate) fn entitlement_company(
    state: &StoreState,
    entitlement: &DownloadEntitlement,
) -> Option<String> {
    state
        .orders
        .get(&entitlement.order_id)
        .and_then(|entry| entry.value().company_id.clone())
}

/// The company a license belongs to: the issued-license mirror
/// first, else its order (`None` when neither names one).
pub(crate) fn license_company(state: &StoreState, license_id: &str) -> Option<String> {
    if let Some(entry) = state.issued_licenses.get(license_id) {
        if let Some(company) = &entry.value().company_id {
            return Some(company.clone());
        }
        if let Some(order_id) = &entry.value().order_id {
            return state
                .orders
                .get(order_id)
                .and_then(|order| order.value().company_id.clone());
        }
        return None;
    }
    state
        .downloads
        .iter()
        .find(|entry| entry.value().license_id == license_id)
        .and_then(|entry| {
            entry
                .value()
                .order_id
                .as_ref()
                .and_then(|order_id| state.orders.get(order_id))
                .and_then(|order| order.value().company_id.clone())
        })
}

/// True for zero-decimal currencies (no minor units), matching the
/// payments module's list.
pub(crate) fn is_zero_decimal_currency(currency: &str) -> bool {
    matches!(
        currency,
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "UGX" | "XAF" | "XOF" | "XPF"
    )
}

/// Convert a major-unit amount (order totals) to integer minor units.
///
/// Returns `None` for non-finite values and for magnitudes that
/// would overflow `i64` — the old unchecked `as i64` cast silently
/// saturated `1e17` to `i64::MAX`. Callers must handle `None`
/// (reject the amount, skip the record) instead of assuming a
/// conversion always succeeds.
pub(crate) fn major_to_minor(amount_major: f64, currency: &str) -> Option<i64> {
    if !amount_major.is_finite() {
        return None;
    }
    if is_zero_decimal_currency(currency) {
        let rounded = amount_major.round();
        if rounded > i64::MAX as f64 || rounded < i64::MIN as f64 {
            return None;
        }
        Some(rounded as i64)
    } else {
        unified_core::money::major_to_minor_checked(amount_major)
    }
}

/// Convert integer minor units to major units, for display edges
/// only (formatted prices, receipts). Wire/hook payloads carry
/// integer minor units; do not use this to build payloads.
pub(crate) fn minor_to_major(amount_minor: i64, currency: &str) -> f64 {
    if is_zero_decimal_currency(currency) {
        amount_minor as f64
    } else {
        round_money(amount_minor as f64 / 100.0)
    }
}

pub(crate) fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

/// Convert pricing scaled units (exponent 4) back to integer cents
/// with half-up rounding: the store boundary conversion, mirroring
/// the pricing module's own helper without calling across modules
/// (this module interconnects only through the hook bus).
pub(crate) fn scaled_to_cents(scaled: i64) -> i64 {
    if scaled >= 0 {
        (scaled + 50) / 100
    } else {
        (scaled - 50) / 100
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;

    /// Map a stable test name to a fixed UUID, so a test user keeps
    /// one identity everywhere: a UUID string parses as-is, any
    /// other name hashes (FNV-1a folded into the low 64 bits, with
    /// the version/variant bits set so the result is a valid v4
    /// UUID). Two different names never map to the same UUID. The
    /// "guest" pseudo-owner is NOT a user and keeps its literal.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        if let Ok(parsed) = uuid::Uuid::parse_str(name) {
            return parsed;
        }
        let mut hasher = DefaultHasher::new();
        name.hash(&mut hasher);
        let low = hasher.finish();
        uuid::Uuid::from_u128(
            ((low as u128) << 64) | 0x0000_0000_0000_4000_8000_0000_0000_0000,
        )
    }

    /// The owner id a seeded record carries for `user`: the verified
    /// caller's UUID, except for the "guest" pseudo-owner, which
    /// stays literal (`caller_owns` lets anonymous callers see
    /// guest-owned records only).
    fn owner_id_for(user: &str) -> String {
        if user == "guest" {
            "guest".to_string()
        } else {
            test_user_uuid(user).to_string()
        }
    }

    fn request_as(user: Option<&str>) -> HttpRequest {
        let req = TestRequest::default().to_http_request();
        if let Some(user) = user {
            use actix_web::HttpMessage;
            req.extensions_mut()
                .insert(unified_core::auth::Caller::verified(
                    test_user_uuid(user),
                    Vec::new(),
                ));
        }
        req
    }

    fn order_owned_by(user: &str) -> Order {
        let timestamp = chrono::Utc::now();
        Order {
            id: "ord-1".to_string(),
            reference: "ORD-1".to_string(),
            user_id: owner_id_for(user),
            items: Vec::new(),
            subtotal: 0,
            discount_total: 0,
            total: 0,
            currency: "USD".to_string(),
            coupon_code: None,
            discount_evaluation: None,
            company_id: None,
            trial_until: None,
            status: "pending".to_string(),
            provider: None,
            payment_reference: None,
            created_at: timestamp,
            updated_at: timestamp,
        }
    }

    fn entitlement_owned_by(user: &str) -> DownloadEntitlement {
        let timestamp = chrono::Utc::now();
        DownloadEntitlement {
            id: "ent-1".to_string(),
            license_id: Some("lic-1".to_string()),
            license_key: None,
            order_id: "ord-1".to_string(),
            user_id: owner_id_for(user),
            product_id: "prod-1".to_string(),
            tier_id: None,
            software_id: None,
            software_slug: None,
            release_id: None,
            download_limit: 5,
            downloads_used: 0,
            status: "active".to_string(),
            expires_at: None,
            created_at: timestamp,
            updated_at: timestamp,
        }
    }

    #[test]
    fn require_order_owner_allows_owner_and_rejects_stranger() {
        let order = order_owned_by("user-1");
        assert!(require_order_owner(&request_as(Some("user-1")), &order).is_ok());
        assert!(require_order_owner(&request_as(Some("user-2")), &order).is_err());
        assert!(require_order_owner(&request_as(None), &order).is_err());
        assert!(require_order_owner(&request_as(None), &order_owned_by("guest")).is_ok());
    }

    #[test]
    fn require_entitlement_owner_allows_owner_and_rejects_stranger() {
        let entitlement = entitlement_owned_by("user-1");
        assert!(require_entitlement_owner(&request_as(Some("user-1")), &entitlement).is_ok());
        assert!(require_entitlement_owner(&request_as(Some("user-2")), &entitlement).is_err());
        assert!(require_entitlement_owner(&request_as(None), &entitlement).is_err());
    }

    #[test]
    fn record_visible_to_caller_scopes_lists() {
        let stranger = request_as(Some("user-2"));
        let owner = request_as(Some("user-1"));
        let anonymous = request_as(None);
        let user_1 = test_user_uuid("user-1").to_string();
        assert!(record_visible_to_caller(&stranger, &user_1, true));
        assert!(!record_visible_to_caller(&stranger, &user_1, false));
        assert!(record_visible_to_caller(&owner, &user_1, false));
        assert!(!record_visible_to_caller(&anonymous, &user_1, false));
        assert!(record_visible_to_caller(&anonymous, "guest", false));
    }

    #[test]
    fn round_money_rounds_to_cents() {
        assert_eq!(round_money(10.006), 10.01);
        assert_eq!(round_money(10.004), 10.0);
        assert_eq!(round_money(2.5), 2.5);
        assert_eq!(round_money(-1.234), -1.23);
    }

    #[test]
    fn paginate_returns_the_requested_page() {
        let params = PaginationParams {
            page: Some(2),
            per_page: Some(2),
            ..Default::default()
        };
        let page = paginate(vec![1, 2, 3, 4, 5], &params);
        assert_eq!(page.items, vec![3, 4]);
        assert_eq!(page.pagination.total, 5);
        assert_eq!(page.pagination.total_pages, 3);
    }

    #[test]
    fn paginate_returns_empty_items_past_the_end() {
        let params = PaginationParams {
            page: Some(9),
            per_page: Some(2),
            ..Default::default()
        };
        let page = paginate(vec![1, 2, 3], &params);
        assert!(page.items.is_empty());
        assert_eq!(page.pagination.total, 3);
    }

    #[test]
    fn matches_search_is_case_insensitive() {
        let params = PaginationParams {
            search: Some("world".to_string()),
            ..Default::default()
        };
        assert!(matches_search("Hello World", &params));
        let params = PaginationParams {
            search: Some("missing".to_string()),
            ..Default::default()
        };
        assert!(!matches_search("Hello World", &params));
        assert!(matches_search("Hello World", &PaginationParams::default()));
        let params = PaginationParams {
            search: Some("   ".to_string()),
            ..Default::default()
        };
        assert!(matches_search("Hello World", &params));
    }

    #[test]
    fn zero_decimal_currencies_have_no_minor_units() {
        assert!(is_zero_decimal_currency("JPY"));
        assert!(is_zero_decimal_currency("KRW"));
        assert!(!is_zero_decimal_currency("USD"));
        assert!(!is_zero_decimal_currency("EUR"));
    }

    #[test]
    fn major_minor_conversions_round_trip() {
        assert_eq!(major_to_minor(10.25, "USD"), Some(1025));
        assert_eq!(major_to_minor(10.0, "JPY"), Some(10));
        assert_eq!(minor_to_major(1025, "USD"), 10.25);
        assert_eq!(minor_to_major(1025, "JPY"), 1025.0);
    }

    #[test]
    fn major_to_minor_rejects_overflow_and_non_finite() {
        assert_eq!(major_to_minor(1e17, "USD"), None);
        assert_eq!(major_to_minor(f64::MAX, "USD"), None);
        assert_eq!(major_to_minor(f64::NAN, "USD"), None);
        assert_eq!(major_to_minor(f64::INFINITY, "JPY"), None);
        assert_eq!(major_to_minor(1e19, "JPY"), None);
    }

    #[test]
    fn apply_ratio_to_minor_rounds_half_away_from_zero() {
        assert_eq!(apply_ratio_to_minor(105, 0.10), 11);
        assert_eq!(apply_ratio_to_minor(-105, 0.10), -11);
        assert_eq!(apply_ratio_to_minor(20000, 0.10), 2000);
    }

    #[test]
    fn div_rounded_rounds_half_away_from_zero() {
        assert_eq!(div_rounded(5, 2), 3);
        assert_eq!(div_rounded(-5, 2), -3);
        assert_eq!(div_rounded(2, 3), 1);
        assert_eq!(div_rounded(10, 0), 0);
    }

    #[test]
    fn line_total_for_multiplies_without_rounding() {
        assert_eq!(line_total_for(1025, 3), 3075);
        assert_eq!(line_total_for(1025, 0), 0);
    }

    #[test]
    fn scaled_to_cents_rounds_half_up() {
        assert_eq!(scaled_to_cents(10050), 101);
        assert_eq!(scaled_to_cents(10049), 100);
        assert_eq!(scaled_to_cents(-10050), -101);
        assert_eq!(scaled_to_cents(0), 0);
    }

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach, plus a verified
    /// caller (optionally with a server-resolved company).
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        use actix_web::HttpMessage;
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
        let mut caller = unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new());
        if let Some(company) = caller_company {
            caller = caller.with_company(company.to_string());
        }
        req.extensions_mut().insert(caller);
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn caller_company_prefers_tenant_company() {
        let req = tenant_request("company-1", Some("company-1"));
        let tenant = require_tenant(&req).expect("tenant resolves");
        assert_eq!(
            caller_company(&tenant, &req).unwrap().as_deref(),
            Some("company-1")
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request("company-1", Some("company-2"));
        let tenant = require_tenant(&req).expect("tenant resolves");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn require_caller_company_refuses_writes_without_a_resolved_company() {
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant resolves");
        assert!(require_caller_company(&tenant, &req).is_err());
        let req = tenant_request("company-1", None);
        let tenant = require_tenant(&req).expect("tenant resolves");
        assert_eq!(require_caller_company(&tenant, &req).unwrap(), "company-1");
    }

    #[test]
    fn in_company_scope_is_strict_for_scoped_and_unresolved_callers() {
        let scope = Some("company-1".to_string());
        assert!(in_company_scope(&Some("company-1".to_string()), &scope));
        assert!(!in_company_scope(&Some("company-2".to_string()), &scope));
        assert!(!in_company_scope(&None, &scope));
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some("company-1".to_string()), &None));
    }

    #[test]
    fn require_company_match_adopts_scope_and_refuses_other_companies() {
        let req = tenant_request("company-1", None);
        let tenant = require_tenant(&req).expect("tenant resolves");
        assert_eq!(
            require_company_match(&tenant, &req, &None).unwrap(),
            Some("company-1".to_string())
        );
        assert_eq!(
            require_company_match(&tenant, &req, &Some("company-1".to_string())).unwrap(),
            Some("company-1".to_string())
        );
        assert!(require_company_match(&tenant, &req, &Some("company-2".to_string())).is_err());
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant resolves");
        assert!(require_company_match(&tenant, &req, &Some("company-1".to_string())).is_err());
        assert_eq!(require_company_match(&tenant, &req, &None).unwrap(), None);
    }

    #[test]
    fn require_record_in_company_scope_forbids_other_companies() {
        let scope = Some("company-1".to_string());
        assert!(require_record_in_company_scope(&Some("company-1".to_string()), &scope).is_ok());
        assert!(require_record_in_company_scope(&Some("company-2".to_string()), &scope).is_err());
    }
}
