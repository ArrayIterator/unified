//! billing - helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::json;
use super::types::*;
use super::state::*;

/// Round a major-unit float to cents. Boundary helper only: the
/// value is converted to minor units immediately afterwards by
/// `minor_to_major`'s callers or by `major_to_minor` (see
/// `school_fees_types`). Business logic never does float money
/// math; amounts are `i64` minor units everywhere else.
pub(crate) fn round_money(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// True for zero-decimal currencies (no minor units), matching the
/// payments module's list.
pub(crate) fn is_zero_decimal_currency(currency: &str) -> bool {
    matches!(
        currency,
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "UGX" | "XAF" | "XOF" | "XPF"
    )
}

/// Convert integer minor units to major units, as JSON and hook
/// payloads carry them. This is the boundary conversion out of the
/// integer representation; business logic keeps minor units.
pub(crate) fn minor_to_major(amount_minor: i64, currency: &str) -> f64 {
    if is_zero_decimal_currency(currency) {
        amount_minor as f64
    } else {
        round_money(amount_minor as f64 / 100.0)
    }
}

/// Apply a ratio (a tax rate, a percentage, a day share) to a
/// minor-unit amount, rounding half away from zero to minor units.
/// Rates are `f64` fractions by design, so the multiplication
/// itself stays float; the result is rounded exactly once to minor
/// units, which is the same rounding `round_money` applied to the
/// old float math (`round(amount_major * rate)` in cents equals
/// `round(amount_minor * rate)` in minor units).
pub(crate) fn apply_ratio_to_minor(amount_minor: i64, ratio: f64) -> i64 {
    (amount_minor as f64 * ratio).round() as i64
}

/// Divide a minor-unit amount by a divisor, rounding half away
/// from zero to minor units. Used for the tax-inclusive split,
/// where the net is `amount / (1 + rate)`.
pub(crate) fn div_ratio_to_minor(amount_minor: i64, divisor: f64) -> i64 {
    (amount_minor as f64 / divisor).round() as i64
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

/// Caller identities from the server-verified caller: the verified
/// user id when authenticated, nothing when anonymous. See the store
/// module: this is an ownership check on verified identity, not
/// authentication. A caller with no verified identity matches only
/// `guest`-owned records.
pub(crate) fn caller_identities(req: &HttpRequest) -> Vec<String> {
    let verified = unified_core::caller::caller_from(req);
    if verified.is_authenticated() {
        vec![verified.user_id]
    } else {
        Vec::new()
    }
}

/// True when the caller may act on a record owned by `owner`.
pub(crate) fn caller_owns(req: &HttpRequest, owner: &str) -> bool {
    let identities = caller_identities(req);
    if identities.is_empty() {
        return owner == "guest";
    }
    identities.iter().any(|identity| identity == owner)
}

/// Reject a mutating invoice call from a caller who does not own the
/// invoice (see `caller_owns`).
pub(crate) fn require_invoice_owner(req: &HttpRequest, invoice: &Invoice) -> UnifiedResult<()> {
    if caller_owns(req, &invoice.user_id) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Invoice belongs to a different user",
        ))
    }
}

