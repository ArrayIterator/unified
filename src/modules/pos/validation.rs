// Pos module - money, discount, tax, and loader helpers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use uuid::Uuid;
use super::*;

// Helpers

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

/// True for zero-decimal currencies (no minor units), matching the
/// billing and store modules' list.
pub(crate) fn is_zero_decimal_currency(currency: &str) -> bool {
    unified_core::db::money::is_zero_decimal_currency(currency)
}

/// Convert major units to integer minor units for `currency`
/// (half away from zero). Boundary conversion for fixed-amount
/// inputs that arrive as major-unit floats.
pub(crate) fn major_to_minor(amount_major: f64, currency: &str) -> i64 {
    if is_zero_decimal_currency(currency) {
        amount_major.round() as i64
    } else {
        (amount_major * 100.0).round() as i64
    }
}

/// Convert integer minor units to major units, as JSON, hook
/// payloads, and human-readable receipts carry them. This is the
/// boundary conversion out of the integer representation; business
/// logic keeps minor units.
pub(crate) fn minor_to_major(amount_minor: i64, currency: &str) -> f64 {
    if is_zero_decimal_currency(currency) {
        amount_minor as f64
    } else {
        let major = amount_minor as f64 / 100.0;
        (major * 100.0).round() / 100.0
    }
}

/// Apply a percentage (0..=100) to a minor-unit amount, rounding
/// half away from zero to minor units.
pub(crate) fn apply_percent_to_minor(amount_minor: i64, percent: f64) -> i64 {
    (amount_minor as f64 * percent / 100.0).round() as i64
}

/// Apply a ratio (0.0..=1.0) to a minor-unit amount, rounding half
/// away from zero to minor units.
pub(crate) fn apply_ratio_to_minor(amount_minor: i64, ratio: f64) -> i64 {
    (amount_minor as f64 * ratio).round() as i64
}

/// Integer division rounded half away from zero. A zero divisor
/// yields 0.
pub(crate) fn div_rounded(numerator: i64, denominator: i64) -> i64 {
    if denominator == 0 {
        return 0;
    }
    let numerator = numerator as i128;
    let denominator = denominator as i128;
    let sign = if (numerator < 0) != (denominator < 0) { -1 } else { 1 };
    let numerator = numerator.abs();
    let denominator = denominator.abs();
    (sign * ((numerator * 2 + denominator) / (denominator * 2))) as i64
}

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the assets module.
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
/// (same fail-closed shape as the assets module's capability check).
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

// Tenant edge guard (C3 pilot 3).

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
/// - a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources (header trust
/// removal is handled elsewhere).
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company))
            if tenant_company != verified_company =>
        {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

/// Resolve the caller's company for a WRITE (C3, strict by
/// default): a write requires a resolved company scope, so an
/// unresolved caller is refused with 403 instead of silently
/// writing a global (unscoped) record. Mutation handlers call
/// this instead of `caller_company`; read handlers keep
/// `caller_company`, and the named carve-outs (self-service
/// writes, settings with no company field, scan/test triggers,
/// platform surfaces) keep it too.
pub(crate) fn require_company_for_write(
    tenant: &unified_core::tenancy::TenantContext,
    req: &actix_web::HttpRequest,
) -> UnifiedResult<Option<String>> {
    match caller_company(tenant, req)? {
        Some(company) => Ok(Some(company)),
        None => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}


/// Verify a request body's company id against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company is refused (C3):
/// a write requires a resolved company scope.
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
        (None, Some(caller)) => Ok(Some(caller.clone())),
        (None, None) => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}

/// True when a stored record belongs to the caller's company
/// scope. With no caller scope (an unresolved caller) only
/// global records match: unscoped records are visible, every
/// company-owned record is denied (the shared
/// `CallerScope::Unresolved` rule). With a scope, only records
/// carrying that company match, plus unscoped (global/legacy) records,
/// which are shared content visible to any resolved scope (C3, rule A).
pub(crate) fn in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => record_company_id.is_none()
                || record_company_id.as_deref() == Some(company.as_str()),
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

/// Fail closed (403) when a stored record is outside the caller's
/// company scope FOR A WRITE (C3, strict by default).
/// Unlike the read check above (rule A), a write requires an exact
/// company match: a company caller may write only their own
/// company's records - global (unscoped) records are platform-only
/// - and an unresolved caller may write nothing. Mutation handlers
/// call this right after loading the record they mutate; handlers
/// that only read the record, or load a parent to scope a child
/// write, keep the read check.
pub(crate) fn require_record_writable_in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    match (record_company_id, caller_company) {
        (Some(record), Some(caller)) if record == caller => Ok(()),
        _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
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
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
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

pub(crate) fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if trimmed.len() > max {
                return Err(UnifiedError::bad_request(format!(
                    "{field} must be at most {max} characters"
                )));
            }
            Ok(Some(trimmed.to_string()))
        }
    }
}

