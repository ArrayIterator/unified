// affiliates module — Helpers.
//
// Split from the former single-file module `src/modules/affiliates.rs`.
// The module root (`affiliates.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hardening::{SecuritySettings, trusted_client_ip_for_http_request};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{CODE_RANDOM_LEN, DUPLICATE_IP_THRESHOLD, DUPLICATE_IP_WINDOW_MINUTES, MAX_CODE_LEN, MAX_EMAIL_LEN, MAX_MONEY_MINOR, PERM_AFFILIATES_MANAGE, VELOCITY_THRESHOLD, VELOCITY_WINDOW_MINUTES};
use super::types::{Affiliate, AffiliateCommission, AffiliateConversion, AffiliateLink, AffiliatePayout, AffiliatesState, CommissionStatus, CommissionType};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources (header trust
/// removal is task 317's job elsewhere).
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

/// Money: a minor-unit amount, non-negative and below
/// `MAX_MONEY_MINOR` (the 1e15-major-unit NUMERIC(19,4) bound,
/// expressed in minor units). The request JSON already converted
/// major units to minor units through `serde_major`, so this only
/// validates the range.
pub(crate) fn valid_price(price: i64) -> UnifiedResult<i64> {
    if price < 0 || price >= MAX_MONEY_MINOR {
        return Err(UnifiedError::bad_request(
            "price must be >= 0 and below 1000000000000000",
        ));
    }
    Ok(price)
}

/// Round a percent-or-fixed `commission_value` to 4 decimal places.
/// This is NOT a money conversion: `commission_value` stays `f64`
/// because it is a percent (0..=100) for percent commissions and a
/// major-unit amount for fixed ones.
pub(crate) fn round_money(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// True for zero-decimal currencies (no minor units), matching the
/// billing and store modules' list.
pub(crate) fn is_zero_decimal_currency(currency: &str) -> bool {
    unified_core::db::money::is_zero_decimal_currency(currency)
}

/// Convert a major-unit amount (a store order total, a fixed
/// commission value) to integer minor units. Rounds half away from
/// zero, matching `serde_major`.
pub(crate) fn major_to_minor(amount_major: f64, currency: &str) -> i64 {
    if is_zero_decimal_currency(currency) {
        amount_major.round() as i64
    } else {
        (amount_major * 100.0).round() as i64
    }
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

/// Apply a percentage (0..=100) to a minor-unit amount, rounding
/// half away from zero to minor units. The rate stays `f64` by
/// design; the result is rounded exactly once to minor units.
pub(crate) fn apply_percent_to_minor(amount_minor: i64, percent: f64) -> i64 {
    (amount_minor as f64 * percent / 100.0).round() as i64
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

pub(crate) fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || email.contains(' ') {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// A referral code is lowercase letters, digits, and single hyphens
/// only, never leading or trailing with a hyphen, at most 64 chars.
pub(crate) fn valid_code(code: &str) -> bool {
    if code.is_empty() || code.len() > MAX_CODE_LEN {
        return false;
    }
    if code.starts_with('-') || code.ends_with('-') || code.contains("--") {
        return false;
    }
    code.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub(crate) fn slugify(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in value.to_lowercase().chars() {
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
        out.push_str("ref");
    }
    out.truncate(MAX_CODE_LEN - CODE_RANDOM_LEN - 1);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Auto-generate a referral code from a display name plus a random
/// suffix. Retried by the caller while the code is already taken.
pub(crate) fn generate_code(base: &str) -> String {
    let slug = slugify(base);
    let random: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(CODE_RANDOM_LEN)
        .map(char::from)
        .collect::<String>()
        .to_lowercase();
    format!("{slug}-{random}")
}

/// One-way SHA-256 hex digest of a visitor IP. The raw IP is never
/// stored or logged anywhere in this module.
pub(crate) fn hash_ip(ip: &str) -> String {
    let digest = Sha256::digest(ip.trim().as_bytes());
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Visitor IP for click hashing: the trusted client IP — the
/// first `X-Forwarded-For` entry only when the TCP peer is a
/// configured trusted proxy (`security.trusted_proxies`, see
/// `unified_core::hardening::trusted_client_ip`), otherwise the
/// peer address — else the body-supplied fallback, else "unknown".
/// A direct client can never spoof the hashed IP by sending its
/// own `X-Forwarded-For` header. The value is hashed immediately
/// by the caller; it is never stored raw.
pub(crate) fn visitor_ip(
    req: &HttpRequest,
    fallback: Option<&str>,
    settings: &SecuritySettings,
) -> String {
    if let Some(ip) = trusted_client_ip_for_http_request(req, settings) {
        return ip.to_string();
    }
    match fallback {
        Some(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => "unknown".to_string(),
    }
}

pub(crate) fn validate_commission(
    commission_type: CommissionType,
    commission_value: f64,
) -> UnifiedResult<f64> {
    if !commission_value.is_finite() || commission_value < 0.0 {
        return Err(UnifiedError::bad_request(
            "commission_value must be a finite number >= 0",
        ));
    }
    match commission_type {
        CommissionType::Percent => {
            if commission_value > 100.0 {
                return Err(UnifiedError::bad_request(
                    "commission_value for a percent commission must be between 0 and 100",
                ));
            }
        }
        CommissionType::Fixed => {
            if commission_value > 1_000_000.0 {
                return Err(UnifiedError::bad_request(
                    "commission_value for a fixed commission must be at most 1000000",
                ));
            }
        }
    }
    Ok(round_money(commission_value))
}

pub(crate) fn load_affiliate(state: &AffiliatesState, raw_id: &str) -> UnifiedResult<Affiliate> {
    let id = parse_uuid(raw_id, "affiliate")?;
    state
        .affiliates
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Affiliate not found: {id}")))
}

pub(crate) fn load_affiliate_by_user(state: &AffiliatesState, user_id: &str) -> UnifiedResult<Affiliate> {
    state
        .affiliates
        .iter()
        .map(|entry| entry.value().clone())
        .find(|affiliate| affiliate.user_id == user_id)
        .ok_or_else(|| UnifiedError::not_found("No affiliate record for the caller"))
}

pub(crate) fn load_link(state: &AffiliatesState, raw_id: &str) -> UnifiedResult<AffiliateLink> {
    let id = parse_uuid(raw_id, "affiliate link")?;
    state
        .links
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Affiliate link not found: {id}")))
}

pub(crate) fn load_link_by_code(state: &AffiliatesState, code: &str) -> UnifiedResult<AffiliateLink> {
    state
        .links
        .iter()
        .map(|entry| entry.value().clone())
        .find(|link| link.code == code)
        .ok_or_else(|| UnifiedError::not_found(format!("Referral code not found: {code}")))
}

pub(crate) fn load_commission(state: &AffiliatesState, raw_id: &str) -> UnifiedResult<AffiliateCommission> {
    let id = parse_uuid(raw_id, "commission")?;
    state
        .commissions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Commission not found: {id}")))
}

pub(crate) fn load_payout(state: &AffiliatesState, raw_id: &str) -> UnifiedResult<AffiliatePayout> {
    let id = parse_uuid(raw_id, "payout")?;
    state
        .payouts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Payout not found: {id}")))
}

/// Load an affiliate and verify it is in the caller scope:
/// company callers read their own and global affiliates only,
/// unresolved callers read global affiliates only. A missing
/// affiliate answers `not_found`; an out-of-scope affiliate
/// answers `forbidden`. Links, clicks, conversions, commissions,
/// commission rules, and payouts carry no company of their own;
/// they are scoped through their parent affiliate with this
/// loader and the two below.
pub(crate) fn require_affiliate_in_scope(
    state: &AffiliatesState,
    raw_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<Affiliate> {
    let affiliate = load_affiliate(state, raw_id)?;
    require_record_in_company_scope(&affiliate.company_id, scope)?;
    Ok(affiliate)
}

/// Load a commission and verify its parent affiliate is in the
/// caller scope.
pub(crate) fn require_affiliate_commission_in_scope(
    state: &AffiliatesState,
    raw_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<AffiliateCommission> {
    let commission = load_commission(state, raw_id)?;
    require_affiliate_in_scope(state, &commission.affiliate_id.to_string(), scope)?;
    Ok(commission)
}

/// Load a payout and verify its parent affiliate is in the
/// caller scope.
pub(crate) fn require_affiliate_payout_in_scope(
    state: &AffiliatesState,
    raw_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<AffiliatePayout> {
    let payout = load_payout(state, raw_id)?;
    require_affiliate_in_scope(state, &payout.affiliate_id.to_string(), scope)?;
    Ok(payout)
}

pub(crate) fn code_in_use(state: &AffiliatesState, code: &str, except: Option<Uuid>) -> bool {
    state
        .links
        .iter()
        .any(|entry| entry.value().code == code && Some(entry.value().id) != except)
}

pub(crate) fn is_manager(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_AFFILIATES_MANAGE)
}

/// Affiliate-scoped access: a manager may see any affiliate; any
/// other caller may only see the affiliate whose `user_id` is
/// their own. Enforced server-side on every affiliate-scoped read.
pub(crate) fn require_affiliate_access(
    ctx: &AppContext,
    caller: &Caller,
    affiliate: &Affiliate,
) -> UnifiedResult<()> {
    if is_manager(ctx, &caller.role) || affiliate.user_id == caller.actor {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "An affiliate may only access their own records",
        ))
    }
}

/// Resolve the commission rule for a purchase: the per-product rule
/// wins when one exists, otherwise the affiliate global default.
/// The returned amount is in minor units; the returned
/// `rate_or_amount` keeps the dual units of `commission_value`
/// (percent, or a fixed amount in major units).
pub(crate) fn commission_for(
    state: &AffiliatesState,
    affiliate: &Affiliate,
    product_id: Option<&str>,
    order_total: i64,
) -> (CommissionType, f64, i64) {
    if let Some(product_id) = product_id {
        if let Some(rule) = state
            .commission_rules
            .iter()
            .map(|entry| entry.value().clone())
            .find(|rule| rule.affiliate_id == affiliate.id && rule.product_id == product_id)
        {
            let amount = match rule.commission_type {
                CommissionType::Percent => {
                    apply_percent_to_minor(order_total, rule.commission_value)
                }
                CommissionType::Fixed => {
                    major_to_minor(rule.commission_value, &affiliate.currency)
                }
            };
            return (rule.commission_type, rule.commission_value, amount);
        }
    }
    let amount = match affiliate.commission_type {
        CommissionType::Percent => {
            apply_percent_to_minor(order_total, affiliate.commission_value)
        }
        CommissionType::Fixed => major_to_minor(affiliate.commission_value, &affiliate.currency),
    };
    (affiliate.commission_type, affiliate.commission_value, amount)
}

/// Fraud checks applied to a click. They only add flag reasons;
/// they never block, ban, or void anything. Duplicate IPs: many
/// clicks from one IP hash on the same link inside an hour.
/// Velocity: many clicks from one IP hash across all links inside
/// ten minutes.
pub(crate) fn fraud_flags_for_click(state: &AffiliatesState, link_id: Uuid, ip_hash: &str, now: DateTime<Utc>) -> Vec<String> {
    let mut flags = Vec::new();
    let duplicate_window = now - Duration::minutes(DUPLICATE_IP_WINDOW_MINUTES);
    let same_link_same_ip = state
        .clicks
        .iter()
        .filter(|entry| {
            entry.value().link_id == link_id
                && entry.value().ip_hash == ip_hash
                && entry.value().clicked_at >= duplicate_window
        })
        .count();
    if same_link_same_ip >= DUPLICATE_IP_THRESHOLD {
        flags.push(format!(
            "duplicate_ip: {same_link_same_ip} clicks from the same address on this link in the last {DUPLICATE_IP_WINDOW_MINUTES} minutes"
        ));
    }
    let velocity_window = now - Duration::minutes(VELOCITY_WINDOW_MINUTES);
    let same_ip_any_link = state
        .clicks
        .iter()
        .filter(|entry| entry.value().ip_hash == ip_hash && entry.value().clicked_at >= velocity_window)
        .count();
    if same_ip_any_link >= VELOCITY_THRESHOLD {
        flags.push(format!(
            "click_velocity: {same_ip_any_link} clicks from the same address in the last {VELOCITY_WINDOW_MINUTES} minutes"
        ));
    }
    flags
}

/// Record a conversion and, for purchases, its pending commission.
/// Shared by the manual endpoint and the hook listeners so both
/// paths attribute identically. Fraud flags are recorded on the
/// conversion (and copied to the commission) for human review; they
/// never prevent the conversion from being recorded.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_conversion(
    state: &AffiliatesState,
    affiliate: &Affiliate,
    link_id: Option<Uuid>,
    click_id: Option<Uuid>,
    customer_user_id: &str,
    kind: &str,
    order_id: Option<String>,
    order_total: i64,
    currency: &str,
    product_id: Option<String>,
    now: DateTime<Utc>,
) -> UnifiedResult<(AffiliateConversion, Option<AffiliateCommission>)> {
    let mut flag_reasons: Vec<String> = Vec::new();
    // Self-referral: the affiliate buying (or signing up) through
    // their own link. Flagged for review only — the conversion is
    // still recorded and the commission stays pending for a human.
    if customer_user_id == affiliate.user_id {
        flag_reasons.push("self_referral: the customer is the affiliate themselves".to_string());
    }
    // Attribution window: the click that produced this conversion
    // must fall inside the affiliate's window (last-click wins).
    // A supplied click id with no matching click row is an error,
    // not a silent skip: recording the conversion without the
    // window check would attribute it to a click that never
    // happened (or that belongs to another store).
    if let Some(click_id) = click_id {
        let click = state
            .clicks
            .get(&click_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Click not found: {click_id}")))?;
        let window_start = now - Duration::days(affiliate.attribution_window_days);
        if click.clicked_at < window_start {
            return Err(UnifiedError::bad_request(format!(
                "Click is outside the {}-day attribution window",
                affiliate.attribution_window_days
            )));
        }
        if let Some(mut entry) = state.clicks.get_mut(&click_id) {
            entry.converted = true;
        }
    }
    let flagged = !flag_reasons.is_empty();
    let conversion = AffiliateConversion {
        id: Uuid::new_v4(),
        affiliate_id: affiliate.id,
        link_id,
        click_id,
        customer_user_id: customer_user_id.to_string(),
        kind: kind.to_string(),
        order_id: order_id.clone(),
        order_total,
        currency: currency.to_string(),
        product_id: product_id.clone(),
        flagged,
        flag_reasons: flag_reasons.clone(),
        attributed_at: now,
        created_at: now,
    };
    state.conversions.insert(conversion.id, conversion.clone());
    let mut commission = None;
    if kind == "purchase" {
        let (commission_type, rate_or_amount, amount) =
            commission_for(state, affiliate, product_id.as_deref(), order_total);
        let record = AffiliateCommission {
            id: Uuid::new_v4(),
            affiliate_id: affiliate.id,
            conversion_id: conversion.id,
            order_id: order_id.clone(),
            product_id: product_id.clone(),
            commission_type,
            rate_or_amount,
            amount,
            currency: currency.to_string(),
            status: CommissionStatus::Pending,
            payout_id: None,
            flagged,
            fraud_flags: flag_reasons.clone(),
            reviewed_by: None,
            review_reason: None,
            created_at: now,
            updated_at: now,
        };
        state.commissions.insert(record.id, record.clone());
        commission = Some(record);
    }
    state.emit(
        "affiliates.conversion.tracked",
        json!({
            "conversion_id": conversion.id,
            "affiliate_id": affiliate.id,
            "kind": kind,
            "customer_user_id": customer_user_id,
            "order_id": order_id,
            "order_total": minor_to_major(conversion.order_total, currency),
            "currency": currency,
            "flagged": flagged,
            "fraud_flags": flag_reasons,
        }),
    );
    if let Some(record) = &commission {
        state.emit(
            "affiliates.commission.created",
            json!({
                "commission_id": record.id,
                "affiliate_id": affiliate.id,
                "conversion_id": conversion.id,
                "amount": minor_to_major(record.amount, &record.currency),
                "currency": record.currency,
                "status": record.status.as_str(),
                "flagged": record.flagged,
            }),
        );
    }
    Ok((conversion, commission))
}

/// Resolve the referral signal from a hook payload: a referral
/// code, a click id, or an affiliate id. Returns the affiliate and
/// the link / click it resolves to. Nothing is guessed: without a
/// signal the caller gets `None` and skips the event. The signals
/// are tried in order and each one falls through to the next when
/// it does not resolve, so an unresolvable referral code never
/// hides a valid click id or affiliate id carried by the same
/// payload.
pub(crate) fn resolve_referral(
    state: &AffiliatesState,
    ctx: &HookContext,
) -> Option<(Affiliate, Option<Uuid>, Option<Uuid>)> {
    if let Some(code) = ctx.get("referral_code").and_then(|value| value.as_str()) {
        if let Ok(link) = load_link_by_code(state, code) {
            if let Some(affiliate) = state.affiliates.get(&link.affiliate_id).map(|e| e.value().clone()) {
                let click_id = ctx
                    .get("click_id")
                    .and_then(|value| value.as_str())
                    .and_then(|raw| Uuid::parse_str(raw).ok());
                return Some((affiliate, Some(link.id), click_id));
            }
        }
        // Unresolvable code: fall through to the other signals.
    }
    if let Some(code) = ctx.get("affiliate_code").and_then(|value| value.as_str()) {
        if let Ok(link) = load_link_by_code(state, code) {
            if let Some(affiliate) = state.affiliates.get(&link.affiliate_id).map(|e| e.value().clone()) {
                return Some((affiliate, Some(link.id), None));
            }
        }
        // Unresolvable code: fall through to the other signals.
    }
    if let Some(raw) = ctx.get("click_id").and_then(|value| value.as_str()) {
        if let Ok(click_id) = Uuid::parse_str(raw) {
            if let Some(click) = state.clicks.get(&click_id).map(|e| e.value().clone()) {
                if let Some(affiliate) = state.affiliates.get(&click.affiliate_id).map(|e| e.value().clone()) {
                    return Some((affiliate, Some(click.link_id), Some(click.id)));
                }
            }
        }
        // Unresolvable click id: fall through to the affiliate id.
    }
    if let Some(raw) = ctx.get("affiliate_id").and_then(|value| value.as_str()) {
        if let Ok(affiliate_id) = Uuid::parse_str(raw) {
            if let Some(affiliate) = state.affiliates.get(&affiliate_id).map(|e| e.value().clone()) {
                return Some((affiliate, None, None));
            }
        }
    }
    None
}

/// Dashboard figures for one affiliate, computed live from the
/// stores (no cached totals that could drift).
pub(crate) fn dashboard_for(state: &AffiliatesState, affiliate_id: Uuid) -> Value {
    let clicks = state
        .clicks
        .iter()
        .filter(|entry| entry.value().affiliate_id == affiliate_id)
        .count() as u64;
    let flagged_clicks = state
        .clicks
        .iter()
        .filter(|entry| entry.value().affiliate_id == affiliate_id && entry.value().flagged)
        .count() as u64;
    let conversions: Vec<AffiliateConversion> = state
        .conversions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|conversion| conversion.affiliate_id == affiliate_id)
        .collect();
    let signups = conversions
        .iter()
        .filter(|conversion| conversion.kind == "signup")
        .count() as u64;
    let purchases = conversions
        .iter()
        .filter(|conversion| conversion.kind == "purchase")
        .count() as u64;
    let flagged_conversions = conversions.iter().filter(|c| c.flagged).count() as u64;
    let commissions: Vec<AffiliateCommission> = state
        .commissions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|commission| commission.affiliate_id == affiliate_id)
        .collect();
    // Dashboard earnings are major units on the wire, as before;
    // the affiliate currency resolves the minor-to-major display
    // conversion (commissions in one dashboard share it).
    let dashboard_currency = state
        .affiliates
        .get(&affiliate_id)
        .map(|entry| entry.value().currency.clone())
        .unwrap_or_else(|| "USD".to_string());
    let sum_by = |status: CommissionStatus| -> f64 {
        minor_to_major(
            commissions
                .iter()
                .filter(|commission| commission.status == status)
                .map(|commission| commission.amount)
                .sum::<i64>(),
            &dashboard_currency,
        )
    };
    let payouts: Vec<AffiliatePayout> = state
        .payouts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|payout| payout.affiliate_id == affiliate_id)
        .collect();
    let mut payouts_sorted = payouts.clone();
    payouts_sorted.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    json!({
        "affiliate_id": affiliate_id,
        "clicks": clicks,
        "flagged_clicks": flagged_clicks,
        "conversions": conversions.len(),
        "signups": signups,
        "purchases": purchases,
        "flagged_conversions": flagged_conversions,
        "earnings": {
            "pending": sum_by(CommissionStatus::Pending),
            "approved": sum_by(CommissionStatus::Approved),
            "paid": sum_by(CommissionStatus::Paid),
            "void": sum_by(CommissionStatus::Void),
        },
        "commissions": commissions.len(),
        "payouts": payouts_sorted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::affiliates::{
        Affiliate, AffiliateClick, AffiliateCommissionRule, AffiliatesState, CommissionType,
    };
    use chrono::Utc;

    fn affiliate(commission_type: CommissionType, commission_value: f64) -> Affiliate {
        Affiliate {
            id: Uuid::new_v4(),
            user_id: "user-1".to_string(),
            display_name: "Acme".to_string(),
            email: "acme@example.com".to_string(),
            status: AffiliateStatus::Approved,
            commission_type,
            commission_value,
            currency: "USD".to_string(),
            attribution_window_days: 30,
            company_id: None,
            payout_method: None,
            payout_reference: None,
            reviewed_by: None,
            review_reason: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn round_money_rounds_to_four_decimals() {
        assert!((round_money(1.23456) - 1.2346).abs() < 1e-9);
        assert!((round_money(2.5) - 2.5).abs() < 1e-9);
    }

    #[test]
    fn valid_price_accepts_minor_units_and_rejects_negatives() {
        assert_eq!(valid_price(1000).unwrap(), 1000);
        assert_eq!(valid_price(0).unwrap(), 0);
        assert!(valid_price(-1).is_err());
    }

    #[test]
    fn valid_price_rejects_amounts_at_or_above_max_money() {
        assert!(valid_price(MAX_MONEY_MINOR).is_err());
        assert!(valid_price(MAX_MONEY_MINOR - 1).is_ok());
        assert!(valid_price(100_000_000).is_ok());
    }

    #[test]
    fn commission_for_computes_percent_and_fixed_in_minor_units() {
        let state = AffiliatesState::default();
        let percent_affiliate = affiliate(CommissionType::Percent, 10.0);
        // 10% of 1025 minor units rounds half away from zero: 103.
        assert_eq!(
            commission_for(&state, &percent_affiliate, None, 1025),
            (CommissionType::Percent, 10.0, 103)
        );
        let fixed_affiliate = affiliate(CommissionType::Fixed, 2.5);
        assert_eq!(
            commission_for(&state, &fixed_affiliate, None, 1025),
            (CommissionType::Fixed, 2.5, 250)
        );
        let jpy_affiliate = Affiliate {
            currency: "JPY".to_string(),
            ..affiliate(CommissionType::Fixed, 250.0)
        };
        assert_eq!(
            commission_for(&state, &jpy_affiliate, None, 1025),
            (CommissionType::Fixed, 250.0, 250)
        );
    }

    #[test]
    fn major_minor_conversions_round_trip() {
        assert_eq!(major_to_minor(10.25, "USD"), 1025);
        assert_eq!(major_to_minor(10.0, "JPY"), 10);
        assert_eq!(minor_to_major(1025, "USD"), 10.25);
        assert_eq!(minor_to_major(1025, "JPY"), 1025.0);
        assert_eq!(apply_percent_to_minor(1025, 10.0), 103);
        assert_eq!(apply_percent_to_minor(1025, 0.0), 0);
    }

    #[test]
    fn valid_currency_normalises_to_uppercase_iso_codes() {
        assert_eq!(valid_currency("usd").unwrap(), "USD");
        assert!(valid_currency("US").is_err());
        assert!(valid_currency("USDD").is_err());
    }

    #[test]
    fn valid_email_checks_shape_only() {
        assert!(valid_email("a@b.co"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a b@c.co"));
        assert!(!valid_email("@b.co"));
        assert!(!valid_email("a@@b.co"));
        assert!(!valid_email(""));
    }

    #[test]
    fn valid_code_accepts_lowercase_hyphenated_codes() {
        assert!(valid_code("acme-1"));
        assert!(!valid_code("Acme"));
        assert!(!valid_code("-acme"));
        assert!(!valid_code("acme-"));
        assert!(!valid_code("ac--me"));
        assert!(!valid_code(""));
        assert!(!valid_code(&"a".repeat(MAX_CODE_LEN + 1)));
    }

    #[test]
    fn slugify_falls_back_to_ref_and_truncates_long_names() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify(""), "ref");
        assert_eq!(slugify("!!!"), "ref");
        assert_eq!(slugify(&"a".repeat(100)).len(), MAX_CODE_LEN - CODE_RANDOM_LEN - 1);
    }

    #[test]
    fn generate_code_is_a_valid_code_with_a_random_suffix() {
        let code = generate_code("Acme Corp");
        assert!(code.starts_with("acme-corp-"));
        assert!(valid_code(&code));
        assert_eq!(code.len(), "acme-corp".len() + 1 + CODE_RANDOM_LEN);
    }

    #[test]
    fn hash_ip_is_a_deterministic_64_char_hex_digest() {
        let first = hash_ip("203.0.113.7");
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(first, hash_ip("203.0.113.7"));
        assert_eq!(first, hash_ip("  203.0.113.7  "));
        assert_ne!(first, hash_ip("203.0.113.8"));
    }

    #[test]
    fn commission_for_uses_the_affiliate_percent_default() {
        let state = AffiliatesState::default();
        let aff = affiliate(CommissionType::Percent, 10.0);
        let (kind, value, amount) = commission_for(&state, &aff, None, 200.0);
        assert_eq!(kind, CommissionType::Percent);
        assert!((value - 10.0).abs() < 1e-9);
        assert!((amount - 20.0).abs() < 1e-9);
    }

    #[test]
    fn commission_for_uses_the_affiliate_fixed_default() {
        let state = AffiliatesState::default();
        let aff = affiliate(CommissionType::Fixed, 7.5);
        let (kind, value, amount) = commission_for(&state, &aff, None, 200.0);
        assert_eq!(kind, CommissionType::Fixed);
        assert!((value - 7.5).abs() < 1e-9);
        assert!((amount - 7.5).abs() < 1e-9);
    }

    #[test]
    fn commission_for_prefers_a_matching_per_product_rule() {
        let state = AffiliatesState::default();
        let aff = affiliate(CommissionType::Percent, 10.0);
        state.commission_rules.insert(
            Uuid::new_v4(),
            AffiliateCommissionRule {
                id: Uuid::new_v4(),
                affiliate_id: aff.id,
                product_id: "prod-1".to_string(),
                commission_type: CommissionType::Fixed,
                commission_value: 5.0,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        );
        let (kind, value, amount) = commission_for(&state, &aff, Some("prod-1"), 200.0);
        assert_eq!(kind, CommissionType::Fixed);
        assert!((value - 5.0).abs() < 1e-9);
        assert!((amount - 5.0).abs() < 1e-9);
    }

    #[test]
    fn fraud_flags_stay_empty_without_repeated_clicks() {
        let state = AffiliatesState::default();
        let flags = fraud_flags_for_click(&state, Uuid::new_v4(), "hash", Utc::now());
        assert!(flags.is_empty());
    }

    #[test]
    fn record_conversion_rejects_an_unknown_click_id() {
        let state = AffiliatesState::default();
        let aff = affiliate(CommissionType::Percent, 10.0);
        let result = record_conversion(
            &state,
            &aff,
            None,
            Some(Uuid::new_v4()),
            "customer-1",
            "signup",
            None,
            0.0,
            "USD",
            None,
            Utc::now(),
        );
        assert!(result.is_err());
        assert!(state.conversions.is_empty());
    }

    #[test]
    fn record_conversion_accepts_a_known_click_inside_the_window() {
        let state = AffiliatesState::default();
        let aff = affiliate(CommissionType::Percent, 10.0);
        let click = AffiliateClick {
            id: Uuid::new_v4(),
            link_id: Uuid::new_v4(),
            affiliate_id: aff.id,
            ip_hash: hash_ip("203.0.113.7"),
            user_agent: None,
            referrer: None,
            flagged: false,
            flag_reasons: Vec::new(),
            converted: false,
            clicked_at: Utc::now(),
        };
        state.clicks.insert(click.id, click.clone());
        let (conversion, _) = record_conversion(
            &state,
            &aff,
            None,
            Some(click.id),
            "customer-1",
            "signup",
            None,
            0.0,
            "USD",
            None,
            Utc::now(),
        )
        .expect("a known click inside the window must record");
        assert_eq!(conversion.click_id, Some(click.id));
        assert!(state.clicks.get(&click.id).expect("click row").converted);
    }

    #[test]
    fn resolve_referral_falls_through_to_click_id_when_the_code_is_unknown() {
        use crate::modules::affiliates::AffiliateLink;
        let state = AffiliatesState::default();
        let aff = affiliate(CommissionType::Percent, 10.0);
        state.affiliates.insert(aff.id, aff.clone());
        let link = AffiliateLink {
            id: Uuid::new_v4(),
            affiliate_id: aff.id,
            code: "acme-code".to_string(),
            campaign: None,
            product_id: None,
            active: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state.links.insert(link.id, link.clone());
        let click = AffiliateClick {
            id: Uuid::new_v4(),
            link_id: link.id,
            affiliate_id: aff.id,
            ip_hash: hash_ip("203.0.113.7"),
            user_agent: None,
            referrer: None,
            flagged: false,
            flag_reasons: Vec::new(),
            converted: false,
            clicked_at: Utc::now(),
        };
        state.clicks.insert(click.id, click.clone());
        // An unresolvable referral code must not hide the click id
        // carried by the same payload.
        let ctx = HookContext::new(
            "store.order.completed",
            json!({
                "referral_code": "no-such-code",
                "click_id": click.id.to_string(),
            }),
        );
        let resolved = resolve_referral(&state, &ctx).expect("click_id signal must resolve");
        assert_eq!(resolved.0.id, aff.id);
        assert_eq!(resolved.1, Some(link.id));
        assert_eq!(resolved.2, Some(click.id));
        // An unresolvable affiliate id falls through the same way.
        let by_affiliate = HookContext::new(
            "store.order.completed",
            json!({
                "referral_code": "no-such-code",
                "affiliate_id": aff.id.to_string(),
            }),
        );
        let resolved = resolve_referral(&state, &by_affiliate).expect("affiliate_id signal must resolve");
        assert_eq!(resolved.0.id, aff.id);
        // No usable signal at all still resolves to nothing.
        let empty = HookContext::new("store.order.completed", json!({}));
        assert!(resolve_referral(&state, &empty).is_none());
        let bad_code_only = HookContext::new(
            "store.order.completed",
            json!({ "referral_code": "no-such-code" }),
        );
        assert!(resolve_referral(&state, &bad_code_only).is_none());
    }

    #[test]
    fn visitor_ip_ignores_x_forwarded_for_from_an_untrusted_peer() {
        use actix_web::test::TestRequest;
        let settings = SecuritySettings::default();
        let req = TestRequest::default()
            .peer_addr("203.0.113.9:1234".parse().unwrap())
            .insert_header(("X-Forwarded-For", "198.51.100.1"))
            .to_http_request();
        assert_eq!(visitor_ip(&req, None, &settings), "203.0.113.9");
    }

    #[test]
    fn visitor_ip_trusts_x_forwarded_for_from_a_configured_proxy() {
        use actix_web::test::TestRequest;
        let mut settings = SecuritySettings::default();
        settings.trusted_proxies = vec!["203.0.113.9".to_string()];
        let req = TestRequest::default()
            .peer_addr("203.0.113.9:1234".parse().unwrap())
            .insert_header(("X-Forwarded-For", "198.51.100.1, 10.0.0.1"))
            .to_http_request();
        assert_eq!(visitor_ip(&req, None, &settings), "198.51.100.1");
    }

    #[test]
    fn visitor_ip_falls_back_when_no_peer_address_is_known() {
        use actix_web::test::TestRequest;
        let settings = SecuritySettings::default();
        let req = TestRequest::default().to_http_request();
        assert_eq!(visitor_ip(&req, Some("192.0.2.5"), &settings), "192.0.2.5");
        assert_eq!(visitor_ip(&req, None, &settings), "unknown");
    }

    #[test]
    fn fraud_flags_mark_ten_same_ip_clicks_on_one_link() {
        let state = AffiliatesState::default();
        let link_id = Uuid::new_v4();
        let ip_hash = hash_ip("203.0.113.7");
        for _ in 0..DUPLICATE_IP_THRESHOLD {
            state.clicks.insert(
                Uuid::new_v4(),
                AffiliateClick {
                    id: Uuid::new_v4(),
                    link_id,
                    affiliate_id: Uuid::new_v4(),
                    ip_hash: ip_hash.clone(),
                    user_agent: None,
                    referrer: None,
                    flagged: false,
                    flag_reasons: Vec::new(),
                    converted: false,
                    clicked_at: Utc::now(),
                },
            );
        }
        let flags = fraud_flags_for_click(&state, link_id, &ip_hash, Utc::now());
        assert!(flags.iter().any(|flag| flag.starts_with("duplicate_ip")));
    }
    #[test]
    fn hash_ip_matches_known_sha256_vector() {
        // The visitor IP is trimmed, then SHA-256 hex-encoded.
        assert_eq!(
            hash_ip("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(hash_ip("  abc  "), hash_ip("abc"));
        assert_eq!(hash_ip("").len(), 64);
    }

}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use super::types::AffiliateStatus;
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

    fn test_affiliate(company_id: Option<&str>) -> Affiliate {
        Affiliate {
            id: Uuid::new_v4(),
            user_id: "user-1".to_string(),
            display_name: "Acme".to_string(),
            email: "acme@example.com".to_string(),
            status: AffiliateStatus::Pending,
            commission_type: CommissionType::Percent,
            commission_value: 10.0,
            currency: "USD".to_string(),
            attribution_window_days: 30,
            company_id: company_id.map(str::to_string),
            payout_method: None,
            payout_reference: None,
            reviewed_by: None,
            review_reason: None,
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
        assert!(
            require_company_match_for(&Some("c1".to_string()), &Some("c2".to_string())).is_err()
        );
    }

    #[test]
    fn require_affiliate_in_scope_partitions_by_company() {
        let state = AffiliatesState::default();
        let affiliate = test_affiliate(Some("acme"));
        let id = affiliate.id.to_string();
        state.affiliates.insert(affiliate.id, affiliate);
        let scope = Some("acme".to_string());
        assert!(require_affiliate_in_scope(&state, &id, &scope).is_ok());
        assert!(require_affiliate_in_scope(&state, &id, &Some("other".to_string())).is_err());
        assert!(require_affiliate_in_scope(&state, &id, &None).is_err());
        assert!(require_affiliate_in_scope(&state, &Uuid::new_v4().to_string(), &scope).is_err());
        let global = test_affiliate(None);
        let global_id = global.id.to_string();
        state.affiliates.insert(global.id, global);
        assert!(require_affiliate_in_scope(&state, &global_id, &None).is_ok());
        assert!(require_affiliate_in_scope(&state, &global_id, &scope).is_err());
    }
}