/// Caller identity for the billing admin routes (`X-User-Id`
/// required, plus `X-Role` required for capability checks), the
/// same shape as the school-fees module.
pub(crate) struct BillingCaller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn billing_caller(req: &HttpRequest) -> UnifiedResult<BillingCaller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(BillingCaller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn billing_check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn billing_require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if billing_check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Authorize one request: caller identity plus one capability.
/// Returns the caller on success; 401 when an identity header is
/// missing, 403 when the role does not hold the permission.
pub(crate) fn billing_authorize(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<BillingCaller> {
    let found = billing_caller(req)?;
    billing_require_permission(ctx, &found.role, permission)?;
    Ok(found)
}

/// Split one line amount into (net amount, tax amount) for a tax rate
/// given as a fraction (0.10 means 10%). When tax is inclusive the
/// amount already contains the tax; otherwise the tax is added on top.
/// Amounts are integer minor units; the rate application rounds half
/// away from zero to minor units, exactly like the old `round_money`
/// float math.
pub(crate) fn line_tax_on_amount(amount: i64, tax_rate: f64, tax_inclusive: bool) -> (i64, i64) {
    if tax_inclusive {
        let net = div_ratio_to_minor(amount, 1.0 + tax_rate);
        (net, amount - net)
    } else {
        (amount, apply_ratio_to_minor(amount, tax_rate))
    }
}

/// Split `unit_price * quantity` into (net amount, tax amount).
pub(crate) fn line_tax(unit_price: i64, quantity: u32, tax_rate: f64, tax_inclusive: bool) -> (i64, i64) {
    line_tax_on_amount(unit_price.saturating_mul(i64::from(quantity)), tax_rate, tax_inclusive)
}

/// True when an invoice is past its due date with a balance still
/// outstanding. Paid, void and credited invoices are never overdue.
pub(crate) fn is_overdue(invoice: &Invoice, at: DateTime<Utc>) -> bool {
    matches!(
        invoice.status.as_str(),
        "issued" | "partially_paid" | "unpaid"
    ) && invoice.balance > 0
        && invoice.due_date.is_some_and(|due| due < at)
}

/// The status callers see: an unpaid invoice past its due date reads
/// as `overdue`. The stored status is left untouched.
pub(crate) fn effective_status(invoice: &Invoice, at: DateTime<Utc>) -> String {
    if is_overdue(invoice, at) {
        "overdue".to_string()
    } else {
        invoice.status.clone()
    }
}

/// A copy of an invoice with the derived `overdue` flag and effective
/// status filled in. All read endpoints return this view.
pub(crate) fn view_invoice(invoice: &Invoice) -> Invoice {
    let mut view = invoice.clone();
    view.overdue = is_overdue(invoice, now());
    if view.overdue {
        view.status = "overdue".to_string();
    }
    view
}

/// Recompute the outstanding balance from the totals. The balance is
/// never negative: overpayment and over-crediting are rejected before
/// they can happen.
pub(crate) fn recompute_balance(invoice: &mut Invoice) {
    invoice.balance = invoice
        .total
        .saturating_sub(invoice.amount_paid)
        .saturating_sub(invoice.credit_total)
        .max(0);
}

/// Reminder offsets that fall due today for one invoice, in days
/// relative to the due date (positive = days before due, negative =
/// days after due). Offsets already sent are excluded.
pub(crate) fn reminders_due(invoice: &Invoice, settings: &BillingSettings, at: DateTime<Utc>) -> Vec<i32> {
    let Some(due) = invoice.due_date else {
        return Vec::new();
    };
    if invoice.balance <= 0
        || matches!(invoice.status.as_str(), "paid" | "void" | "credited")
    {
        return Vec::new();
    }
    let today = at.date_naive();
    let due_day = due.date_naive();
    let mut due_offsets: Vec<i32> = Vec::new();
    for &days in &settings.reminder_before_days {
        if days > 0 && today == due_day - Duration::days(i64::from(days)) {
            due_offsets.push(days);
        }
    }
    for &days in &settings.reminder_after_days {
        if days > 0 && today == due_day + Duration::days(i64::from(days)) {
            due_offsets.push(-days);
        }
    }
    due_offsets.retain(|offset| !invoice.reminders_sent.contains(offset));
    due_offsets
}

/// Resolve the total for a new invoice: when the caller supplies
/// one it must equal the computed subtotal + tax total, otherwise
/// the stored lines would not sum to the stored total. Callers
/// that omit the total get the computed total. Amounts are integer
/// minor units; the mismatch message renders them in major units.
pub(crate) fn resolve_invoice_total(
    supplied: Option<i64>,
    computed: i64,
    currency: &str,
) -> UnifiedResult<i64> {
    match supplied {
        None => Ok(computed),
        Some(supplied) => {
            if supplied < 0 {
                return Err(UnifiedError::bad_request(
                    "Invoice total must be a non-negative number",
                ));
            }
            if supplied != computed {
                return Err(UnifiedError::bad_request(format!(
                    "Invoice total {:.2} does not match the computed total {:.2} from the invoice lines",
                    minor_to_major(supplied, currency),
                    minor_to_major(computed, currency)
                )));
            }
            Ok(supplied)
        }
    }
}

/// Reject a payment against an invoice that must not be paid: a
/// draft has never been issued (issuing is the pricing
/// reconciliation enforcement point, so paying a draft would
/// bypass reconciliation), and paid / void / credited /
/// partially-refunded invoices have no payable balance left.
pub(crate) fn check_payment_allowed(invoice: &Invoice) -> UnifiedResult<()> {
    match invoice.status.as_str() {
        "draft" => Err(UnifiedError::unprocessable(
            "A draft invoice must be issued before it can be paid",
        )),
        "paid" => Err(UnifiedError::conflict("Invoice is already paid")),
        "void" => Err(UnifiedError::unprocessable(
            "A void invoice cannot be paid",
        )),
        "credited" => Err(UnifiedError::unprocessable(
            "A credited invoice cannot be paid",
        )),
        "partially_refunded" => Err(UnifiedError::conflict(
            "A partially refunded invoice has no outstanding balance to pay",
        )),
        _ => Ok(()),
    }
}

/// Reject voiding an invoice that must not be voided: a paid
/// invoice, an already-void one, or one with recorded payments
/// still on the books (voiding it would leave payments with no
/// invoice and no reversal; refund or credit the payments first).
pub(crate) fn check_void_allowed(invoice: &Invoice) -> UnifiedResult<()> {
    if invoice.status == "paid" {
        return Err(UnifiedError::unprocessable(
            "A paid invoice cannot be voided",
        ));
    }
    if invoice.status == "void" {
        return Err(UnifiedError::conflict("Invoice is already void"));
    }
    if invoice.amount_paid > 0 {
        return Err(UnifiedError::unprocessable(
            "An invoice with recorded payments cannot be voided; refund or credit the payments first",
        ));
    }
    Ok(())
}

/// Largest credit note an invoice may receive. An invoice that is
/// paid (or partially refunded) has no outstanding balance, so its
/// credit is capped at what was paid -- the refund of paid money,
/// mirroring the `payment.refunded` hook. Any other invoice is
/// capped at its outstanding balance.
pub(crate) fn credit_note_cap(invoice: &Invoice) -> i64 {
    if matches!(invoice.status.as_str(), "paid" | "partially_refunded") {
        invoice.amount_paid
    } else {
        invoice.balance
    }
}

/// Split a captured amount into the part applied to the invoice
/// balance and the overpayment excess. The excess is never silently
/// discarded: the caller records it (credit note plus an
/// `invoice.overpayment` event) instead of clamping it away.
/// Amounts are integer minor units.
pub(crate) fn split_capture(captured: i64, balance: i64) -> (i64, i64) {
    let applied = captured.min(balance).max(0);
    let excess = captured.saturating_sub(applied).max(0);
    (applied, excess)
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

pub(crate) fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub(crate) fn render_invoice_html(invoice: &Invoice) -> String {
    let mut rows = String::new();
    for item in &invoice.items {
        rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{:.2}</td><td>{:.2}</td><td>{:.2}</td></tr>",
            escape_html(&item.name),
            item.quantity,
            minor_to_major(item.unit_price, &invoice.currency),
            minor_to_major(item.tax_amount, &invoice.currency),
            minor_to_major(item.line_total, &invoice.currency)
        ));
    }
    let due = invoice
        .due_date
        .map(|due| due.to_rfc3339())
        .unwrap_or_else(|| "-".to_string());
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Invoice {}</title></head>\
        <body><h1>Invoice {}</h1>\
        <p>Status: {}<br>User: {}<br>Currency: {}<br>Due date: {}<br>\
        Subtotal: {:.2}<br>Tax: {:.2}<br>Total: {:.2}<br>Paid: {:.2}<br>Balance: {:.2}</p>\
        <table><thead><tr><th>Item</th><th>Quantity</th><th>Unit price</th><th>Tax</th><th>Total</th></tr></thead>\
        <tbody>{}</tbody></table></body></html>",
        escape_html(&invoice.number),
        escape_html(&invoice.number),
        escape_html(&invoice.status),
        escape_html(&invoice.user_id),
        escape_html(&invoice.currency),
        escape_html(&due),
        minor_to_major(invoice.subtotal, &invoice.currency),
        minor_to_major(invoice.tax_total, &invoice.currency),
        minor_to_major(invoice.total, &invoice.currency),
        minor_to_major(invoice.amount_paid, &invoice.currency),
        minor_to_major(invoice.balance, &invoice.currency),
        rows
    )
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 5, group A).
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

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
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