/// Money: an amount already in minor units (request JSON converted
/// major units through `serde_major` before this runs), non-negative.
pub(crate) fn valid_money(amount: i64, field: &str) -> UnifiedResult<i64> {
    if amount < 0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be zero or greater"
        )));
    }
    Ok(amount)
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

/// Discount type: empty (none), `percent`, or `fixed`.
pub(crate) fn valid_discount_type(value: &Option<String>) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(raw) => {
            let trimmed = raw.trim().to_lowercase();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if !matches!(trimmed.as_str(), "percent" | "fixed") {
                return Err(UnifiedError::bad_request(
                    "discount_type must be one of: percent, fixed",
                ));
            }
            Ok(Some(trimmed))
        }
    }
}

/// Discount amount in minor units for a percent/fixed discount
/// against a minor-unit base amount. Percent is capped at the
/// configured maximum; a fixed discount never exceeds the base
/// amount. `discount_value` stays `f64` by design: it is a percent
/// (0..=100) or a fixed amount in major units, and the fixed case
/// converts through `major_to_minor` with the sale currency.
pub(crate) fn discount_amount(
    discount_type: &Option<String>,
    discount_value: f64,
    base: i64,
    max_percent: f64,
    currency: &str,
) -> UnifiedResult<i64> {
    match discount_type.as_deref() {
        Some("percent") => {
            if !discount_value.is_finite() || discount_value < 0.0 || discount_value > max_percent {
                return Err(UnifiedError::bad_request(format!(
                    "discount percent must be between 0 and {max_percent}"
                )));
            }
            Ok(apply_percent_to_minor(base, discount_value).min(base))
        }
        Some("fixed") => {
            if !discount_value.is_finite() || discount_value < 0.0 {
                return Err(UnifiedError::bad_request(
                    "discount amount must be zero or greater",
                ));
            }
            Ok(major_to_minor(discount_value, currency).min(base))
        }
        _ => Ok(0),
    }
}

/// Tax on a minor-unit amount for a rate given as a fraction. When
/// tax is inclusive the amount already contains the tax; otherwise
/// the tax is added on top. Returns `(net, tax)` in minor units.
/// Rounding matches the old cents rounding: each leg rounds half
/// away from zero to minor units exactly once.
pub(crate) fn tax_on_amount(amount: i64, tax_rate: f64, tax_inclusive: bool) -> (i64, i64) {
    if tax_inclusive {
        let net = (amount as f64 / (1.0 + tax_rate)).round() as i64;
        (net, amount - net)
    } else {
        (amount, (amount as f64 * tax_rate).round() as i64)
    }
}