/// Verify a request body's company id against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company stores
/// a global row.
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

/// Verify a newly named company on an update body
/// (`Option<Option<String>>`): `Some(id)` must equal the caller
/// scope (403 otherwise; an unresolved caller may not name a
/// company); `None` is an explicit clear and stays a clear, never
/// re-stamped with the caller scope. The outer `None` (field
/// absent) is handled by the caller leaving the stored value
/// untouched.
pub(crate) fn require_company_change(
    scope: &Option<String>,
    new: &Option<String>,
) -> UnifiedResult<Option<String>> {
    match (new, scope) {
        (Some(id), Some(scope)) if id == scope => Ok(Some(id.clone())),
        (Some(_), _) => Err(UnifiedError::forbidden(
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
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => record_company_id.as_deref() == Some(company.as_str()),
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope` as a filter instead.
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

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::HttpMessage;
    use actix_web::test::TestRequest;

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
    fn in_company_scope_hides_unscoped_records_from_scoped_callers() {
        let scope = Some("acme".to_string());
        assert!(in_company_scope(&Some("acme".to_string()), &scope));
        assert!(!in_company_scope(&Some("other".to_string()), &scope));
        assert!(!in_company_scope(&None, &scope));
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some("other".to_string()), &None));
    }

    #[test]
    fn require_company_match_for_refuses_body_company_from_unresolved_callers() {
        let body = Some("c1".to_string());
        assert!(require_company_match_for(&None, &body).is_err());
        assert!(require_company_match_for(&None, &None).unwrap().is_none());
        assert_eq!(
            require_company_match_for(&Some("c1".to_string()), &body).unwrap(),
            Some("c1".to_string())
        );
        assert!(require_company_match_for(&Some("c2".to_string()), &body).is_err());
    }

    #[test]
    fn require_company_change_keeps_an_explicit_clear_a_clear() {
        let scope = Some("c1".to_string());
        assert_eq!(require_company_change(&scope, &None).unwrap(), None);
        assert_eq!(
            require_company_change(&scope, &Some("c1".to_string())).unwrap(),
            Some("c1".to_string())
        );
        assert!(require_company_change(&scope, &Some("c2".to_string())).is_err());
        assert!(require_company_change(&None, &Some("c1".to_string())).is_err());
    }
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
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

    use super::*;
    use chrono::Duration;
    use std::sync::Arc;

    fn test_invoice(status: &str, balance: i64, due: Option<DateTime<Utc>>) -> Invoice {
        Invoice {
            id: "inv-1".to_string(),
            number: "INV-000001".to_string(),
            reference: String::new(),
            order_id: None,
            subscription_id: None,
            user_id: "user-1".to_string(),
            company_id: None,
            items: Vec::new(),
            subtotal: 10000,
            tax_total: 0,
            total: 10000,
            currency: "USD".to_string(),
            status: status.to_string(),
            overdue: false,
            tax_inclusive: false,
            amount_paid: 10000 - balance,
            credit_total: 0,
            balance,
            issue_date: None,
            due_date: due,
            payment_terms_days: None,
            paid_at: None,
            notes: None,
            reminders_sent: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn round_money_rounds_half_cents() {
        assert_eq!(round_money(10.126), 10.13);
        assert_eq!(round_money(10.124), 10.12);
    }

    #[test]
    fn minor_to_major_converts_cents_and_zero_decimal_currencies() {
        assert_eq!(minor_to_major(10050, "USD"), 100.5);
        assert_eq!(minor_to_major(500, "JPY"), 500.0);
    }

    #[test]
    fn is_zero_decimal_currency_matches_jpy_only() {
        assert!(is_zero_decimal_currency("JPY"));
        assert!(!is_zero_decimal_currency("USD"));
    }

    #[test]
    fn line_tax_exclusive_and_inclusive() {
        assert_eq!(line_tax(10000, 2, 0.10, false), (20000, 2000));
        assert_eq!(line_tax(11000, 1, 0.10, true), (10000, 1000));
    }

    #[test]
    fn line_tax_rounds_half_cents_away_from_zero() {
        // 105 minor at 10% is 10.5 minor: the old float math
        // rounded 0.105 major to 0.11, i.e. 11 minor units.
        assert_eq!(line_tax_on_amount(105, 0.10, false), (105, 11));
        assert_eq!(line_tax_on_amount(-105, 0.10, false), (-105, -11));
        // 7.5% on 1000 minor is exactly 75 minor units.
        assert_eq!(line_tax_on_amount(1000, 0.075, false), (1000, 75));
    }

    #[test]
    fn div_rounded_rounds_half_away_from_zero() {
        assert_eq!(div_rounded(5, 2), 3);
        assert_eq!(div_rounded(-5, 2), -3);
        assert_eq!(div_rounded(4, 2), 2);
        assert_eq!(div_rounded(1, 3), 0);
        assert_eq!(div_rounded(2, 3), 1);
        assert_eq!(div_rounded(10, 0), 0);
    }

    #[test]
    fn apply_ratio_to_minor_rounds_half_away_from_zero() {
        assert_eq!(apply_ratio_to_minor(105, 0.10), 11);
        assert_eq!(apply_ratio_to_minor(-105, 0.10), -11);
        assert_eq!(apply_ratio_to_minor(10000, 0.10), 1000);
    }

    #[test]
    fn is_overdue_true_for_open_invoice_past_due_with_balance() {
        let invoice = test_invoice("issued", 10000, Some(Utc::now() - Duration::days(1)));
        assert!(is_overdue(&invoice, Utc::now()));
    }

    #[test]
    fn is_overdue_false_for_paid_invoice() {
        let invoice = test_invoice("paid", 0, Some(Utc::now() - Duration::days(1)));
        assert!(!is_overdue(&invoice, Utc::now()));
    }

    #[test]
    fn is_overdue_false_without_due_date() {
        let invoice = test_invoice("issued", 10000, None);
        assert!(!is_overdue(&invoice, Utc::now()));
    }

    #[test]
    fn is_overdue_false_with_zero_balance() {
        let invoice = test_invoice("issued", 0, Some(Utc::now() - Duration::days(1)));
        assert!(!is_overdue(&invoice, Utc::now()));
    }

    #[test]
    fn effective_status_reports_overdue_for_past_due_invoice() {
        let invoice = test_invoice("issued", 10000, Some(Utc::now() - Duration::days(1)));
        assert_eq!(effective_status(&invoice, Utc::now()), "overdue");
        let current = test_invoice("issued", 10000, Some(Utc::now() + Duration::days(1)));
        assert_eq!(effective_status(&current, Utc::now()), "issued");
    }

    #[test]
    fn recompute_balance_subtracts_payments_and_credits_and_clamps_at_zero() {
        let mut invoice = test_invoice("issued", 10000, None);
        invoice.amount_paid = 3000;
        invoice.credit_total = 1000;
        recompute_balance(&mut invoice);
        assert_eq!(invoice.balance, 6000);
        invoice.amount_paid = 15000;
        recompute_balance(&mut invoice);
        assert_eq!(invoice.balance, 0);
    }

    #[test]
    fn is_overdue_false_for_draft_invoice() {
        let invoice = test_invoice("draft", 10000, Some(Utc::now() - Duration::days(1)));
        assert!(!is_overdue(&invoice, Utc::now()));
    }

    #[test]
    fn resolve_invoice_total_accepts_omitted_and_matching_totals() {
        assert_eq!(resolve_invoice_total(None, 12000, "USD").unwrap(), 12000);
        assert_eq!(
            resolve_invoice_total(Some(12000), 12000, "USD").unwrap(),
            12000
        );
    }

    #[test]
    fn resolve_invoice_total_rejects_a_supplied_total_that_differs() {
        assert!(resolve_invoice_total(Some(9900), 12000, "USD").is_err());
        assert!(resolve_invoice_total(Some(-500), 12000, "USD").is_err());
    }

    #[test]
    fn check_payment_allowed_rejects_draft_and_terminal_statuses() {
        for status in ["draft", "paid", "void", "credited", "partially_refunded"] {
            assert!(
                check_payment_allowed(&test_invoice(status, 10000, None)).is_err(),
                "{status} must not be payable"
            );
        }
        assert!(check_payment_allowed(&test_invoice("issued", 10000, None)).is_ok());
        assert!(
            check_payment_allowed(&test_invoice("partially_paid", 5000, None)).is_ok()
        );
    }

    #[test]
    fn check_void_allowed_rejects_paid_and_partially_paid_invoices() {
        assert!(check_void_allowed(&test_invoice("paid", 0, None)).is_err());
        assert!(check_void_allowed(&test_invoice("void", 0, None)).is_err());
        let mut partially_paid = test_invoice("partially_paid", 5000, None);
        partially_paid.amount_paid = 5000;
        assert!(check_void_allowed(&partially_paid).is_err());
        assert!(check_void_allowed(&test_invoice("issued", 10000, None)).is_ok());
    }

    #[test]
    fn credit_note_cap_uses_amount_paid_for_paid_invoices() {
        let mut paid = test_invoice("paid", 0, None);
        paid.amount_paid = 10000;
        assert_eq!(credit_note_cap(&paid), 10000);
        let mut refunded = test_invoice("partially_refunded", 0.0, None);
        refunded.amount_paid = 7000;
        assert_eq!(credit_note_cap(&refunded), 7000);
        assert_eq!(credit_note_cap(&test_invoice("issued", 6000, None)), 6000);
    }

    #[test]
    fn split_capture_separates_applied_amount_from_overpayment() {
        assert_eq!(split_capture(10000, 10000), (10000, 0));
        assert_eq!(split_capture(4000, 10000), (4000, 0));
        assert_eq!(split_capture(15000, 10000), (10000, 5000));
        assert_eq!(split_capture(2500, 0), (0, 2500));
        assert_eq!(split_capture(-500, 10000), (0, 0));
    }

    // Caller authorization for the billing settings routes: a
    // missing identity header is 401, a role without the
    // capability is 403, and no `role.check` answer fails closed.
    fn billing_test_context() -> AppContext {
        use unified_core::config::AppConfig;
        use unified_core::hooks::HookBus;
        use unified_core::modules::ModuleRegistry;
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn billing_authed_request() -> HttpRequest {
        use actix_web::test::TestRequest;
        verified_request("user-1", "admin")
    }

    fn billing_allow_all_role_checks(ctx: &AppContext) {
        use unified_core::hooks::HookContext;
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
    }

    #[test]
    fn billing_caller_accepts_a_verified_caller() {
        let found = billing_caller(&billing_authed_request()).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn billing_caller_rejects_a_request_without_a_verified_caller() {
        use actix_web::test::TestRequest;
        let req = TestRequest::default().to_http_request();
        let error = billing_caller(&req).unwrap_err();
        assert_eq!(error.code, 401);
    }

    #[test]
    fn billing_caller_without_a_resolved_role_is_anonymous() {
        // A verified caller with no role resolved holds the anonymous
        // role: the identity resolves, the capability check is what
        // refuses it.
        let req = verified_request_without_roles("user-1");
        let found = billing_caller(&req).expect("a verified caller resolves without a role");
        assert_eq!(found.role, "anonymous");
    }

    #[test]
    fn billing_authorize_fails_closed_without_a_role_check_handler() {
        let ctx = billing_test_context();
        assert!(!billing_check_capability(&ctx, "admin", PERM_BILLING_SETTINGS_VIEW));
        assert!(!billing_check_capability(&ctx, "admin", PERM_BILLING_SETTINGS_MANAGE));
        let view = billing_authorize(&ctx, &billing_authed_request(), PERM_BILLING_SETTINGS_VIEW)
            .unwrap_err();
        assert_eq!(view.code, 403);
        let manage =
            billing_authorize(&ctx, &billing_authed_request(), PERM_BILLING_SETTINGS_MANAGE)
                .unwrap_err();
        assert_eq!(manage.code, 403);
    }

    #[test]
    fn billing_authorize_rejects_a_request_without_a_verified_caller() {
        use actix_web::test::TestRequest;
        let ctx = billing_test_context();
        billing_allow_all_role_checks(&ctx);
        let req = TestRequest::default().to_http_request();
        let error = billing_authorize(&ctx, &req, PERM_BILLING_SETTINGS_VIEW).unwrap_err();
        assert_eq!(error.code, 401);
    }

    #[test]
    fn billing_authorize_accepts_when_role_check_allows() {
        let ctx = billing_test_context();
        billing_allow_all_role_checks(&ctx);
        let found =
            billing_authorize(&ctx, &billing_authed_request(), PERM_BILLING_SETTINGS_VIEW)
                .unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
        assert!(billing_authorize(&ctx, &billing_authed_request(), PERM_BILLING_SETTINGS_MANAGE)
            .is_ok());
    }
}