/// Recompute every derived total of a sale from its lines and its
/// sale-level discount. The sale discount is spread across the
/// lines in proportion to their net amounts; tax is then computed
/// per line on the discounted base, and every total is rounded to
/// cents.
///
/// Returns an error (instead of silently dropping the discount)
/// when the sale-level discount is invalid - for example a percent
/// above the configured maximum. Callers must propagate it so an
/// over-cap discount can never be stored while applying as zero.
pub(crate) fn recompute_sale(sale: &mut PosSale, settings: &PosSettings) -> UnifiedResult<()> {
    let gross_net: i64 = sale
        .items
        .iter()
        .map(|item| item.unit_price.saturating_mul(item.quantity as i64) - item.discount_amount)
        .map(|net| net.max(0))
        .sum();
    let sale_discount = discount_amount(
        &sale.discount_type,
        sale.discount_value,
        gross_net,
        settings.max_discount_percent,
        &sale.currency,
    )?;
    let ratio = if gross_net > 0 {
        (sale_discount as f64 / gross_net as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut subtotal = 0i64;
    let mut tax_total = 0i64;
    for item in sale.items.iter_mut() {
        let net = (item.unit_price.saturating_mul(item.quantity as i64) - item.discount_amount)
            .max(0);
        let discounted_base = apply_ratio_to_minor(net, 1.0 - ratio);
        let (line_net, tax) = tax_on_amount(discounted_base, item.tax_rate, sale.tax_inclusive);
        item.line_subtotal = line_net;
        item.tax_amount = tax;
        item.line_total = line_net + tax;
        subtotal += line_net;
        tax_total += tax;
    }
    sale.subtotal = subtotal;
    sale.discount_total = sale_discount;
    sale.tax_total = tax_total;
    sale.total = subtotal + tax_total;
    Ok(())
}

/// Build one sale line from a request, validating price, quantity,
/// discount, and tax rate.
pub(crate) fn build_sale_item(
    input: &NewSaleItemRequest,
    settings: &PosSettings,
    currency: &str,
) -> UnifiedResult<PosSaleItem> {
    let product_id = require_text(&input.product_id, "product_id", MAX_CODE_LEN)?;
    let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
    let unit_price = valid_money(input.unit_price, "unit_price")?;
    let quantity = input.quantity.unwrap_or(1);
    if quantity == 0 || quantity > 10_000 {
        return Err(UnifiedError::bad_request(
            "quantity must be between 1 and 10000",
        ));
    }
    let discount_type = valid_discount_type(&input.discount_type)?;
    let discount_value = input.discount_value.unwrap_or(0.0);
    let gross = unit_price.saturating_mul(quantity as i64);
    let discount_amount = discount_amount(
        &discount_type,
        discount_value,
        gross,
        settings.max_discount_percent,
        currency,
    )?;
    let tax_rate = input.tax_rate.unwrap_or(settings.default_tax_rate);
    if !tax_rate.is_finite() || tax_rate < 0.0 || tax_rate > 10.0 {
        return Err(UnifiedError::bad_request(
            "tax_rate must be a finite fraction between 0 and 10",
        ));
    }
    let barcode = optional_text(&input.barcode, "barcode", MAX_CODE_LEN)?;
    let sku = optional_text(&input.sku, "sku", MAX_CODE_LEN)?;
    Ok(PosSaleItem {
        id: new_id(),
        product_id,
        name,
        barcode,
        sku,
        unit_price,
        quantity,
        refunded_quantity: 0,
        discount_type,
        discount_value,
        discount_amount,
        line_subtotal: 0,
        tax_rate,
        tax_amount: 0,
        line_total: 0,
    })
}

pub(crate) fn load_register(state: &PosState, id: &str) -> UnifiedResult<PosRegister> {
    state
        .registers
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Register not found: {id}")))
}

pub(crate) fn load_shift(state: &PosState, id: &str) -> UnifiedResult<PosShift> {
    state
        .shifts
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Shift not found: {id}")))
}

pub(crate) fn load_sale(state: &PosState, id: &str) -> UnifiedResult<PosSale> {
    state
        .sales
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Sale not found: {id}")))
}

/// Load a sale and verify it is in the caller scope: company
/// callers read their own and global sales only, unresolved
/// callers read global sales only. A missing sale answers
/// `not_found`; an out-of-scope sale answers `forbidden`, so the
/// check never leaks whether a sale exists in another company.
/// Receipt handlers scope through the parent sale with this
/// loader, since receipts carry no company of their own.
pub(crate) fn require_sale_in_scope(
    state: &PosState,
    id: &str,
    scope: &Option<String>,
) -> UnifiedResult<PosSale> {
    let sale = load_sale(state, id)?;
    require_record_in_company_scope(&sale.company_id, scope)?;
    Ok(sale)
}

pub(crate) fn open_shift_for_register(state: &PosState, register_id: &str) -> Option<PosShift> {
    state
        .shifts
        .iter()
        .map(|entry| entry.value().clone())
        .find(|shift| shift.register_id == register_id && shift.status == "open")
}

/// True when the caller may sell on this register: a manager, or
/// an assigned cashier when assignment is required.
pub(crate) fn caller_may_sell(
    ctx: &AppContext,
    state: &PosState,
    register: &PosRegister,
    caller: &Caller,
) -> UnifiedResult<()> {
    if check_capability(ctx, &caller.role, PERM_POS_MANAGE) {
        return Ok(());
    }
    let settings = state.settings();
    if !settings.require_cashier_assignment {
        return Ok(());
    }
    if register.cashiers.iter().any(|id| id == &caller.actor) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Caller is not an assigned cashier for this register",
        ))
    }
}

/// True when the caller may act on this sale: the sale cashier, or
/// a manager holding `pos.manage`.
pub(crate) fn caller_may_act_on_sale(
    ctx: &AppContext,
    sale: &PosSale,
    caller: &Caller,
) -> UnifiedResult<()> {
    if caller.actor == sale.cashier_id {
        return Ok(());
    }
    if check_capability(ctx, &caller.role, PERM_POS_MANAGE) {
        return Ok(());
    }
    Err(UnifiedError::forbidden(
        "Caller is not the cashier of this sale",
    ))
}

/// Cash the drawer should hold for a shift: opening float plus the
/// cash payments of the completed sales in the shift, minus the
/// cash refunds tied to the shift.
pub(crate) fn expected_cash_for_shift(state: &PosState, shift: &PosShift) -> i64 {
    let cash_method_ids: Vec<String> = state
        .payment_methods
        .iter()
        .filter(|entry| entry.value().is_cash)
        .map(|entry| entry.key().clone())
        .collect();
    let mut expected = shift.opening_float;
    for entry in state.sales.iter() {
        let sale = entry.value();
        if sale.shift_id.as_deref() != Some(shift.id.as_str()) {
            continue;
        }
        if sale.status != "completed"
            && sale.status != "partially_refunded"
            && sale.status != "refunded"
        {
            continue;
        }
        for payment in state.payments.iter() {
            let payment = payment.value();
            if payment.sale_id == sale.id && cash_method_ids.contains(&payment.method_id) {
                expected += payment.amount;
            }
        }
    }
    for entry in state.refunds.iter() {
        let refund = entry.value();
        if refund.shift_id.as_deref() == Some(shift.id.as_str())
            && cash_method_ids.contains(&refund.method_id)
        {
            expected -= refund.amount;
        }
    }
    expected
}

/// Totals of the completed sales in a shift, grouped by payment
/// method code, plus refund and void counts.
pub(crate) fn shift_summary(state: &PosState, shift: &PosShift) -> Value {
    let mut by_method: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut sales_count = 0u64;
    let mut gross = 0i64;
    for entry in state.sales.iter() {
        let sale = entry.value();
        if sale.shift_id.as_deref() != Some(shift.id.as_str()) {
            continue;
        }
        if sale.status == "voided" {
            continue;
        }
        if sale.status != "completed"
            && sale.status != "partially_refunded"
            && sale.status != "refunded"
        {
            continue;
        }
        sales_count += 1;
        gross += sale.total;
        for payment in state.payments.iter() {
            let payment = payment.value();
            if payment.sale_id == sale.id {
                *by_method.entry(payment.method_code.clone()).or_insert(0) += payment.amount;
            }
        }
    }
    let mut refunds_total = 0i64;
    let mut refunds_count = 0u64;
    for entry in state.refunds.iter() {
        let refund = entry.value();
        if refund.shift_id.as_deref() == Some(shift.id.as_str()) {
            refunds_total += refund.amount;
            refunds_count += 1;
        }
    }
    let voids_count = state
        .sales
        .iter()
        .filter(|entry| {
            entry.value().shift_id.as_deref() == Some(shift.id.as_str())
                && entry.value().status == "voided"
        })
        .count() as u64;
    let mut by_method_json: Vec<Value> = by_method
        .into_iter()
        .map(|(method, amount)| {
            json!({ "method": method, "amount": minor_to_major(amount, &shift.currency) })
        })
        .collect();
    by_method_json.sort_by(|a, b| {
        a.get("method")
            .and_then(Value::as_str)
            .cmp(&b.get("method").and_then(Value::as_str))
    });
    json!({
        "shift_id": shift.id,
        "register_id": shift.register_id,
        "cashier_id": shift.cashier_id,
        "status": shift.status,
        "sales_count": sales_count,
        "gross_sales": minor_to_major(gross, &shift.currency),
        "refunds_total": minor_to_major(refunds_total, &shift.currency),
        "refunds_count": refunds_count,
        "voids_count": voids_count,
        "net_sales": minor_to_major(gross - refunds_total, &shift.currency),
        "by_payment_method": by_method_json,
        "opening_float": minor_to_major(shift.opening_float, &shift.currency),
        "expected_cash": shift
            .expected_cash
            .map(|amount| minor_to_major(amount, &shift.currency)),
        "counted_cash": shift
            .counted_cash
            .map(|amount| minor_to_major(amount, &shift.currency)),
        "variance": shift
            .variance
            .map(|amount| minor_to_major(amount, &shift.currency)),
        "currency": shift.currency,
        "opened_at": shift.opened_at,
        "closed_at": shift.closed_at,
    })
}

/// Build the structured print lines for a receipt at the
/// configured width. Plain text lines a receipt printer renders
/// in order; this is receipt data for printing, not a driver.
pub(crate) fn build_print_lines(
    receipt: &PosReceipt,
    settings: &PosSettings,
) -> Vec<ReceiptPrintLine> {
    let width = settings.receipt_width_chars.max(16) as usize;
    let mut lines: Vec<ReceiptPrintLine> = Vec::new();
    let center = |text: &str| ReceiptPrintLine {
        kind: "header".to_string(),
        text: text.to_string(),
        bold: true,
        align: Some("center".to_string()),
    };
    lines.push(center(&receipt.store_name));
    if let Some(address) = &receipt.store_address {
        lines.push(ReceiptPrintLine {
            kind: "text".to_string(),
            text: address.clone(),
            bold: false,
            align: Some("center".to_string()),
        });
    }
    lines.push(ReceiptPrintLine {
        kind: "divider".to_string(),
        text: "-".repeat(width),
        bold: false,
        align: None,
    });
    lines.push(ReceiptPrintLine {
        kind: "text".to_string(),
        text: format!("Receipt {}", receipt.number),
        bold: false,
        align: None,
    });
    lines.push(ReceiptPrintLine {
        kind: "text".to_string(),
        text: format!("Sale {}", receipt.sale_number),
        bold: false,
        align: None,
    });
    lines.push(ReceiptPrintLine {
        kind: "text".to_string(),
        text: format!(
            "Register {}   Cashier {}",
            receipt.register_code.clone().unwrap_or_default(),
            receipt.cashier_id
        ),
        bold: false,
        align: None,
    });
    lines.push(ReceiptPrintLine {
        kind: "text".to_string(),
        text: receipt.issued_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        bold: false,
        align: None,
    });
    lines.push(ReceiptPrintLine {
        kind: "divider".to_string(),
        text: "-".repeat(width),
        bold: false,
        align: None,
    });
    for item in &receipt.items {
        lines.push(ReceiptPrintLine {
            kind: "item".to_string(),
            text: format!("{} x {}", item.quantity, item.name),
            bold: false,
            align: None,
        });
        let amount = format!(
            "{} {:.2}",
            receipt.currency,
            minor_to_major(item.line_total, &receipt.currency)
        );
        let left = format!(
            "  @ {} {:.2} each",
            receipt.currency,
            minor_to_major(item.unit_price, &receipt.currency)
        );
        let padding = width.saturating_sub(left.len() + amount.len());
        lines.push(ReceiptPrintLine {
            kind: "item".to_string(),
            text: format!("{left}{}{amount}", " ".repeat(padding)),
            bold: false,
            align: None,
        });
        if item.discount_amount > 0 {
            lines.push(ReceiptPrintLine {
                kind: "text".to_string(),
                text: format!(
                    "  Discount -{} {:.2}",
                    receipt.currency,
                    minor_to_major(item.discount_amount, &receipt.currency)
                ),
                bold: false,
                align: None,
            });
        }
    }
    lines.push(ReceiptPrintLine {
        kind: "divider".to_string(),
        text: "-".repeat(width),
        bold: false,
        align: None,
    });
    let money_line = |label: &str, amount: i64, bold: bool| ReceiptPrintLine {
        kind: if bold { "total".to_string() } else { "text".to_string() },
        text: {
            let value = format!(
                "{} {:.2}",
                receipt.currency,
                minor_to_major(amount, &receipt.currency)
            );
            let padding = width.saturating_sub(label.len() + value.len());
            format!("{label}{}{value}", " ".repeat(padding))
        },
        bold,
        align: None,
    };
    lines.push(money_line("Subtotal", receipt.subtotal, false));
    if receipt.discount_total > 0 {
        lines.push(money_line("Discount", -receipt.discount_total, false));
    }
    for line in &receipt.tax_breakdown {
        lines.push(money_line(
            &format!("Tax {:.2}%", line.tax_rate * 100.0),
            line.tax_amount,
            false,
        ));
    }
    lines.push(money_line("TOTAL", receipt.total, true));
    for payment in &receipt.payments {
        lines.push(ReceiptPrintLine {
            kind: "payment".to_string(),
            text: {
                let label = payment.method_code.clone();
                let value = format!(
                    "{} {:.2}",
                    receipt.currency,
                    minor_to_major(payment.amount, &receipt.currency)
                );
                let padding = width.saturating_sub(label.len() + value.len());
                format!("{label}{}{value}", " ".repeat(padding))
            },
            bold: false,
            align: None,
        });
    }
    if receipt.change_due > 0 {
        lines.push(money_line("Change", receipt.change_due, false));
    }
    lines.push(ReceiptPrintLine {
        kind: "divider".to_string(),
        text: "-".repeat(width),
        bold: false,
        align: None,
    });
    if let Some(footer) = &receipt.footer {
        lines.push(ReceiptPrintLine {
            kind: "footer".to_string(),
            text: footer.clone(),
            bold: false,
            align: Some("center".to_string()),
        });
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::pos::{PosSale, PosSaleItem, PosSettings};

    fn sale_item(price: i64, quantity: u32, tax_rate: f64) -> PosSaleItem {
        PosSaleItem {
            id: "item-1".to_string(),
            product_id: "prod-1".to_string(),
            name: "Widget".to_string(),
            barcode: None,
            sku: None,
            unit_price: price,
            quantity,
            refunded_quantity: 0,
            discount_type: None,
            discount_value: 0.0,
            discount_amount: 0,
            line_subtotal: 0,
            tax_rate,
            tax_amount: 0,
            line_total: 0,
        }
    }

    fn sale(items: Vec<PosSaleItem>) -> PosSale {
        PosSale {
            id: "sale-1".to_string(),
            number: "POS-000001".to_string(),
            register_id: "reg-1".to_string(),
            shift_id: None,
            cashier_id: "cashier-1".to_string(),
            company_id: None,
            items,
            subtotal: 0,
            discount_type: None,
            discount_value: 0.0,
            discount_total: 0,
            tax_total: 0,
            total: 0,
            currency: "USD".to_string(),
            tax_inclusive: false,
            status: "draft".to_string(),
            amount_paid: 0,
            change_due: 0,
            refunded_amount: 0,
            customer_name: None,
            notes: None,
            completed_at: None,
            voided_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn money_conversions_round_trip() {
        assert_eq!(major_to_minor(10.25, "USD"), 1025);
        assert_eq!(minor_to_major(1025, "USD"), 10.25);
        assert_eq!(major_to_minor(250.0, "JPY"), 250);
        assert_eq!(minor_to_major(250, "JPY"), 250.0);
        assert_eq!(apply_percent_to_minor(1025, 10.0), 103);
        assert_eq!(div_rounded(100, 12), 8);
        assert_eq!(div_rounded(1025, 12), 85);
    }

    #[test]
    fn require_text_trims_and_rejects_empty_or_long() {
        assert_eq!(require_text("  Widget ", "name", 10).unwrap(), "Widget");
        assert!(require_text("", "name", 10).is_err());
        assert!(require_text("   ", "name", 10).is_err());
        assert!(require_text("toolongname", "name", 5).is_err());
    }

    #[test]
    fn optional_text_treats_blank_as_none() {
        assert_eq!(optional_text(&None, "notes", 10).unwrap(), None);
        assert_eq!(
            optional_text(&Some("   ".to_string()), "notes", 10).unwrap(),
            None
        );
        assert_eq!(
            optional_text(&Some(" hi ".to_string()), "notes", 10).unwrap(),
            Some("hi".to_string())
        );
        assert!(optional_text(&Some("toolong".to_string()), "notes", 3).is_err());
    }

    #[test]
    fn valid_money_accepts_minor_units_and_rejects_negatives() {
        assert_eq!(valid_money(1001, "amount").unwrap(), 1001);
        assert_eq!(valid_money(0, "amount").unwrap(), 0);
        assert!(valid_money(-1, "amount").is_err());
    }

    #[test]
    fn valid_currency_uppercases_and_validates() {
        assert_eq!(valid_currency("usd").unwrap(), "USD");
        assert_eq!(valid_currency(" EUR ").unwrap(), "EUR");
        assert!(valid_currency("US").is_err());
        assert!(valid_currency("USDD").is_err());
    }

    #[test]
    fn valid_discount_type_normalizes() {
        assert_eq!(valid_discount_type(&None).unwrap(), None);
        assert_eq!(valid_discount_type(&Some("".to_string())).unwrap(), None);
        assert_eq!(
            valid_discount_type(&Some("PERCENT".to_string())).unwrap(),
            Some("percent".to_string())
        );
        assert_eq!(
            valid_discount_type(&Some("fixed".to_string())).unwrap(),
            Some("fixed".to_string())
        );
        assert!(valid_discount_type(&Some("bogus".to_string())).is_err());
    }

    #[test]
    fn discount_amount_caps_percent_and_fixed() {
        let percent = Some("percent".to_string());
        assert_eq!(
            discount_amount(&percent, 10.0, 20000, 100.0, "USD").unwrap(),
            2000
        );
        assert!(discount_amount(&percent, 101.0, 20000, 100.0, "USD").is_err());
        let fixed = Some("fixed".to_string());
        assert_eq!(
            discount_amount(&fixed, 30.0, 20000, 100.0, "USD").unwrap(),
            3000
        );
        assert_eq!(
            discount_amount(&fixed, 500.0, 20000, 100.0, "USD").unwrap(),
            20000
        );
        assert_eq!(
            discount_amount(&None, 50.0, 20000, 100.0, "USD").unwrap(),
            0
        );
    }

    #[test]
    fn tax_on_amount_handles_exclusive_and_inclusive() {
        assert_eq!(tax_on_amount(10000, 0.10, false), (10000, 1000));
        assert_eq!(tax_on_amount(11000, 0.10, true), (10000, 1000));
        assert_eq!(tax_on_amount(5000, 0.0, false), (5000, 0));
    }

    #[test]
    fn recompute_sale_totals_exclusive_tax() {
        let mut sale = sale(vec![sale_item(10000, 2, 0.10)]);
        recompute_sale(&mut sale, &PosSettings::default()).unwrap();
        assert_eq!(sale.subtotal, 20000);
        assert_eq!(sale.tax_total, 2000);
        assert_eq!(sale.total, 22000);
        assert_eq!(sale.items[0].line_total, 22000);
    }

    #[test]
    fn recompute_sale_spreads_sale_discount_across_lines() {
        let mut sale = sale(vec![sale_item(10000, 2, 0.10)]);
        sale.discount_type = Some("percent".to_string());
        sale.discount_value = 10.0;
        recompute_sale(&mut sale, &PosSettings::default()).unwrap();
        assert_eq!(sale.discount_total, 2000);
        assert_eq!(sale.subtotal, 18000);
        assert_eq!(sale.tax_total, 1800);
        assert_eq!(sale.total, 19800);
    }

    #[test]
    fn recompute_sale_rejects_discount_over_cap_instead_of_swallowing_it() {
        // Regression: a 150% sale discount used to be swallowed by
        // `unwrap_or(0.0)` - the discount silently became 0 while
        // the stored discount_value stayed 150. It must error so
        // create/update reject the sale instead of storing it.
        let mut sale = sale(vec![sale_item(10000, 2, 0.10)]);
        sale.discount_type = Some("percent".to_string());
        sale.discount_value = 150.0;
        let result = recompute_sale(&mut sale, &PosSettings::default());
        assert!(result.is_err());
        assert_eq!(sale.discount_total, 0);
        assert_eq!(sale.total, 0);
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

    fn test_sale(company_id: Option<&str>) -> PosSale {
        PosSale {
            id: "sale-1".to_string(),
            number: "POS-000001".to_string(),
            register_id: "reg-1".to_string(),
            shift_id: None,
            cashier_id: "cashier-1".to_string(),
            company_id: company_id.map(str::to_string),
            items: Vec::new(),
            subtotal: 0,
            discount_type: None,
            discount_value: 0.0,
            discount_total: 0,
            tax_total: 0,
            total: 0,
            currency: "USD".to_string(),
            tax_inclusive: false,
            status: "draft".to_string(),
            amount_paid: 0,
            change_due: 0,
            refunded_amount: 0,
            customer_name: None,
            notes: None,
            completed_at: None,
            voided_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
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
    fn in_company_scope_shows_global_records_to_scoped_callers() {
        let scope = Some("acme".to_string());
        assert!(in_company_scope(&Some("acme".to_string()), &scope));
        assert!(!in_company_scope(&Some("other".to_string()), &scope));
        assert!(in_company_scope(&None, &scope));
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some("other".to_string()), &None));
    }

    #[test]
    fn require_company_match_for_refuses_writes_from_unresolved_callers() {
        let body = Some("c1".to_string());
        assert!(require_company_match_for(&None, &body).is_err());
        assert!(require_company_match_for(&None, &None).is_err());
        assert_eq!(
            require_company_match_for(&Some("c1".to_string()), &body).unwrap(),
            Some("c1".to_string())
        );
        assert!(
            require_company_match_for(&Some("c1".to_string()), &Some("c2".to_string())).is_err()
        );
    }

    #[test]
    fn require_sale_in_scope_partitions_by_company() {
        let state = PosState::default();
        state
            .sales
            .insert("sale-1".to_string(), test_sale(Some("acme")));
        let scope = Some("acme".to_string());
        assert!(require_sale_in_scope(&state, "sale-1", &scope).is_ok());
        assert!(require_sale_in_scope(&state, "sale-1", &Some("other".to_string())).is_err());
        assert!(require_sale_in_scope(&state, "sale-1", &None).is_err());
        assert!(require_sale_in_scope(&state, "missing", &scope).is_err());
        state
            .sales
            .insert("sale-1".to_string(), test_sale(None));
        assert!(require_sale_in_scope(&state, "sale-1", &None).is_ok());
        assert!(require_sale_in_scope(&state, "sale-1", &scope).is_err());
    }
}
